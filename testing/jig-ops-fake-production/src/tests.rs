use super::*;

fn key(n: u64) -> Key {
    Key::procedure([1; 16], 2, n)
}

#[test]
fn an_incident_changes_both_streams_and_a_restart_recovers_them() {
    let mut production = Production::new(12, Backend::Fake);
    let name = ServiceName::new("production", "checkout");
    production.add_service(name.clone(), "v2", 3);
    assert!(production.inject_incident(&name));
    assert_eq!(production.read_service(&name).expect("service exists").errors, 20);
    assert_eq!(production.read_alerts(&name, 0, 10, 100).len(), 1);
    assert_eq!(production.read_logs(&name, 0, 10, "incident", 100).len(), 1);
    assert_eq!(production.read_series(&name, 0, 10, 100).last().expect("incident sample").load_percent, 95);
    assert_eq!(production.restart(&name, 77), ResultValue::Made);
    production.advance(5);
    let facts = production.read_service(&name).expect("service exists");
    assert_eq!(facts.errors, 0);
    assert_eq!(facts.healthy_replicas, 3);
    assert!(facts.load_percent < 20);
    assert_eq!(production.read_logs(&name, 0, 10, "recovered", 100).len(), 1);
}

#[test]
fn a_lost_restart_answer_is_found_by_operation_id_and_is_not_repeated() {
    let mut production = Production::new(0, Backend::Fake);
    let name = ServiceName::new("production", "checkout");
    production.add_service(name.clone(), "v2", 2);
    production.queue_fault(Fault::LostAnswer);
    assert_eq!(production.restart(&name, 77), ResultValue::Uncertain);
    assert_eq!(production.find_restart(77), Some(name.clone()));
    assert_eq!(production.restart(&name, 77), ResultValue::Made);
    assert_eq!(
        production
            .observed()
            .iter()
            .filter(|effect| matches!(effect, ObservedEffect::Restart { applied: true, .. }))
            .count(),
        1
    );

    let mut without_ids = Production::new(0, Backend::NoOperationIds);
    without_ids.add_service(name.clone(), "v2", 2);
    without_ids.queue_fault(Fault::LostAnswer);
    assert_eq!(without_ids.restart(&name, 77), ResultValue::Uncertain);
    assert_eq!(without_ids.find_restart(77), None);
}

#[test]
fn a_lost_creation_answer_is_recovered_by_its_key() {
    let mut production = Production::new(0, Backend::Fake);
    production.set_pool_quota("staging", 1);
    let name = EnvironmentName::new("staging", "payments");
    production.queue_fault(Fault::LostAnswer);
    assert_eq!(production.create_environment(name.clone(), key(1), 604_800, 42), ResultValue::Uncertain);
    assert_eq!(production.read_environment(&name).expect("environment exists").key, key(1));
    assert_eq!(production.create_environment(name.clone(), key(1), 604_800, 42), ResultValue::Made);
    assert_eq!(production.used_slots("staging"), 1);
    assert_eq!(
        production
            .observed()
            .iter()
            .filter(|effect| matches!(effect, ObservedEffect::Create { applied: true, .. }))
            .count(),
        1
    );
}

#[test]
fn a_shrinking_pool_keeps_holders_and_conditional_deletion_frees_a_slot() {
    let mut production = Production::new(0, Backend::Fake);
    production.set_pool_quota("staging", 2);
    let a = EnvironmentName::new("staging", "a");
    let b = EnvironmentName::new("staging", "b");
    let c = EnvironmentName::new("staging", "c");
    assert_eq!(production.create_environment(a.clone(), key(1), 100, 10), ResultValue::Made);
    assert_eq!(production.create_environment(b.clone(), key(2), 100, 10), ResultValue::Made);
    production.set_pool_quota("staging", 1);
    assert_eq!(production.create_environment(c.clone(), key(3), 100, 10), ResultValue::Full);
    assert_eq!(production.tear_down(&a, key(2)), ResultValue::Conflict);
    assert_eq!(production.tear_down(&a, key(1)), ResultValue::Made);
    assert_eq!(production.create_environment(c, key(3), 100, 10), ResultValue::Full);
    assert_eq!(production.tear_down(&b, key(2)), ResultValue::Made);
    assert_eq!(production.used_slots("staging"), 0);
}

#[test]
fn hand_changes_are_visible_without_an_application_effect() {
    let mut production = Production::new(0, Backend::Fake);
    let name = ServiceName::new("production", "checkout");
    production.add_service(name.clone(), "v2", 2);
    production.other_hand(Fault::HandScale { service: name.clone(), replicas: 4 });
    assert_eq!(production.read_service(&name).expect("service exists").replicas, 4);
    assert_eq!(production.scale(&name, 2, 1), ResultValue::Conflict);
    assert!(production.observed().iter().any(|effect| matches!(effect, ObservedEffect::Scale { applied: false, .. })));
}

