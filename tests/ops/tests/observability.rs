use jig_ops_domain_observability as obs;
use jig_ops_fake_production as production;
use jig_ops_world::{World, service};

fn key() -> obs::Key {
    obs::Key { deployment: 1, task: 7, purpose: 3 }
}

#[test]
fn an_alert_is_classified_for_each_subscriber_and_its_echo_is_dropped() {
    let mut world = World::new(0);
    let topic = obs::Topic::Alerts(service());
    world.event(obs::Event::Subscribe { task: 1, topic: topic.clone(), wake_at: 7, keep_at: 3 });
    world.event(obs::Event::Subscribe { task: 2, topic: topic.clone(), wake_at: 9, keep_at: 4 });
    world.event(obs::Event::Subscribe { task: 3, topic, wake_at: 9, keep_at: 8 });
    let name = production::ServiceName::new("production", "checkout");
    assert!(world.production.inject_incident(&name));
    let alert = obs::Alert { number: 1, service: service(), severity: 7 };
    let news = world.event(obs::Event::System(obs::SystemEvent::Alert { alert: alert.clone(), own: false }));
    assert_eq!(
        news,
        vec![obs::Request::News {
            alert: alert.clone(),
            subscribers: Box::from([
                obs::Classed { task: 1, class: obs::Class::Wake },
                obs::Classed { task: 2, class: obs::Class::Keep },
            ]),
        }]
    );
    assert!(world.event(obs::Event::System(obs::SystemEvent::Alert { alert, own: true })).is_empty());
}

#[test]
fn observed_verdicts_wait_for_fresh_facts_and_load_tracks_change() {
    let mut world = World::new(0);
    let ask = obs::Event::Judge {
        token: World::token(1),
        requirement: obs::Requirement::HealthyReplicaElsewhere,
        service: service(),
        freshness: 60,
    };
    let pending = world.event(ask.clone());
    assert!(pending.contains(&obs::Request::Verdict { token: World::token(1), verdict: obs::Verdict::Wait }));
    let settled = world.release(&pending);
    assert!(
        settled.contains(&obs::Request::Verdict { token: World::token(1), verdict: obs::Verdict::Met { observed: 0 } })
    );
    world.now = 61;
    let stale = world.event(ask);
    assert!(stale.contains(&obs::Request::Verdict { token: World::token(1), verdict: obs::Verdict::Wait }));
    world.production.advance(61);
    world.release(&stale);
    let load = world.event(obs::Event::Judge {
        token: World::token(2),
        requirement: obs::Requirement::LoadBelow { percent: 40, for_seconds: 30 },
        service: service(),
        freshness: 60,
    });
    assert!(load.contains(&obs::Request::Verdict { token: World::token(2), verdict: obs::Verdict::Wait }));
    world.production.advance(30);
    world.now = 91;
    world.release(&load);
    let fresh = world.event(obs::Event::Judge {
        token: World::token(3),
        requirement: obs::Requirement::LoadBelow { percent: 40, for_seconds: 30 },
        service: service(),
        freshness: 60,
    });
    assert!(
        fresh.contains(&obs::Request::Verdict { token: World::token(3), verdict: obs::Verdict::Met { observed: 91 } })
    );
    assert!(world.production.inject_incident(&production::ServiceName::new("production", "checkout")));
    let refresh = world.event(obs::Event::System(obs::SystemEvent::Fact {
        service: service(),
        fact: obs::Fact {
            observed: 91,
            healthy_replicas: 2,
            errors: 20,
            error_rate_percent: 20,
            load_percent: 95,
            load: Box::from([obs::LoadPoint { at: 61, percent: 10 }, obs::LoadPoint { at: 91, percent: 95 }]),
        },
    }));
    assert!(refresh.contains(&obs::Request::Changed { service: service() }));
    let failed = world.event(obs::Event::Judge {
        token: World::token(4),
        requirement: obs::Requirement::LoadBelow { percent: 40, for_seconds: 30 },
        service: service(),
        freshness: 60,
    });
    assert!(failed.contains(&obs::Request::Verdict { token: World::token(4), verdict: obs::Verdict::Refuse }));
}

