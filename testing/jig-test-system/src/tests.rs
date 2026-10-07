use std::boxed::Box;

use jig_test_connector::{
    self as connector, Config, Domain, Effect, Event, Form, Hold, Key, KindSpec, Limits, Outcome, Path, Record,
    Recovery, Request, ResourceSpec, SystemRequest,
};
use skein_lib::{Duration, Env, List, Queue, Time, Token, Wall};

use crate::{Copy, Fault, System};

const LIMITS: Limits = Limits {
    tasks: 1,
    adoptions: 1,
    subscriptions: 0,
    pools: 0,
    resources: 2,
    topics: 0,
    kinds: 4,
    requirements: 0,
    procedures: 0,
    actions_per_procedure: 0,
    facts: 0,
    judges: 0,
    values: 0,
    value_bytes: 0,
    staged: 2,
    entries: 2,
    made: 3,
    resources_per_effect: 1,
    max_attempts: 3,
    write_lifetime: Duration::from_secs(5),
    clock_margin: Duration::from_secs(1),
    resources_per_task: 1,
    subscribers_per_topic: 0,
    lost_per_pool: 0,
    path_segments: 2,
    segment_bytes: 16,
};

fn path(name: &[u8]) -> Path {
    let mut segments = List::with_capacity(2);
    segments.push(Box::from(&b"unit"[..])).expect("prefix fits");
    segments.push(Box::from(name)).expect("name fits");
    Path::new(segments, LIMITS.segment_bytes).expect("nonempty path")
}

fn prefix() -> Path {
    let mut segments = List::with_capacity(1);
    segments.push(Box::from(&b"unit"[..])).expect("prefix fits");
    Path::new(segments, LIMITS.segment_bytes).expect("nonempty path")
}

fn config() -> Config {
    Config {
        deployment: 9,
        prefix: prefix(),
        resources: Box::from([
            ResourceSpec { path: path(b"one"), hold: Hold::None, writable: true },
            ResourceSpec { path: path(b"two"), hold: Hold::None, writable: true },
        ]),
        pools: Box::from([]),
        topics: Box::from([]),
        kinds: Box::from([
            KindSpec { kind: 1, form: Form::Creation, recovery: Recovery::Keyed, price: Some(3) },
            KindSpec { kind: 2, form: Form::Transition, recovery: Recovery::Conditional, price: Some(5) },
            KindSpec { kind: 3, form: Form::Set, recovery: Recovery::Idempotent, price: None },
            KindSpec { kind: 4, form: Form::Transition, recovery: Recovery::Unrecoverable, price: Some(2) },
        ]),
        requirements: Box::from([]),
        procedures: Box::from([]),
    }
}

fn effect(kind: u16, purpose: u64, target: u64) -> Effect {
    Effect {
        kind,
        resources: Box::from([path(b"one")]),
        purpose,
        condition: if kind == 2 { Some(0) } else { None },
        target,
        state: target,
    }
}

fn run(domain: &mut Domain, at: u64, event: Event) -> Vec<Request> {
    let env = Env { now: Time::ZERO, wall: Wall::from_nanos(at), limits: LIMITS };
    let mut out = Queue::with_capacity(connector::MAX_OUT);
    connector::step(domain, &env, event, &mut out);
    let mut requests = Vec::new();
    while let Some(request) = out.pop() {
        requests.push(request);
    }
    requests
}

fn fire(domain: &mut Domain, at: u64) -> Vec<Request> {
    let env = Env { now: Time::ZERO, wall: Wall::from_nanos(at), limits: LIMITS };
    let mut out = Queue::with_capacity(connector::MAX_OUT);
    connector::fire(domain, &env, &mut out);
    let mut requests = Vec::new();
    while let Some(request) = out.pop() {
        requests.push(request);
    }
    requests
}

