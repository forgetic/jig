use jig_ops_domain_infrastructure as infra;
use jig_ops_domain_observability as obs;
use jig_ops_fake_production as production;
use jig_ops_world::{InfrastructureWorld, World, infra_environment, infra_service, service};
use skein_lib::Token;

fn infra_key() -> infra::Key {
    infra::Key::procedure([1; 16], 7, 3)
}

fn obs_key() -> obs::Key {
    obs::Key::procedure([1; 16], 7, 3)
}

#[test]
fn observability_refreshes_live_services_before_resuming_after_restart() {
    let mut world = World::new(0);
    world.event(obs::Event::Subscribe { task: 7, topic: obs::Topic::Health(service()), wake_at: 7, keep_at: 1 });
    assert!(world.production.inject_incident(&production::ServiceName::new("production", "checkout")));
    let before = world.production.calls();
    world.restart();
    assert_eq!(world.production.calls(), before + 2, "one service fact and its bounded series");
    let judged = world.event(obs::Event::Judge {
        token: Token::new(1),
        requirement: obs::Requirement::HealthyReplicaElsewhere,
        service: service(),
        freshness: 60,
    });
    assert!(
        judged.contains(&obs::Request::Verdict { token: Token::new(1), verdict: obs::Verdict::Met { observed: 0 } })
    );
}

#[test]
fn observability_keyed_silence_survives_each_commit_cut() {
    for cut in 0..3 {
        let mut world = World::new(0);
        let token = Token::new(1);
        world.event(obs::Event::Describe {
            token,
            effect: obs::Effect { rule: Box::from(*b"checkout-errors"), until: 90 },
        });
        let kept = world.event(obs::Event::Keep { entry: 3, token, key: obs_key() });
        match cut {
            0 => {
                world.restart();
            }
            1 => {
                world.event(obs::Event::Make { key: obs_key() });
                world.restart();
                world.now = 30;
                world.fire();
            }
            2 => {
                world.production.queue_fault(production::Fault::LostAnswer);
                world.release(&kept);
                world.restart();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            world
                .production
                .observed()
                .iter()
                .filter(|row| matches!(row, production::ObservedEffect::Silence { applied: true, .. }))
                .count(),
            1,
            "cut {cut}"
        );
        assert!(
            world
                .event(obs::Event::System(obs::SystemEvent::Applied {
                    key: obs_key(),
                    attempt: 1,
                    outcome: obs::Outcome::Made,
                }))
                .is_empty(),
            "a late copy cannot settle twice"
        );
    }
}

#[test]
fn infrastructure_retries_only_after_the_saved_deadline_and_ignores_late_copies() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let token = Token::new(1);
    world.event(infra::Event::Describe {
        token,
        effect: infra::Effect::CreateEnvironment { environment: infra_environment(), until: 100, price: 30 },
    });
    world.event(infra::Event::Keep { entry: 3, token, key: infra_key() });
    let prepared = world.event(infra::Event::Make { key: infra_key() });
    assert!(
        prepared
            .iter()
            .any(|row| matches!(row, infra::Request::System(infra::SystemRequest::Apply { attempt: 1, .. })))
    );
    let uncertain = world.event(infra::Event::System(infra::SystemEvent::Applied {
        key: infra_key(),
        attempt: 1,
        result: infra::ApplyResult::Uncertain,
    }));
    assert!(uncertain.iter().any(|row| matches!(row, infra::Request::System(infra::SystemRequest::Look { .. }))));
    world.event(infra::Event::System(infra::SystemEvent::Looked { key: infra_key(), result: infra::Looked::CanRetry }));
    world.now = 29;
    world.restart();
    assert!(world.fire().is_empty());
    assert_eq!(world.production.observed().len(), 0);
    world.now = 30;
    let made = world.fire();
    assert!(made.contains(&infra::Request::Outcome { entry: 3, key: infra_key(), outcome: infra::Outcome::Made }));
    assert_eq!(world.production.used_slots("staging"), 1);
    assert!(
        world
            .event(infra::Event::System(infra::SystemEvent::Applied {
                key: infra_key(),
                attempt: 1,
                result: infra::ApplyResult::Made,
            }))
            .is_empty()
    );
    assert_eq!(
        world
            .production
            .observed()
            .iter()
            .filter(|row| matches!(row, production::ObservedEffect::Create { applied: true, .. }))
            .count(),
        1
    );
}

