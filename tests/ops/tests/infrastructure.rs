use jig_ops_domain_infrastructure as infra;
use jig_ops_fake_production as production;
use jig_ops_world::{InfrastructureWorld, infra_environment, infra_service};
use skein_lib::Token;

fn key() -> infra::Key {
    infra::Key { deployment: 1, task: 7, purpose: 3 }
}

fn make(world: &mut InfrastructureWorld, effect: infra::Effect) -> Vec<infra::Request> {
    let described = world.event(infra::Event::Describe { token: Token::new(1), effect });
    assert!(matches!(described[0], infra::Request::Described { .. }));
    let kept = world.event(infra::Event::Keep { token: Token::new(1), key: key() });
    world.release(&kept)
}

#[test]
fn an_uncertain_keyed_restart_is_found_once() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    world.production.queue_fault(production::Fault::LostAnswer);
    let requests = make(&mut world, infra::Effect::Restart { service: infra_service(), operation: 88 });
    assert!(requests.contains(&infra::Request::Outcome { key: key(), outcome: infra::Outcome::Made }));
    assert_eq!(world.production.observed().len(), 1);
    world.restart();
    assert_eq!(world.production.observed().len(), 1);
}

#[test]
fn an_uncertain_restart_without_operation_ids_is_held() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::NoOperationIds);
    world.production.queue_fault(production::Fault::LostAnswer);
    let requests = make(&mut world, infra::Effect::Restart { service: infra_service(), operation: 88 });
    assert!(requests.contains(&infra::Request::Outcome { key: key(), outcome: infra::Outcome::Uncertain }));
    world.restart();
    assert_eq!(world.production.observed().len(), 1);
}

#[test]
fn creation_survives_a_lost_answer_and_pool_shrink_retains_its_holder() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let pool = infra::Pool(Box::from(*b"staging"));
    let first = world.event(infra::Event::System(infra::SystemEvent::Pool { pool: pool.clone(), quota: 2, used: 0 }));
    assert!(first.contains(&infra::Request::Slots { pool: pool.clone(), quota: 2, used: 0 }));
    world.production.queue_fault(production::Fault::LostAnswer);
    let requests =
        make(&mut world, infra::Effect::CreateEnvironment { environment: infra_environment(), until: 100, price: 40 });
    assert!(requests.contains(&infra::Request::Outcome { key: key(), outcome: infra::Outcome::Made }));
    assert_eq!(world.production.used_slots("staging"), 1);
    world.restart();
    assert_eq!(world.production.used_slots("staging"), 1);
    world.production.set_pool_quota("staging", 0);
    let answer = world.production.read_pool("staging").expect("staging pool exists");
    let changed = world.event(infra::Event::System(infra::SystemEvent::Pool {
        pool: pool.clone(),
        quota: answer.0,
        used: answer.1,
    }));
    assert!(changed.contains(&infra::Request::Slots { pool, quota: 0, used: 1 }));
}

#[test]
fn a_hand_change_reports_drift_to_a_reliant_task() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let service = infra_service();
    world.event(infra::Event::Names { task: 7, resources: Box::from([infra::Resource::Service(service.clone())]) });
    let initial = world.event(infra::Event::System(infra::SystemEvent::Service {
        service: service.clone(),
        fact: infra::ServiceFact { version: Box::from(*b"v2"), replicas: 3, healthy: true, revision: 1, observed: 0 },
        other_hand: false,
    }));
    assert!(initial.contains(&infra::Request::Changed { resource: infra::Resource::Service(service.clone()) }));
    world.production.other_hand(production::Fault::HandScale {
        service: production::ServiceName::new("production", "checkout"),
        replicas: 4,
    });
    let after = world.release(&[infra::Request::System(infra::SystemRequest::Service { service: service.clone() })]);
    assert!(after.contains(&infra::Request::Drift { task: 7, resource: infra::Resource::Service(service) }));
}

#[test]
fn provision_waits_for_readiness_and_tears_down_on_release() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let environment = infra_environment();
    world.event(infra::Event::StartProcedure {
        task: 7,
        procedure: infra::Procedure::Provision {
            environment: environment.clone(),
            until: 100,
            price: 40,
            deadline: 90,
        },
    });
    let first = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    world.release(&first);
    let second = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(second.contains(&infra::Request::Step {
        task: 7,
        decision: infra::StepDecision::Effect(infra::Effect::CreateEnvironment {
            environment: environment.clone(),
            until: 100,
            price: 40,
        }),
    }));
    let created =
        make(&mut world, infra::Effect::CreateEnvironment { environment: environment.clone(), until: 100, price: 40 });
    assert!(created.contains(&infra::Request::Outcome { key: key(), outcome: infra::Outcome::Made }));
    world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::EffectMade });
    world.production.advance(5);
    world.now = 5;
    let waiting = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    let refreshed = world.release(&waiting);
    assert!(
        refreshed.contains(&infra::Request::Changed { resource: infra::Resource::Environment(environment.clone()) })
    );
    let finished = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(finished.contains(&infra::Request::Step { task: 7, decision: infra::StepDecision::Finish }));
    let released = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Release });
    assert!(released.contains(&infra::Request::Step {
        task: 7,
        decision: infra::StepDecision::Effect(infra::Effect::TearDown {
            environment: environment.clone(),
            created_by: key(),
        }),
    }));
    let deleted = make(&mut world, infra::Effect::TearDown { environment, created_by: key() });
    assert!(deleted.contains(&infra::Request::Outcome { key: key(), outcome: infra::Outcome::Made }));
    assert_eq!(world.production.used_slots("staging"), 0);
}

