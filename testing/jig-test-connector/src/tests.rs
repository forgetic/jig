use alloc::boxed::Box;
use skein_lib::{Env, List, Queue, Time, Wall};

use crate::{
    Adoption, Class, Classed, Config, Domain, Event, Hold, HoldMode, Limits, MAX_OUT, Origin, Path, PoolSpec, Record,
    Request, ResourceRole, ResourceSpec, SystemEvent, TopicSpec, step,
};

const LIMITS: Limits = Limits {
    tasks: 2,
    adoptions: 3,
    subscriptions: 3,
    pools: 1,
    resources: 2,
    topics: 1,
    kinds: 0,
    requirements: 1,
    procedures: 1,
    actions_per_procedure: 5,
    facts: 2,
    judges: 2,
    values: 2,
    value_bytes: 18,
    staged: 0,
    entries: 0,
    made: 0,
    resources_per_effect: 0,
    max_attempts: 1,
    write_lifetime: skein_lib::Duration::from_secs(1),
    clock_margin: skein_lib::Duration::from_secs(1),
    resources_per_task: 2,
    subscribers_per_topic: 2,
    lost_per_pool: 2,
    path_segments: 3,
    segment_bytes: 16,
};

fn path(parts: &[&[u8]]) -> Path {
    let mut segments = List::with_capacity(3);
    for part in parts {
        segments.push(Box::from(*part)).expect("fixture path fits");
    }
    Path::new(segments, LIMITS.segment_bytes).expect("fixture path has segments")
}

fn prefix() -> Path {
    path(&[b"unit"])
}

fn service() -> Path {
    path(&[b"unit", b"service"])
}

fn pool() -> Path {
    path(&[b"unit", b"pool"])
}

fn domain() -> Domain {
    Domain::new(
        Config {
            prefix: prefix(),
            resources: Box::from([
                ResourceSpec { path: service(), hold: Hold::Exclusive { mode: HoldMode::Wait }, writable: true },
                ResourceSpec { path: pool(), hold: Hold::Pooled { mode: HoldMode::Refuse }, writable: true },
            ]),
            pools: Box::from([PoolSpec { path: pool(), slots: 2 }]),
            topics: Box::from([TopicSpec { topic: 7 }]),
            deployment: [1; 16],
            kinds: Box::from([]),
            requirements: Box::from([crate::RequirementSpec {
                number: 4,
                guarded: true,
                freshness: skein_lib::Duration::from_nanos(5),
            }]),
            procedures: Box::from([crate::ProcedureSpec {
                number: 3,
                actions: Box::from([
                    crate::ProcedureAction::Effect { kind: 1, purpose: 9, target: 3 },
                    crate::ProcedureAction::Delegate { kinds: Box::from([2]) },
                    crate::ProcedureAction::Propose { number: 7 },
                    crate::ProcedureAction::Wait { state: 3 },
                    crate::ProcedureAction::Finish { result: crate::ProcedureResult::Local { code: 8 } },
                ]),
                max_steps: 5,
                stall: skein_lib::Duration::from_nanos(10),
            }]),
        },
        &LIMITS,
    )
}

fn run(domain: &mut Domain, event: Event) -> Box<[Request]> {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let mut out = Queue::with_capacity(MAX_OUT);
    step(domain, &env, event, &mut out);
    let mut requests = List::with_capacity(MAX_OUT);
    while let Some(request) = out.pop() {
        requests.push(request).expect("the request came from this queue");
    }
    requests.into_boxed()
}

fn run_at(domain: &mut Domain, wall: u64, event: Event) -> Box<[Request]> {
    let env = Env { now: Time::ZERO, wall: Wall::from_nanos(wall), limits: LIMITS };
    let mut out = Queue::with_capacity(MAX_OUT);
    step(domain, &env, event, &mut out);
    let mut requests = List::with_capacity(MAX_OUT);
    while let Some(request) = out.pop() {
        requests.push(request).expect("the request came from this queue");
    }
    requests.into_boxed()
}

fn procedure_event(signal: crate::ProcedureSignal) -> Event {
    Event::Procedure { task: 9, number: 3, resource: service(), signal }
}