fn keep(domain: &mut Domain, kind: u16, entry: u64, purpose: u64, target: u64) -> Vec<Request> {
    let token = Token::new(entry);
    let described = run(domain, 0, Event::Describe { token, effect: effect(kind, purpose, target) });
    assert!(matches!(described.as_slice(), [Request::Described { .. }]));
    run(domain, 0, Event::Keep { token, entry, task: 4, key: Key { deployment: 9, task: 4, purpose } })
}

fn system_call(requests: &[Request]) -> SystemRequest {
    for request in requests {
        if let Request::System(call) = request {
            return call.clone();
        }
    }
    panic!("a system request was emitted")
}

fn saved_outbox(requests: &[Request]) -> Record {
    for request in requests {
        if let Request::Save { record: record @ Record::Outbox(..) } = request {
            return record.clone();
        }
    }
    panic!("an outbox record was saved")
}

#[test]
fn every_form_and_recovery_class_has_a_description_and_price() {
    let mut domain = Domain::new(config(), &LIMITS);
    for (kind, form, recovery, price) in [
        (1, Form::Creation, Recovery::Keyed, Some(3)),
        (2, Form::Transition, Recovery::Conditional, Some(5)),
        (3, Form::Set, Recovery::Idempotent, None),
        (4, Form::Transition, Recovery::Unrecoverable, Some(2)),
    ] {
        let token = Token::new(u64::from(kind));
        let described = run(&mut domain, 0, Event::Describe { token, effect: effect(kind, 8, 11) });
        match described.as_slice() {
            [Request::Described { description, .. }] => {
                assert_eq!(description.form, form);
                assert_eq!(description.recovery, recovery);
                assert_eq!(description.price, price);
                assert_eq!(description.state, 11);
            }
            other => panic!("unexpected description: {other:?}"),
        }
        run(&mut domain, 0, Event::Drop { token });
    }
}

#[test]
fn keyed_timeout_found_after_restart_is_made_once() {
    let mut domain = Domain::new(config(), &LIMITS);
    let mut system = System::new();
    let kept = keep(&mut domain, 1, 1, 10, 7);
    assert!(matches!(kept.as_slice(), [Request::Save { .. }, Request::Make { entry: 1 }]));
    let started = run(&mut domain, 0, Event::Make { entry: 1 });
    let durable_attempt = saved_outbox(&started);
    assert!(matches!(started.as_slice(), [Request::Save { .. }, Request::System(..)]));
    let reply = system.answer(system_call(&started), Fault::AfterApply);
    assert_eq!(system.observed().len(), 1);
    assert!(system.observed()[0].applied);
    let mut cold = Domain::new(config(), &LIMITS);
    run(&mut cold, 1, Event::Restore { record: durable_attempt });
    let seeking = fire(&mut cold, 1);
    assert!(matches!(seeking.as_slice(), [Request::System(SystemRequest::Look { .. })]));
    let found = system.answer(system_call(&seeking), Fault::None);
    let settled = run(&mut cold, 1, Event::System(found));
    assert!(matches!(
        settled.last(),
        Some(Request::Outcome { entry: 1, outcome: Outcome::Made { state: 7, found: true } })
    ));
    assert_eq!(system.observed().len(), 1);
    assert!(fire(&mut cold, 20).is_empty());
    let _ = reply;
}

#[test]
fn a_late_keyed_copy_and_retry_change_the_target_once() {
    let mut domain = Domain::new(config(), &LIMITS);
    let mut system = System::new();
    keep(&mut domain, 1, 1, 10, 7);
    let started = run(&mut domain, 0, Event::Make { entry: 1 });
    let uncertain = system.answer(system_call(&started), Fault::Late);
    let seeking = run(&mut domain, 0, Event::System(uncertain));
    assert!(matches!(seeking.last(), Some(Request::System(SystemRequest::Look { .. }))));
    let not_found = system.answer(system_call(&seeking), Fault::None);
    assert!(run(&mut domain, 0, Event::System(not_found)).is_empty());
    assert!(fire(&mut domain, 5).is_empty());
    let again = fire(&mut domain, Duration::from_secs(6).as_nanos());
    assert!(matches!(again.as_slice(), [Request::System(SystemRequest::Look { .. })]));
    let not_found = system.answer(system_call(&again), Fault::None);
    let retried = run(&mut domain, Duration::from_secs(6).as_nanos(), Event::System(not_found));
    assert!(matches!(
        retried.as_slice(),
        [Request::Save { .. }, Request::System(SystemRequest::Apply { attempt: 2, .. })]
    ));
    let made = system.answer(system_call(&retried), Fault::None);
    run(&mut domain, Duration::from_secs(6).as_nanos(), Event::System(made));
    let late = system.deliver_late().expect("late copy was scheduled");
    run(&mut domain, Duration::from_secs(7).as_nanos(), Event::System(late));
    assert_eq!(system.observed().iter().filter(|row| row.applied).count(), 1);
    assert_eq!(system.observed().last().expect("late copy observed").copy, Copy::Late);
}

