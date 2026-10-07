//! The engine's local slots and worker-only charters at the fleet boundary.

use jig_core_fleet::{self as fleet, Answer, Domain, Event, Hello, HostKind, Kinds, Limits, Request};
use skein_lib::{Duration, Env, Queue, ReplyTo, Time, Token, Wall};

fn step(domain: &mut Domain, limits: Limits, event: Event) -> Vec<Request> {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
    let mut out = Queue::with_capacity(fleet::max_out(&limits));
    fleet::step(domain, &env, event, &mut out);
    let mut requests = Vec::new();
    while let Some(request) = out.pop() {
        requests.push(request);
    }
    domain.reclaim();
    requests
}

fn place(domain: &mut Domain, limits: Limits) -> Vec<Request> {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
    let mut out = Queue::with_capacity(fleet::max_out(&limits));
    fleet::resume(domain, &env, &mut out);
    let mut requests = Vec::new();
    while let Some(request) = out.pop() {
        requests.push(request);
    }
    domain.reclaim();
    requests
}

#[test]
fn a_run_placed_on_the_engine_slots_is_settled_as_not_started_by_a_restart_before_its_first_turn() {
    let limits = Limits { workers: 0, engine_slots: 1, ..jig_fleet_world::LIMITS };
    let mut domain = Domain::new(&limits);
    let task = Token::new(1);
    let attempt = Token::new(2);
    assert!(
        step(
            &mut domain,
            limits,
            Event::Start { reply_to: ReplyTo::new(attempt), run: task, attempt, workstream: 1, kinds: Kinds::Engine }
        )
        .is_empty()
    );
    assert_eq!(
        place(&mut domain, limits),
        vec![
            Request::Assign { channel: Token::new(0), kind: HostKind::Engine, run: task, attempt },
            Request::Placed { run: task, attempt },
        ]
    );
    assert!(step(&mut domain, limits, Event::Lost { channel: Token::new(0) }).is_empty());
    assert_eq!(domain.attempts(), 1, "the local host stays until restart");
    let mut restarted = Domain::new(&limits);
    assert_eq!(
        step(
            &mut restarted,
            limits,
            Event::Adopt {
                reply_to: ReplyTo::new(attempt),
                run: task,
                attempt,
                kept: 0,
                kind: HostKind::Engine,
                worked: false
            }
        ),
        vec![Request::NotStarted { to: ReplyTo::new(attempt), run: task, attempt }]
    );
    let mut restarted_with_work = Domain::new(&limits);
    assert_eq!(
        step(
            &mut restarted_with_work,
            limits,
            Event::Adopt {
                reply_to: ReplyTo::new(attempt),
                run: task,
                attempt,
                kept: 0,
                kind: HostKind::Engine,
                worked: true,
            }
        ),
        vec![Request::Lost { to: ReplyTo::new(attempt), run: task, attempt }]
    );
}

#[test]
fn a_charter_for_workers_only_waits_while_only_engine_slots_are_free() {
    let limits = Limits { workers: 1, engine_slots: 1, ..jig_fleet_world::LIMITS };
    let mut domain = Domain::new(&limits);
    let task = Token::new(3);
    let attempt = Token::new(4);
    assert!(
        step(
            &mut domain,
            limits,
            Event::Start { reply_to: ReplyTo::new(attempt), run: task, attempt, workstream: 3, kinds: Kinds::Workers }
        )
        .is_empty()
    );
    assert!(place(&mut domain, limits).is_empty());
    assert_eq!(domain.waiting(), 1);
    assert!(
        step(
            &mut domain,
            limits,
            Event::Hello {
                channel: Token::new(5),
                hello: Hello {
                    slots: 1,
                    graces: Some(Duration::from_secs(1)),
                    workstreams: Box::new([]),
                    hosting: Box::new([]),
                }
            }
        )
        .is_empty()
    );
    assert_eq!(
        place(&mut domain, limits),
        vec![
            Request::Assign { channel: Token::new(5), kind: HostKind::Worker, run: task, attempt },
            Request::Placed { run: task, attempt },
        ]
    );
}

#[test]
fn a_worker_holding_the_workstream_is_preferred_over_a_free_engine_slot() {
    let limits = Limits { workers: 1, engine_slots: 1, ..jig_fleet_world::LIMITS };
    let mut domain = Domain::new(&limits);
    let task = Token::new(9);
    let attempt = Token::new(10);
    assert!(
        step(
            &mut domain,
            limits,
            Event::Hello {
                channel: Token::new(11),
                hello: Hello {
                    slots: 1,
                    graces: Some(Duration::from_secs(1)),
                    workstreams: Box::new([9]),
                    hosting: Box::new([]),
                }
            }
        )
        .is_empty()
    );
    assert!(
        step(
            &mut domain,
            limits,
            Event::Start { reply_to: ReplyTo::new(attempt), run: task, attempt, workstream: 9, kinds: Kinds::Both }
        )
        .is_empty()
    );
    assert_eq!(
        place(&mut domain, limits),
        vec![
            Request::Assign { channel: Token::new(11), kind: HostKind::Worker, run: task, attempt },
            Request::Placed { run: task, attempt },
        ]
    );
    assert_eq!(
        step(
            &mut domain,
            limits,
            Event::Answer {
                channel: Token::new(11),
                run: task,
                attempt,
                answer: Answer::Busy,
                payload: Token::new(12),
            }
        ),
        vec![Request::Drop { payload: Token::new(12) }]
    );
    assert_eq!(
        place(&mut domain, limits),
        vec![
            Request::Assign { channel: Token::new(0), kind: HostKind::Engine, run: task, attempt },
            Request::Placed { run: task, attempt },
        ]
    );
}