#[test]
fn verdict_waits_for_a_fresh_fact_and_reanswers_for_the_exact_state() {
    let mut connector = domain();
    let token = skein_lib::Token::new(5);
    let old = crate::Fact { state: Some(3), observed: Wall::from_nanos(10), pending: false };
    drop(run_at(
        &mut connector,
        10,
        Event::System(SystemEvent::Fact { resource: service(), fact: old, origin: Origin::Own }),
    ));
    assert_eq!(
        run_at(&mut connector, 20, Event::Judge { token, requirement: 4, resources: Box::from([service()]), state: 3 }),
        Box::from([
            Request::Verdict { token, verdict: crate::Verdict::Wait },
            Request::System(crate::SystemRequest::ReadFact { resource: service(), observed: Wall::from_nanos(20) }),
        ])
    );
    let fresh = crate::Fact { state: Some(3), observed: Wall::from_nanos(20), pending: false };
    assert_eq!(
        run_at(
            &mut connector,
            20,
            Event::System(SystemEvent::Fact { resource: service(), fact: fresh, origin: Origin::Own })
        ),
        Box::from([
            Request::Changed { resource: service() },
            Request::Verdict {
                token,
                verdict: crate::Verdict::Met { state: 3, observed: Wall::from_nanos(20), guarded: true },
            },
        ])
    );
    assert_eq!(
        run_at(&mut connector, 20, Event::Judge { token, requirement: 4, resources: Box::from([service()]), state: 4 }),
        Box::from([Request::Verdict { token, verdict: crate::Verdict::Refuse { actual: Some(3) } }])
    );
}

#[test]
fn procedure_commits_each_decision_once_and_waits_on_a_fact_not_a_wake() {
    let mut connector = domain();
    let first = run_at(&mut connector, 10, procedure_event(crate::ProcedureSignal::Activate));
    assert_eq!(first.len(), 2);
    assert_eq!(
        first.get(1),
        Some(&Request::Step {
            task: 9,
            decision: crate::StepDecision::Effect(crate::Effect {
                kind: 1,
                resources: Box::from([service()]),
                purpose: 9,
                condition: None,
                target: 3,
                state: 3,
            }),
        })
    );
    assert!(run_at(&mut connector, 10, procedure_event(crate::ProcedureSignal::Message)).is_empty());
    let delegated = run_at(&mut connector, 11, procedure_event(crate::ProcedureSignal::Settled));
    assert_eq!(
        delegated.get(1),
        Some(&Request::Step { task: 9, decision: crate::StepDecision::Delegate { kinds: Box::from([2]) } })
    );
    let proposed = run_at(&mut connector, 12, procedure_event(crate::ProcedureSignal::DelegateDone));
    assert_eq!(proposed.get(1), Some(&Request::Step { task: 9, decision: crate::StepDecision::Propose { number: 7 } }));
    let waiting = run_at(&mut connector, 13, procedure_event(crate::ProcedureSignal::ProposalDone));
    assert_eq!(
        waiting.get(1),
        Some(&Request::Step { task: 9, decision: crate::StepDecision::Wait { deadline: Wall::from_nanos(23) } })
    );
    assert_eq!(
        run_at(&mut connector, 14, procedure_event(crate::ProcedureSignal::Message)),
        Box::from([Request::Step { task: 9, decision: crate::StepDecision::Wait { deadline: Wall::from_nanos(23) } }])
    );
    let fact = crate::Fact { state: Some(3), observed: Wall::from_nanos(15), pending: false };
    drop(run_at(
        &mut connector,
        15,
        Event::System(SystemEvent::Fact { resource: service(), fact, origin: Origin::Own }),
    ));
    let finished = run_at(&mut connector, 15, procedure_event(crate::ProcedureSignal::Message));
    assert_eq!(finished.len(), 3);
    assert_eq!(
        finished.get(2),
        Some(&Request::Step {
            task: 9,
            decision: crate::StepDecision::Finish { result: crate::ProcedureResult::Local { code: 8 } }
        })
    );
    assert!(run_at(&mut connector, 16, procedure_event(crate::ProcedureSignal::Message)).is_empty());
}

#[test]
fn sections_and_workspace_items_have_one_handover_and_drop_with_their_token() {
    let mut connector = domain();
    drop(run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Owned }));
    drop(run(&mut connector, Event::Names { task: 4, project: 1, resources: Box::from([service()]) }));
    let token = skein_lib::Token::new(1);
    assert_eq!(
        run(&mut connector, Event::Read { token, read: crate::Read { resource: service(), size: 8 } }),
        Box::from([Request::Answer { token, bytes: Box::from(0_u64.to_be_bytes()) }])
    );
    assert_eq!(
        run(&mut connector, Event::Gather { token, task: 4, budget: 18 }),
        Box::from([Request::Ready { token, size: 18 }])
    );
    assert_eq!(run(&mut connector, Event::Cut { token, size: 10 }), Box::from([Request::Ready { token, size: 10 }]));
    assert_eq!(run(&mut connector, Event::HandOver { token }).len(), 1);
    assert!(run(&mut connector, Event::HandOver { token }).is_empty());
    assert_eq!(run(&mut connector, Event::Items { token, task: 4 }), Box::from([Request::Ready { token, size: 1 }]));
    assert_eq!(
        run(&mut connector, Event::HandOver { token }),
        Box::from([Request::Workspace {
            token,
            items: Box::from([crate::Item { resource: service(), writable: true, state: None }]),
        }])
    );
    drop(run(&mut connector, Event::Gather { token, task: 4, budget: 8 }));
    assert!(run(&mut connector, Event::Drop { token }).is_empty());
    assert!(run(&mut connector, Event::HandOver { token }).is_empty());
}

