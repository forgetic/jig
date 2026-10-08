use alloc::boxed::Box;
use skein_lib::{Env, Queue, Time, Token, Wall};

use super::*;

fn limits() -> Limits {
    Limits {
        services: 4,
        subscriptions: 8,
        watches: 2,
        staged: 2,
        effects: 2,
        judges: 4,
        reads: 2,
        subscribers_per_alert: 8,
        samples_per_service: 8,
        services_per_watch: 2,
        alerts_per_batch: 4,
        name_bytes: 32,
        answer_bytes: 64,
        max_attempts: 3,
        retry_after_seconds: 30,
    }
}

fn service() -> Service {
    Service::new(Box::from(*b"prod"), Box::from(*b"checkout"))
}

fn event(domain: &mut Domain, seconds: u64, input: Event) -> Queue<Request> {
    let wall = seconds.checked_mul(1_000_000_000).expect("world clock fits");
    let env = Env { now: Time::from_nanos(seconds), wall: Wall::from_nanos(wall), limits: limits() };
    let mut out = Queue::with_capacity(MAX_OUT);
    step(domain, &env, input, &mut out);
    out
}

fn fact(at: u64, load: u8) -> Fact {
    Fact {
        observed: at,
        healthy_replicas: 2,
        errors: 0,
        error_rate_percent: 0,
        load_percent: load,
        load: Box::from([LoadPoint { at: 0, percent: 10 }, LoadPoint { at, percent: load }]),
    }
}

#[test]
fn a_fresh_observed_verdict_uses_this_connectors_facts() {
    let mut domain = Domain::new(&limits());
    let token = Token::new(1);
    let mut judged = event(
        &mut domain,
        30,
        Event::Judge { token, requirement: Requirement::HealthyReplicaElsewhere, service: service(), freshness: 60 },
    );
    assert_eq!(judged.pop(), Some(Request::Verdict { token, verdict: Verdict::Wait }));
    assert_eq!(judged.pop(), Some(Request::System(SystemRequest::Facts { service: service() })));
    let mut fresh = event(&mut domain, 30, Event::System(SystemEvent::Fact { service: service(), fact: fact(30, 20) }));
    assert_eq!(fresh.pop(), Some(Request::Changed { service: service() }));
    assert_eq!(fresh.pop(), Some(Request::Verdict { token, verdict: Verdict::Met { observed: 30 } }));
    let mut load = event(
        &mut domain,
        30,
        Event::Judge {
            token: Token::new(2),
            requirement: Requirement::LoadBelow { percent: 40, for_seconds: 30 },
            service: service(),
            freshness: 60,
        },
    );
    assert_eq!(load.pop(), Some(Request::Verdict { token: Token::new(2), verdict: Verdict::Met { observed: 30 } }));
}

#[test]
fn a_watch_emits_one_triage_per_committed_batch() {
    let mut domain = Domain::new(&limits());
    let mut started =
        event(&mut domain, 1, Event::StartWatch { task: 7, services: Box::from([service()]), template: 3 });
    assert_eq!(
        started.pop(),
        Some(Request::Save {
            record: Record::Subscription {
                topic: Topic::Alerts(service()),
                task: 7,
                subscription: 7,
                wake_at: 1,
                keep_at: 0
            }
        })
    );
    assert_eq!(
        started.pop(),
        Some(Request::Save {
            record: Record::Watch(Watch {
                task: 7,
                services: Box::from([service()]),
                template: 3,
                last_batch: None,
                pending: Box::new([])
            })
        })
    );
    let wake = Event::WakeWatch { task: 7, batch: 99, alerts: Box::from([1, 2]) };
    let mut first = event(&mut domain, 2, wake.clone());
    assert_eq!(
        first.pop(),
        Some(Request::Save {
            record: Record::Watch(Watch {
                task: 7,
                services: Box::from([service()]),
                template: 3,
                last_batch: Some(99),
                pending: Box::new([]),
            })
        })
    );
    assert_eq!(
        first.pop(),
        Some(Request::Triage { watch: 7, template: 3, batch: 99, alerts: Box::from([1, 2]), delegate: Box::new([]) })
    );
    assert!(event(&mut domain, 2, wake).is_empty(), "same batch makes no second task");
}