#[test]
fn a_hand_reaching_a_conditional_target_is_ambiguous() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let service_name = production::ServiceName::new("production", "checkout");
    let token = Token::new(1);
    world.event(infra::Event::Describe {
        token,
        effect: infra::Effect::Scale { service: infra_service(), from: 3, to: 4 },
    });
    world.event(infra::Event::Keep { entry: 3, token, key: infra_key() });
    world.event(infra::Event::Make { key: infra_key() });
    world.production.other_hand(production::Fault::HandScale { service: service_name, replicas: 4 });
    let uncertain = world.event(infra::Event::System(infra::SystemEvent::Applied {
        key: infra_key(),
        attempt: 1,
        result: infra::ApplyResult::Uncertain,
    }));
    let settled = world.release(&uncertain);
    assert!(settled.contains(&infra::Request::Outcome {
        entry: 3,
        key: infra_key(),
        outcome: infra::Outcome::Uncertain
    }));
    world.now = 40;
    world.fire();
    world.restart();
    assert!(world.production.observed().is_empty(), "another hand's target is never claimed or retried");
}

#[test]
fn infrastructure_procedure_repeats_a_wait_but_never_repeats_an_effect_decision() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    world.event(infra::Event::StartProcedure {
        task: 7,
        procedure: infra::Procedure::Remediate { service: infra_service(), operation: 17, deadline: 90 },
    });
    let first = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    world.release(&first);
    assert!(world.production.inject_incident(&production::ServiceName::new("production", "checkout")));
    world.release(&[infra::Request::System(infra::SystemRequest::Service { service: infra_service() })]);
    let once = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(once.iter().any(|row| matches!(
        row,
        infra::Request::Step { decision: infra::StepDecision::Effect(infra::Effect::Restart { .. }), .. }
    )));
    let again = world.event(infra::Event::Procedure { task: 7, signal: infra::ProcedureSignal::Step });
    assert!(
        !again.iter().any(|row| matches!(row, infra::Request::Step { decision: infra::StepDecision::Effect(..), .. }))
    );
}

#[test]
fn idle_calls_depend_on_live_resources_and_changes_not_old_history() {
    let mut short = World::new(0);
    let mut long = World::new(0);
    short.production.preload_history(10);
    long.production.preload_history(100);
    short.event(obs::Event::Subscribe { task: 1, topic: obs::Topic::Health(service()), wake_at: 7, keep_at: 1 });
    long.event(obs::Event::Subscribe { task: 1, topic: obs::Topic::Health(service()), wake_at: 7, keep_at: 1 });
    short.restart();
    long.restart();
    for _ in 0..20 {
        short.fire();
        long.fire();
    }
    assert_eq!(short.production.calls(), long.production.calls());
    let mut short = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let mut long = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    short.production.preload_history(10);
    long.production.preload_history(100);
    short.event(infra::Event::Names {
        project: 1,
        task: 1,
        resources: Box::from([infra::Resource::Service(infra_service())]),
    });
    long.event(infra::Event::Names {
        project: 1,
        task: 1,
        resources: Box::from([infra::Resource::Service(infra_service())]),
    });
    short.restart();
    long.restart();
    for _ in 0..20 {
        short.fire();
        long.fire();
    }
    assert_eq!(short.production.calls(), long.production.calls());
}

#[test]
fn a_conditional_effect_fails_when_facts_change_after_decision() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let token = Token::new(1);
    world.event(infra::Event::Describe {
        token,
        effect: infra::Effect::Scale { service: infra_service(), from: 3, to: 4 },
    });
    let kept = world.event(infra::Event::Keep { entry: 3, token, key: infra_key() });
    world.production.other_hand(production::Fault::HandScale {
        service: production::ServiceName::new("production", "checkout"),
        replicas: 5,
    });
    let refused = world.release(&kept);
    assert!(refused.contains(&infra::Request::Outcome { entry: 3, key: infra_key(), outcome: infra::Outcome::Failed }));
    assert_eq!(
        world
            .production
            .observed()
            .iter()
            .filter(|row| matches!(row, production::ObservedEffect::Scale { applied: true, .. }))
            .count(),
        0
    );
}

#[test]
fn a_hand_deleting_an_owned_environment_reports_drift() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let environment = infra_environment();
    world.event(infra::Event::Names {
        project: 1,
        task: 7,
        resources: Box::from([infra::Resource::Environment(environment.clone())]),
    });
    world.event(infra::Event::Describe {
        token: Token::new(1),
        effect: infra::Effect::CreateEnvironment { environment: environment.clone(), until: 100, price: 20 },
    });
    let kept = world.event(infra::Event::Keep { entry: 3, token: Token::new(1), key: infra_key() });
    world.release(&kept);
    world.release(&[infra::Request::System(infra::SystemRequest::Environment { environment: environment.clone() })]);
    world.production.other_hand(production::Fault::HandDelete {
        environment: production::EnvironmentName::new("staging", "payments"),
    });
    let changed = world
        .release(&[infra::Request::System(infra::SystemRequest::Environment { environment: environment.clone() })]);
    assert!(changed.contains(&infra::Request::Drift { task: 7, resource: infra::Resource::Environment(environment) }));
}

