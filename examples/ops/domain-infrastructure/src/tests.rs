use alloc::boxed::Box;
use skein_lib::{Env, Queue, Time, Token, Wall};

use super::*;

fn limits() -> Limits {
    Limits {
        services: 4,
        environments: 4,
        pools: 2,
        tasks: 4,
        procedures: 4,
        staged: 4,
        proposals: 4,
        effects: 4,
        made: 4,
        resources_per_task: 4,
        name_bytes: 64,
        max_attempts: 3,
        retry_after_seconds: 30,
    }
}

fn service() -> Service {
    Service::new(Box::from(*b"production"), Box::from(*b"checkout"))
}
fn environment() -> Environment {
    Environment::new(Box::from(*b"staging"), Box::from(*b"payments"))
}
fn key() -> Key {
    Key { deployment: 1, task: 7, purpose: 3 }
}

fn event(domain: &mut Domain, seconds: u64, input: Event) -> Queue<Request> {
    let wall = seconds.checked_mul(1_000_000_000).expect("world time fits");
    let env = Env { now: Time::from_nanos(seconds), wall: Wall::from_nanos(wall), limits: limits() };
    let mut out = Queue::with_capacity(MAX_OUT);
    step(domain, &env, input, &mut out);
    out
}

#[test]
fn effect_descriptions_name_their_recovery_and_price() {
    let mut domain = Domain::new(Backend::OperationIds, &limits());
    let effect = Effect::CreateEnvironment { environment: environment(), until: 100, price: 50 };
    let mut out = event(&mut domain, 1, Event::Describe { token: Token::new(1), effect });
    assert_eq!(
        out.pop(),
        Some(Request::Described {
            token: Token::new(1),
            description: Description {
                kind: 4,
                resources: Box::from([
                    Resource::Environment(environment()),
                    Resource::Pool(Pool(Box::from(*b"staging")))
                ]),
                condition: false,
                form: Form::Creation,
                recovery: Recovery::Keyed,
                price: Some(50),
                state: 100,
            }
        })
    );
    let mut no_ids = Domain::new(Backend::NoOperationIds, &limits());
    let mut restart = event(
        &mut no_ids,
        1,
        Event::Describe { token: Token::new(2), effect: Effect::Restart { service: service(), operation: 9 } },
    );
    assert_eq!(
        restart.pop(),
        Some(Request::Described {
            token: Token::new(2),
            description: Description {
                kind: 1,
                resources: Box::from([Resource::Service(service())]),
                condition: false,
                form: Form::Transition,
                recovery: Recovery::Unrecoverable,
                price: None,
                state: 9,
            }
        })
    );
}

#[test]
fn a_service_is_exclusive_and_a_pool_slot_waits() {
    let mut domain = Domain::new(Backend::OperationIds, &limits());
    let mut named = event(
        &mut domain,
        1,
        Event::Names {
            task: 7,
            resources: Box::from([Resource::Service(service()), Resource::Pool(Pool(Box::from(*b"staging")))]),
        },
    );
    assert_eq!(
        named.pop(),
        Some(Request::Save {
            record: Record::Rely {
                task: 7,
                resources: Box::from([Resource::Service(service()), Resource::Pool(Pool(Box::from(*b"staging"))),])
            }
        })
    );
    assert_eq!(
        named.pop(),
        Some(Request::Named {
            task: 7,
            resources: Box::from([
                Named { resource: Resource::Service(service()), hold: Hold::ExclusiveWait },
                Named { resource: Resource::Pool(Pool(Box::from(*b"staging"))), hold: Hold::PooledWait },
            ])
        })
    );
}

#[test]
fn an_uncertain_restart_without_ids_is_held_and_not_resent() {
    let mut domain = Domain::new(Backend::NoOperationIds, &limits());
    let token = Token::new(1);
    event(&mut domain, 1, Event::Describe { token, effect: Effect::Restart { service: service(), operation: 9 } });
    event(&mut domain, 1, Event::Keep { token, key: key() });
    event(&mut domain, 1, Event::Make { key: key() });
    let mut uncertain = event(
        &mut domain,
        1,
        Event::System(SystemEvent::Applied { key: key(), attempt: 1, result: ApplyResult::Uncertain }),
    );
    assert_eq!(
        uncertain.pop(),
        Some(Request::Save {
            record: Record::Outbox(Entry {
                key: key(),
                effect: Effect::Restart { service: service(), operation: 9 },
                attempt: 1,
                deadline: 31,
                phase: Phase::Held,
            })
        })
    );
    assert_eq!(uncertain.pop(), Some(Request::Outcome { key: key(), outcome: Outcome::Uncertain }));
    assert!(event(&mut domain, 31, Event::Restart).is_empty(), "held restart is not retried");
}

#[test]
fn a_procedure_waits_for_current_facts_and_never_asks_for_two_effects() {
    let mut domain = Domain::new(Backend::OperationIds, &limits());
    let procedure = Procedure::Remediate { service: service(), operation: 9, deadline: 100 };
    event(&mut domain, 1, Event::StartProcedure { task: 7, procedure });
    let mut first = event(&mut domain, 1, Event::Procedure { task: 7, signal: ProcedureSignal::Step });
    assert_eq!(first.pop(), Some(Request::System(SystemRequest::Service { service: service() })));
    let fact = ServiceFact { version: Box::from(*b"v1"), replicas: 2, healthy: false, revision: 1, observed: 1 };
    event(&mut domain, 1, Event::System(SystemEvent::Service { service: service(), fact, other_hand: false }));
    let effect = event(&mut domain, 1, Event::Procedure { task: 7, signal: ProcedureSignal::Step });
    assert_eq!(effect.len(), 2);
    let repeated = event(&mut domain, 1, Event::Procedure { task: 7, signal: ProcedureSignal::Step });
    assert_eq!(repeated.len(), 1);
    let mut second = repeated;
    assert_eq!(second.pop(), Some(Request::Step { task: 7, decision: StepDecision::Wait { until: 100 } }));
}