#[test]
fn reads_are_bounded_and_unrelated_history_adds_no_idle_calls() {
    let mut production = Production::new(0, Backend::Fake);
    let name = ServiceName::new("production", "checkout");
    production.add_service(name.clone(), "v2", 2);
    production.preload_history(10_000);
    assert_eq!(production.calls(), 0);
    production.advance(1);
    assert!(production.read_logs(&name, 0, 10, "", 7).len() <= 1);
    assert!(production.read_series(&name, 0, 10, 16).len() <= 1);
    assert_eq!(production.calls(), 2);
}

#[test]
fn faults_are_seeded_and_explicit_faults_apply_at_the_api_boundary() {
    let name = ServiceName::new("production", "checkout");
    let mut a = Production::new(1, Backend::Fake);
    let mut b = Production::new(1, Backend::Fake);
    a.add_service(name.clone(), "v2", 2);
    b.add_service(name.clone(), "v2", 2);
    for n in 1..=98 {
        assert_eq!(a.restart(&name, n), b.restart(&name, n));
    }
    assert_eq!(a.observed(), b.observed());

    a.queue_fault(Fault::ApiError);
    assert_eq!(a.restart(&name, 999), ResultValue::Error);
    assert!(a.find_restart(999).is_none());
    a.set_pool_quota("staging", 1);
    a.queue_fault(Fault::SlowProvisioning { seconds: 60 });
    let env = EnvironmentName::new("staging", "slow");
    assert_eq!(a.create_environment(env.clone(), key(9), 100, 10), ResultValue::Made);
    assert_eq!(a.read_environment(&env).expect("created").ready_at, 65);
}

#[test]
fn one_key_cannot_create_two_environments_or_restart_two_services() {
    let mut production = Production::new(0, Backend::Fake);
    production.set_pool_quota("staging", 2);
    let a = EnvironmentName::new("staging", "a");
    let b = EnvironmentName::new("staging", "b");
    assert_eq!(production.create_environment(a, key(1), 100, 10), ResultValue::Made);
    assert_eq!(production.create_environment(b, key(1), 100, 10), ResultValue::Conflict);

    let x = ServiceName::new("production", "checkout");
    let y = ServiceName::new("production", "payments");
    production.add_service(x.clone(), "v1", 2);
    production.add_service(y.clone(), "v1", 2);
    assert_eq!(production.restart(&x, 77), ResultValue::Made);
    assert_eq!(production.restart(&y, 77), ResultValue::Conflict);
}

#[test]
fn a_keyed_silence_suppresses_alerts_and_can_be_found_after_its_answer_is_lost() {
    let mut production = Production::new(0, Backend::Fake);
    let name = ServiceName::new("production", "checkout");
    production.add_service(name.clone(), "v1", 2);
    production.add_alert_rule("checkout-errors", name.clone());
    production.queue_fault(Fault::LostAnswer);
    assert_eq!(production.silence("checkout-errors", key(8), 100), ResultValue::Uncertain);
    assert!(production.find_silence(key(8)));
    assert_eq!(production.silence("checkout-errors", key(8), 100), ResultValue::Made);
    assert!(production.inject_incident(&name));
    assert!(production.alerts().is_empty());
    assert_eq!(
        production
            .observed()
            .iter()
            .filter(|effect| matches!(effect, ObservedEffect::Silence { applied: true, .. }))
            .count(),
        1
    );
    production.advance(101);
    assert!(production.inject_incident(&name));
    assert_eq!(production.alerts().len(), 1);
}

#[test]
fn conditional_changes_report_their_origin_and_hand_changes_clear_it() {
    let mut production = Production::new(0, Backend::Fake);
    let service = ServiceName::new("production", "checkout");
    production.add_service(service.clone(), "v2", 2);
    assert_eq!(production.scale_with_key(&service, 2, 3, key(1)), ResultValue::Made);
    assert_eq!(production.read_service(&service).expect("service exists").last_change, Some(key(1)));
    production.other_hand(Fault::HandScale { service: service.clone(), replicas: 4 });
    assert_eq!(production.read_service(&service).expect("service exists").last_change, None);
    assert_eq!(production.rollback_with_key(&service, "v2", "v1", key(2)), ResultValue::Made);
    assert_eq!(production.read_service(&service).expect("service exists").last_change, Some(key(2)));

    production.set_pool_quota("staging", 1);
    let environment = EnvironmentName::new("staging", "payments");
    assert_eq!(production.create_environment(environment.clone(), key(3), 100, 10), ResultValue::Made);
    assert_eq!(production.tear_down(&environment, key(3)), ResultValue::Made);
    assert_eq!(production.deleted_by(&environment), Some(key(3)));
}