#[test]
fn infrastructure_creation_is_made_once_across_each_restart_cut() {
    for cut in 0..3 {
        let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
        world.event(infra::Event::Describe {
            token: Token::new(1),
            effect: infra::Effect::CreateEnvironment { environment: infra_environment(), until: 100, price: 20 },
        });
        let kept = world.event(infra::Event::Keep { entry: 3, token: Token::new(1), key: infra_key() });
        match cut {
            0 => {
                world.restart();
            }
            1 => {
                world.event(infra::Event::Make { key: infra_key() });
                world.restart();
                world.now = 30;
                world.fire();
            }
            2 => {
                world.production.queue_fault(production::Fault::LostAnswer);
                world.release(&kept);
                world.restart();
            }
            _ => unreachable!(),
        }
        assert_eq!(world.production.used_slots("staging"), 1, "cut {cut}");
        assert_eq!(
            world
                .production
                .observed()
                .iter()
                .filter(|row| matches!(row, production::ObservedEffect::Create { applied: true, .. }))
                .count(),
            1,
            "cut {cut}"
        );
    }
}

#[test]
fn dropping_a_staged_silence_discards_its_value() {
    let mut world = World::new(0);
    let token = Token::new(8);
    world.event(obs::Event::Describe {
        token,
        effect: obs::Effect { rule: Box::from(*b"checkout-errors"), until: 100 },
    });
    world.event(obs::Event::Drop { token });
    assert!(world.event(obs::Event::Keep { entry: 3, token, key: obs_key() }).is_empty());
    world.restart();
    assert!(world.production.observed().is_empty());
}

#[test]
fn a_pool_shrink_keeps_holders_and_new_claims_wait() {
    let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
    let pool = infra::Pool(Box::from(*b"staging"));
    let held = world.event(infra::Event::Names {
        project: 1,
        task: 1,
        resources: Box::from([infra::Resource::Pool(pool.clone())]),
    });
    assert!(held.contains(&infra::Request::Named {
        project: 1,
        task: 1,
        resources: Box::from([infra::Named {
            role: infra::ResourceRole::Owned,
            resource: infra::Resource::Pool(pool.clone()),
            hold: infra::Hold::PooledWait,
        }]),
    }));
    world.event(infra::Event::System(infra::SystemEvent::Pool { pool: pool.clone(), quota: 1, used: 1 }));
    let shrunk = world.event(infra::Event::System(infra::SystemEvent::Pool { pool: pool.clone(), quota: 0, used: 1 }));
    assert!(shrunk.contains(&infra::Request::Slots { pool: pool.clone(), quota: 0, used: 1 }));
    let waiting = world.event(infra::Event::Names {
        project: 1,
        task: 2,
        resources: Box::from([infra::Resource::Pool(pool.clone())]),
    });
    assert!(waiting.contains(&infra::Request::Named {
        project: 1,
        task: 2,
        resources: Box::from([infra::Named {
            role: infra::ResourceRole::Owned,
            resource: infra::Resource::Pool(pool),
            hold: infra::Hold::PooledWait
        }]),
    }));
    assert!(!shrunk.iter().any(|row| matches!(row, infra::Request::Erase { key: infra::RecordKey::Rely(1) })));
}

#[test]
fn conditional_scale_and_rollback_find_their_own_lost_answers() {
    let effects = [
        infra::Effect::Scale { service: infra_service(), from: 3, to: 4 },
        infra::Effect::Rollback { service: infra_service(), from: Box::from(*b"v2"), to: Box::from(*b"v1") },
    ];
    for effect in effects {
        let mut world = InfrastructureWorld::new(0, infra::Backend::OperationIds);
        world.event(infra::Event::Describe { token: Token::new(1), effect });
        world.production.queue_fault(production::Fault::LostAnswer);
        let kept = world.event(infra::Event::Keep { entry: 3, token: Token::new(1), key: infra_key() });
        let settled = world.release(&kept);
        assert!(settled.contains(&infra::Request::Outcome {
            entry: 3,
            key: infra_key(),
            outcome: infra::Outcome::Made
        }));
        world.restart();
        assert_eq!(
            world
                .production
                .observed()
                .iter()
                .filter(|row| match row {
                    production::ObservedEffect::Scale { applied, .. }
                    | production::ObservedEffect::Rollback { applied, .. } => *applied,
                    production::ObservedEffect::Silence { .. }
                    | production::ObservedEffect::Restart { .. }
                    | production::ObservedEffect::Create { .. }
                    | production::ObservedEffect::TearDown { .. } => false,
                })
                .count(),
            1
        );
    }
}
