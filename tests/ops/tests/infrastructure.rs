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