#[test]
fn a_condition_fails_when_another_hand_reaches_the_target() {
    let mut domain = Domain::new(config(), &LIMITS);
    let mut system = System::new();
    keep(&mut domain, 2, 1, 10, 7);
    let started = run(&mut domain, 0, Event::Make { entry: 1 });
    let uncertain = system.answer(system_call(&started), Fault::Late);
    let seeking = run(&mut domain, 0, Event::System(uncertain));
    system.other_hand(&path(b"one"), 7);
    let looked = system.answer(system_call(&seeking), Fault::None);
    let failed = run(&mut domain, 1, Event::System(looked));
    assert!(matches!(failed.last(), Some(Request::Outcome { outcome: Outcome::Failed, .. })));
    let late = system.deliver_late().expect("late copy arrived");
    run(&mut domain, 2, Event::System(late));
    assert_eq!(system.state(&path(b"one")), Some(7));
    assert!(system.observed().iter().all(|row| !row.applied));
}

#[test]
fn an_idempotent_set_can_repeat_and_unrecoverable_uncertainty_holds() {
    let mut domain = Domain::new(config(), &LIMITS);
    let mut system = System::new();
    keep(&mut domain, 3, 1, 10, 7);
    let first = run(&mut domain, 0, Event::Make { entry: 1 });
    let uncertain = system.answer(system_call(&first), Fault::AfterApply);
    run(&mut domain, 0, Event::System(uncertain));
    let retry = fire(&mut domain, Duration::from_secs(6).as_nanos());
    let made = system.answer(system_call(&retry), Fault::None);
    run(&mut domain, Duration::from_secs(6).as_nanos(), Event::System(made));
    assert_eq!(system.state(&path(b"one")), Some(7));
    assert_eq!(system.observed().len(), 2);

    let mut held = Domain::new(config(), &LIMITS);
    let mut another = System::new();
    keep(&mut held, 4, 2, 11, 8);
    let first = run(&mut held, 0, Event::Make { entry: 2 });
    let uncertain = another.answer(system_call(&first), Fault::AfterApply);
    let held_result = run(&mut held, 0, Event::System(uncertain));
    assert!(matches!(held_result.last(), Some(Request::Outcome { outcome: Outcome::Uncertain, .. })));
    assert!(fire(&mut held, Duration::from_secs(100).as_nanos()).is_empty());
    assert_eq!(another.observed().len(), 1);
}

#[test]
fn an_entry_on_the_same_resource_waits_for_the_first_one_to_settle() {
    let mut domain = Domain::new(config(), &LIMITS);
    let mut system = System::new();
    keep(&mut domain, 3, 1, 10, 7);
    keep(&mut domain, 3, 2, 11, 8);
    assert!(run(&mut domain, 0, Event::Make { entry: 2 }).is_empty());
    let first = run(&mut domain, 0, Event::Make { entry: 1 });
    let reply = system.answer(system_call(&first), Fault::None);
    run(&mut domain, 0, Event::System(reply));
    let second = fire(&mut domain, 0);
    assert!(matches!(
        second.as_slice(),
        [Request::Save { .. }, Request::System(SystemRequest::Apply { entry: 2, .. })]
    ));
}