#[test]
fn a_proposed_restart_survives_restart_and_is_made_after_acceptance() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let token = skein_lib::Token::new(1);
    let effect = infra::Effect::Restart { service: infra_service(), operation: 17 };
    world.event(infra::Event::Describe { token, effect: effect.clone() });
    let proposed = world.event(infra::Event::KeepProposal { token, number: 9, task: 7 });
    assert_eq!(
        proposed,
        vec![infra::Request::Save { record: infra::Record::Proposal { number: 9, task: 7, effect: effect.clone() } }]
    );
    world.release(&proposed);
    world.restart();
    assert!(world.production.observed().is_empty(), "a proposal grants no permission to make it");
    let accepted = world.event(infra::Event::DescribeProposal { token, number: 9 });
    assert!(accepted.iter().any(|request| matches!(request, infra::Request::Described { .. })));
    let kept = world.event(infra::Event::Keep { token, key: key() });
    world.event(infra::Event::DropProposal { number: 9 });
    assert!(world.production.observed().is_empty(), "the outbox waits for its commit's release");
    world.production.queue_fault(production::Fault::LostAnswer);
    world.release(&kept);
    world.restart();
    assert_eq!(
        world
            .production
            .observed()
            .iter()
            .filter(|row| matches!(row, production::ObservedEffect::Restart { applied: true, .. }))
            .count(),
        1
    );
    assert_eq!(
        world.event(infra::Event::DescribeProposal { token, number: 9 }),
        vec![infra::Request::Refused { token }],
        "the terminal proposal payload was erased"
    );
}

#[test]
fn scale_reads_replicas_afresh_and_finishes_at_the_requested_count() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let service = infra::Service::new(Box::from(*b"staging"), Box::from(*b"checkout"));
    world.production.add_service(production::ServiceName::new("staging", "checkout"), "v2", 3);
    world.release(&[infra::Request::System(infra::SystemRequest::Service { service: service.clone() })]);
    world.production.other_hand(production::Fault::HandScale {
        service: production::ServiceName::new("staging", "checkout"),
        replicas: 4,
    });
    world.event(infra::Event::StartProcedure {
        task: 7,
        procedure: infra::Procedure::Scale { service: service.clone(), replicas: 1, deadline: 100 },
    });
    let read = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(read.contains(&infra::Request::System(infra::SystemRequest::Service { service: service.clone() })));
    assert!(
        !read.iter().any(|row| matches!(row, infra::Request::Step { decision: infra::StepDecision::Effect(..), .. }))
    );
    world.release(&read);
    let decided = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    let effect = infra::Effect::Scale { service: service.clone(), from: 4, to: 1 };
    assert!(decided.contains(&infra::Request::Step { task: 7, decision: infra::StepDecision::Effect(effect.clone()) }));
    // The scripted parent allows the conditional effect after its observed check.
    make(&mut world, effect);
    let reread = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::EffectMade });
    world.release(&reread);
    let finished = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(finished.contains(&infra::Request::Step { task: 7, decision: infra::StepDecision::Finish }));
    assert_eq!(
        world
            .production
            .observed()
            .iter()
            .filter(|row| matches!(row, production::ObservedEffect::Scale { applied: true, .. }))
            .count(),
        1
    );
}

#[test]
fn scale_waiting_for_authority_rereads_before_asking_again() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let service = infra::Service::new(Box::from(*b"staging"), Box::from(*b"checkout"));
    world.production.add_service(production::ServiceName::new("staging", "checkout"), "v2", 3);
    world.event(infra::Event::StartProcedure {
        task: 7,
        procedure: infra::Procedure::Scale { service: service.clone(), replicas: 1, deadline: 100 },
    });
    let read = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    world.release(&read);
    world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    let waited = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::EffectWaiting });
    world.production.other_hand(production::Fault::HandScale {
        service: production::ServiceName::new("staging", "checkout"),
        replicas: 5,
    });
    world.release(&waited);
    let next = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(next.contains(&infra::Request::Step {
        task: 7,
        decision: infra::StepDecision::Effect(infra::Effect::Scale { service: service.clone(), from: 5, to: 1 })
    }));
    assert!(world.production.observed().is_empty(), "wait commits no outbox entry");
}

#[test]
fn dropping_a_proposal_erases_its_payload_across_restart() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let token = skein_lib::Token::new(1);
    world.event(infra::Event::Describe {
        token,
        effect: infra::Effect::Restart { service: infra_service(), operation: 17 },
    });
    world.event(infra::Event::KeepProposal { token, number: 9, task: 7 });
    assert_eq!(
        world.event(infra::Event::DropProposal { number: 9 }),
        vec![infra::Request::Erase { key: infra::RecordKey::Proposal(9) }]
    );
    world.restart();
    assert_eq!(
        world.event(infra::Event::DescribeProposal { token, number: 9 }),
        vec![infra::Request::Refused { token }]
    );
    assert!(world.production.observed().is_empty());
}