#[test]
fn owned_drift_is_reported_and_restart_refreshes_before_procedures_resume() {
    let mut connector = domain();
    drop(run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Owned }));
    drop(run(&mut connector, Event::Names { task: 4, project: 1, resources: Box::from([service()]) }));
    let old = crate::Fact { state: Some(1), observed: Wall::from_nanos(3), pending: false };
    drop(run_at(
        &mut connector,
        3,
        Event::System(SystemEvent::Fact { resource: service(), fact: old, origin: Origin::Own }),
    ));
    let changed = crate::Fact { state: Some(2), observed: Wall::from_nanos(4), pending: false };
    assert_eq!(
        run_at(
            &mut connector,
            4,
            Event::System(SystemEvent::Fact { resource: service(), fact: changed, origin: Origin::Other })
        ),
        Box::from([
            Request::Changed { resource: service() },
            Request::DriftResource { tasks: Box::from([4]), resource: service() },
        ])
    );
    assert!(run_at(&mut connector, 5, Event::Restart(crate::RestartStep::Records)).is_empty());
    assert_eq!(
        run_at(&mut connector, 5, Event::Restart(crate::RestartStep::FreshRead { resource: service() })),
        Box::from([Request::System(crate::SystemRequest::ReadFact {
            resource: service(),
            observed: Wall::from_nanos(5)
        })])
    );
    assert_eq!(
        run_at(&mut connector, 5, Event::Restart(crate::RestartStep::SettleOutbox)),
        Box::from([Request::RestartDone])
    );
}

#[test]
fn restored_procedure_waits_for_its_outstanding_effect_answer() {
    let mut original = domain();
    let first = run_at(&mut original, 10, procedure_event(crate::ProcedureSignal::Activate));
    let Some(Request::Save { record }) = first.first() else {
        panic!("procedure state was saved before its effect");
    };
    let mut cold = domain();
    assert!(run_at(&mut cold, 11, Event::Restore { record: record.clone() }).is_empty());
    assert!(run_at(&mut cold, 11, procedure_event(crate::ProcedureSignal::Message)).is_empty());
    let resumed = run_at(&mut cold, 12, procedure_event(crate::ProcedureSignal::Settled));
    assert_eq!(
        resumed.get(1),
        Some(&Request::Step { task: 9, decision: crate::StepDecision::Delegate { kinds: Box::from([2]) } })
    );
}

#[test]
fn adoption_and_names_report_roles_and_holds_from_the_connector() {
    let mut connector = domain();
    assert_eq!(
        run(&mut connector, Event::Names { task: 4, project: 1, resources: Box::from([service()]) }),
        Box::from([Request::Unknown { task: 4, resource: service() }])
    );
    let adopted = run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Owned });
    assert_eq!(adopted.len(), 2);
    assert_eq!(adopted[1], Request::Adopted { project: 1, resource: service(), result: Adoption::Added });
    let named = run(&mut connector, Event::Names { task: 4, project: 1, resources: Box::from([service()]) });
    assert_eq!(named.len(), 2);
    assert_eq!(
        named[0],
        Request::Save { record: Record::Task { task: 4, project: 1, resources: Box::from([service()]) } }
    );
    assert_eq!(
        named[1],
        Request::Named {
            task: 4,
            resources: Box::from([crate::Named {
                path: service(),
                role: ResourceRole::Owned,
                hold: Hold::Exclusive { mode: HoldMode::Wait },
            }]),
        }
    );
    assert_eq!(connector.live_tasks(), 1);
    assert_eq!(
        run(&mut connector, Event::Unnamed { task: 4 }),
        Box::from([Request::Erase { key: crate::RecordKey::Task(4) }])
    );
    assert_eq!(connector.live_tasks(), 0);
}