#[test]
fn reads_are_bounded_and_charged_as_production_calls() {
    let mut world = World::new(0);
    let before = world.production.calls();
    let read = world.event(obs::Event::Read {
        token: World::token(1),
        read: obs::Read::Logs {
            service: service(),
            window: obs::Window { from: 0, through: 10 },
            filter: Box::from(*b"base"),
            max_bytes: 8,
        },
    });
    let settled = world.release(&read);
    assert!(settled.contains(&obs::Request::Answer { token: World::token(1), bytes: Box::from(*b"baseline") }));
    assert_eq!(world.production.calls(), before + 1);
    assert_eq!(world.release(&read).iter().filter(|request| matches!(request, obs::Request::Answer { .. })).count(), 0);
}

#[test]
fn a_lost_silence_answer_is_found_once_after_restart() {
    let mut world = World::new(0);
    let effect = obs::Effect { rule: Box::from(*b"checkout-errors"), until: 100 };
    world.event(obs::Event::Describe { token: World::token(1), effect });
    let kept = world.event(obs::Event::Keep { token: World::token(1), key: key() });
    assert_eq!(world.commits, 1);
    world.production.queue_fault(production::Fault::LostAnswer);
    let made = world.release(&kept);
    assert!(made.contains(&obs::Request::Outcome { key: key(), outcome: obs::Outcome::Made }));
    world.restart();
    assert_eq!(
        world
            .production
            .observed()
            .iter()
            .filter(|effect| matches!(effect, production::ObservedEffect::Silence { applied: true, .. }))
            .count(),
        1
    );
}

#[test]
fn a_watch_keeps_its_last_batch_across_a_restart() {
    let mut world = World::new(0);
    world.event(obs::Event::StartWatch { task: 8, services: Box::from([service()]), template: 2 });
    let wake = obs::Event::WakeWatch { task: 8, batch: 9, alerts: Box::from([3, 4]) };
    let first = world.event(wake.clone());
    assert!(first.contains(&obs::Request::Triage { watch: 8, template: 2, batch: 9, alerts: Box::from([3, 4]) }));
    world.restart();
    assert!(world.event(wake).is_empty());
}

#[test]
fn a_watch_subscribes_to_its_services_and_cancellation_erases_its_interest() {
    let mut world = World::new(0);
    let started = world.event(obs::Event::StartWatch { task: 8, services: Box::from([service()]), template: 2 });
    assert!(started.contains(&obs::Request::Save {
        record: obs::Record::Subscription { topic: obs::Topic::Alerts(service()), task: 8, wake_at: 1, keep_at: 0 }
    }));
    let alert = obs::Alert { number: 1, service: service(), severity: 5 };
    let news = world.event(obs::Event::System(obs::SystemEvent::Alert { alert: alert.clone(), own: false }));
    assert_eq!(
        news,
        vec![obs::Request::News {
            alert: alert.clone(),
            subscribers: Box::from([obs::Classed { task: 8, class: obs::Class::Wake },])
        }]
    );
    world.event(obs::Event::StopWatch { task: 8 });
    assert!(world.event(obs::Event::System(obs::SystemEvent::Alert { alert, own: false })).is_empty());
}

#[test]
fn health_topic_reaches_subscribers_when_health_changes() {
    let mut world = World::new(0);
    world.event(obs::Event::Subscribe { task: 3, topic: obs::Topic::Health(service()), wake_at: 7, keep_at: 1 });
    let first = obs::Fact {
        observed: 0,
        healthy_replicas: 2,
        errors: 0,
        error_rate_percent: 0,
        load_percent: 10,
        load: Box::from([obs::LoadPoint { at: 0, percent: 10 }]),
    };
    world.event(obs::Event::System(obs::SystemEvent::Fact { service: service(), fact: first.clone() }));
    let changed = world.event(obs::Event::System(obs::SystemEvent::Fact {
        service: service(),
        fact: obs::Fact { healthy_replicas: 0, ..first },
    }));
    assert!(changed.contains(&obs::Request::Health {
        service: service(),
        healthy: false,
        subscribers: Box::from([obs::Classed { task: 3, class: obs::Class::Wake }]),
    }));
}