#[test]
fn a_silence_is_saved_before_its_system_call_and_found_after_uncertainty() {
    let mut domain = Domain::new(&limits());
    let token = Token::new(5);
    let key = Key::procedure([1; 16], 7, 3);
    let effect = Effect { rule: Box::from(*b"checkout-errors"), until: 100 };
    let mut described = event(&mut domain, 1, Event::Describe { token, effect: effect.clone() });
    assert_eq!(described.pop(), Some(Request::Described { token, effect: effect.clone() }));
    let mut kept = event(&mut domain, 1, Event::Keep { entry: 3, token, key });
    assert_eq!(
        kept.pop(),
        Some(Request::Save {
            record: Record::Outbox(Entry {
                number: 3,
                key,
                effect: effect.clone(),
                attempt: 0,
                deadline: 0,
                phase: Phase::Kept
            })
        })
    );
    assert_eq!(kept.pop(), Some(Request::Make { key }));
    let mut made = event(&mut domain, 1, Event::Make { key });
    assert_eq!(
        made.pop(),
        Some(Request::Save {
            record: Record::Outbox(Entry {
                number: 3,
                key,
                effect: effect.clone(),
                attempt: 1,
                deadline: 31,
                phase: Phase::Sent
            })
        })
    );
    assert_eq!(made.pop(), Some(Request::System(SystemRequest::Silence { key, attempt: 1, effect: effect.clone() })));
    let mut uncertain =
        event(&mut domain, 1, Event::System(SystemEvent::Applied { key, attempt: 1, outcome: Outcome::Uncertain }));
    assert_eq!(
        uncertain.pop(),
        Some(Request::Save {
            record: Record::Outbox(Entry {
                number: 3,
                key,
                effect: effect.clone(),
                attempt: 1,
                deadline: 31,
                phase: Phase::Uncertain
            })
        })
    );
    assert_eq!(uncertain.pop(), Some(Request::System(SystemRequest::FindSilence { key, rule: effect.rule })));
    let mut found = event(&mut domain, 1, Event::System(SystemEvent::Found { key, found: true }));
    assert_eq!(found.pop(), Some(Request::Erase { key: RecordKey::Outbox(key) }));
    assert_eq!(found.pop(), Some(Request::Outcome { entry: 3, key, outcome: Outcome::Made }));
}

#[test]
fn an_uncertain_silence_waits_for_its_durable_deadline_before_retry() {
    let mut domain = Domain::new(&limits());
    let token = Token::new(5);
    let key = Key::procedure([1; 16], 7, 3);
    let effect = Effect { rule: Box::from(*b"checkout-errors"), until: 100 };
    event(&mut domain, 1, Event::Describe { token, effect: effect.clone() });
    event(&mut domain, 1, Event::Keep { entry: 3, token, key });
    event(&mut domain, 1, Event::Make { key });
    event(&mut domain, 1, Event::System(SystemEvent::Applied { key, attempt: 1, outcome: Outcome::Uncertain }));
    assert_eq!(next_deadline(&domain), Some(Time::from_nanos(31_000_000_000)));
    let mut early = event(&mut domain, 2, Event::System(SystemEvent::Found { key, found: false }));
    assert_eq!(
        early.pop(),
        Some(Request::Save {
            record: Record::Outbox(Entry {
                number: 3,
                key,
                effect: effect.clone(),
                attempt: 1,
                deadline: 31,
                phase: Phase::Uncertain,
            })
        })
    );
    assert!(early.is_empty(), "no retry before the deadline");
    let env = Env { now: Time::from_nanos(31), wall: Wall::from_nanos(31_000_000_000), limits: limits() };
    let mut due = Queue::with_capacity(MAX_OUT);
    fire(&mut domain, &env, &mut due);
    assert_eq!(due.pop(), Some(Request::System(SystemRequest::FindSilence { key, rule: effect.rule.clone() })));
    let mut after = event(&mut domain, 31, Event::System(SystemEvent::Found { key, found: false }));
    assert_eq!(
        after.pop(),
        Some(Request::Save {
            record: Record::Outbox(Entry {
                number: 3,
                key,
                effect: effect.clone(),
                attempt: 1,
                deadline: 31,
                phase: Phase::Kept
            })
        })
    );
    assert_eq!(after.pop(), Some(Request::Make { key }));
    event(&mut domain, 31, Event::Make { key });
    let mut late =
        event(&mut domain, 31, Event::System(SystemEvent::Applied { key, attempt: 1, outcome: Outcome::Made }));
    assert_eq!(late.pop(), Some(Request::Erase { key: RecordKey::Outbox(key) }));
    assert_eq!(late.pop(), Some(Request::Outcome { entry: 3, key, outcome: Outcome::Made }));
}

#[test]
fn sustained_load_needs_a_whole_window_and_a_low_starting_sample() {
    let mut domain = Domain::new(&limits());
    event(&mut domain, 10, Event::System(SystemEvent::Fact { service: service(), fact: fact(10, 10) }));
    let judge = Event::Judge {
        token: Token::new(1),
        requirement: Requirement::LoadBelow { percent: 40, for_seconds: 30 },
        service: service(),
        freshness: 60,
    };
    assert_eq!(
        event(&mut domain, 10, judge.clone()).pop(),
        Some(Request::Verdict { token: Token::new(1), verdict: Verdict::Wait })
    );
    let facts = Fact {
        load: Box::from([
            LoadPoint { at: 0, percent: 95 },
            LoadPoint { at: 20, percent: 10 },
            LoadPoint { at: 40, percent: 10 },
        ]),
        ..fact(40, 10)
    };
    event(&mut domain, 40, Event::System(SystemEvent::Fact { service: service(), fact: facts }));
    assert_eq!(
        event(&mut domain, 40, judge).pop(),
        Some(Request::Verdict { token: Token::new(1), verdict: Verdict::Wait })
    );
}