#[test]
fn context_resources_cannot_be_upgraded_to_a_write_role() {
    let mut config = Config {
        prefix: prefix(),
        resources: Box::from([ResourceSpec { path: service(), hold: Hold::None, writable: false }]),
        pools: Box::from([]),
        topics: Box::from([]),
        deployment: [1; 16],
        kinds: Box::from([]),
        requirements: Box::from([]),
        procedures: Box::from([]),
    };
    let mut connector = Domain::new(config.clone(), &LIMITS);
    assert_eq!(
        run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Owned }),
        Box::from([Request::Adopted { project: 1, resource: service(), result: Adoption::Refused }])
    );
    assert_eq!(
        run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Context }).len(),
        2
    );
    config.resources = Box::from([]);
    let mut other = Domain::new(config, &LIMITS);
    assert_eq!(
        run(&mut other, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Context }),
        Box::from([Request::Adopted { project: 1, resource: service(), result: Adoption::Unknown }])
    );
}

#[test]
fn shrinking_a_pool_reports_slots_and_lost_allocations_without_retracting_holds() {
    let mut connector = domain();
    assert_eq!(connector.slots(&pool()), Some(2));
    let output =
        run(&mut connector, Event::System(SystemEvent::Pool { path: pool(), slots: 1, lost: Box::from([9, 10]) }));
    assert_eq!(output.len(), 3);
    assert_eq!(output[0], Request::Save { record: Record::Pool { path: pool(), slots: 1 } });
    assert_eq!(output[1], Request::Slots { pool: pool(), slots: 1 });
    assert_eq!(output[2], Request::Drift { pool: pool(), tasks: Box::from([9, 10]) });
    assert_eq!(connector.slots(&pool()), Some(1));
}

#[test]
fn topic_news_is_classified_per_subscriber_and_own_echoes_are_dropped() {
    let mut connector = domain();
    run(&mut connector, Event::Subscribe { task: 1, topic: 7, wake_at: 5, keep_at: 2 });
    run(&mut connector, Event::Subscribe { task: 2, topic: 7, wake_at: 8, keep_at: 4 });
    assert!(
        run(&mut connector, Event::System(SystemEvent::News { topic: 7, importance: 9, origin: Origin::Own }))
            .is_empty()
    );
    assert_eq!(
        run(&mut connector, Event::System(SystemEvent::News { topic: 7, importance: 6, origin: Origin::Other })),
        Box::from([Request::News {
            topic: 7,
            subscribers: Box::from([Classed { task: 1, class: Class::Wake }, Classed { task: 2, class: Class::Keep }]),
        }])
    );
    assert!(
        run(&mut connector, Event::System(SystemEvent::News { topic: 7, importance: 1, origin: Origin::Other }))
            .is_empty()
    );
    assert_eq!(
        run(&mut connector, Event::Unsubscribe { task: 1, topic: 7 }),
        Box::from([Request::Erase { key: crate::RecordKey::Subscription { topic: 7, task: 1 } }])
    );
}

#[test]
fn restored_records_rebuild_adoptions_names_topics_and_pool_slots() {
    let mut original = domain();
    let mut records = List::with_capacity(4);
    for event in [
        Event::Adopt { project: 1, resource: service(), role: ResourceRole::Participating },
        Event::Names { task: 1, project: 1, resources: Box::from([service()]) },
        Event::Subscribe { task: 1, topic: 7, wake_at: 4, keep_at: 2 },
        Event::System(SystemEvent::Pool { path: pool(), slots: 1, lost: Box::from([]) }),
    ] {
        for request in run(&mut original, event) {
            match request {
                Request::Save { record } => records.push(record).expect("four changes made four records"),
                Request::Named { .. }
                | Request::Described { .. }
                | Request::EffectBusy { .. }
                | Request::EffectRefused { .. }
                | Request::Make { .. }
                | Request::Outcome { .. }
                | Request::System(..)
                | Request::Unknown { .. }
                | Request::Refused { .. }
                | Request::Adopted { .. }
                | Request::Slots { .. }
                | Request::Drift { .. }
                | Request::News { .. }
                | Request::Erase { .. }
                | Request::Verdict { .. }
                | Request::Step { .. }
                | Request::Answer { .. }
                | Request::Ready { .. }
                | Request::Section { .. }
                | Request::Workspace { .. }
                | Request::DriftResource { .. }
                | Request::Changed { .. }
                | Request::RestartDone => {}
            }
        }
    }
    let mut cold = domain();
    for record in &records {
        assert!(run(&mut cold, Event::Restore { record: record.clone() }).is_empty());
    }
    assert_eq!(cold.slots(&pool()), Some(1));
    assert_eq!(cold.live_tasks(), 1);
    assert_eq!(
        run(&mut cold, Event::System(SystemEvent::News { topic: 7, importance: 3, origin: Origin::Other })),
        Box::from([Request::News { topic: 7, subscribers: Box::from([Classed { task: 1, class: Class::Keep }]) }])
    );
}
