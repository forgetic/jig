//! Entry-point checks for slot admission, Smith translation and cancellation.

use alloc::boxed::Box;
use skein_lib::{Duration, Env, List, Queue, Time, Wall};
use smith_domain as smith;

use crate::{
    Assignment, Budget, Charter, Completion, Contract, Event, Grant, Host, Limits, Model, Prices, Refusal, Request,
    TextRule, fire, max_out, step,
};

fn limits() -> Limits {
    let smith = smith_agent_world::Settings::calm(17).limits;
    let largest = smith::max_turn_bytes(&smith).expect("Smith's bounded turn");
    Limits {
        slots: 2,
        smith,
        window: smith::Window { turns: 2, bytes: largest.checked_mul(2).expect("two bounded turns") },
        cancel_grace: Duration::from_secs(1),
    }
}

fn assignment(task: u64, attempt: u32) -> Assignment {
    Assignment {
        task,
        attempt,
        charter: Charter {
            instructions: b"Answer the task".as_slice().into(),
            tools: Box::new([]),
            wait: true,
            agents: false,
            workspace: crate::WorkspaceTools { inspect: false, modify: false, shell: false },
            conventions: None,
            contract: Contract {
                report: Some(TextRule { max: 128, fields: Box::new([]) }),
                failure: Some(TextRule { max: 128, fields: Box::new([]) }),
                verdicts: Box::new([]),
                change: None,
            },
            budget: Budget { turns: 4, spend: 1, time: Duration::from_secs(100) },
            model: Model {
                prices: Prices { input: 0, cached: 0, output: 0, unit: 1 },
                dialect: 0,
                account: 0,
                endpoint: 0,
                name: b"fake-1".as_slice().into(),
                max_tokens: 100,
            },
            models: Box::new([]),
            waiting: Duration::from_secs(1),
            resumes: true,
        },
        brief: Box::new([]),
        transcript: None,
        calls: Box::new([]),
        grants: Box::new([Grant { account: 0, generation: 1, valid: Duration::from_secs(7200) }]),
    }
}

struct Harness {
    host: Host,
    env: Env<Limits>,
    out: Queue<Request>,
}

impl Harness {
    fn new() -> Harness {
        let limits = limits();
        Harness {
            host: Host::new(&limits, Box::new([smith::run::charter::Endpoint(0)]), 17),
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            out: Queue::with_capacity(max_out(&limits)),
        }
    }

    fn step(&mut self, event: Event) -> Box<[Request]> {
        step(&mut self.host, &self.env, event, &mut self.out);
        self.drain()
    }

    fn drain(&mut self) -> Box<[Request]> {
        let mut requests = List::with_capacity(max_out(&self.env.limits));
        for _ in 0..max_out(&self.env.limits) {
            match self.out.pop() {
                Some(request) => requests.push(request).expect("reserved output room"),
                None => break,
            }
        }
        requests.into_boxed()
    }
}

#[test]
fn two_engine_slots_admit_independent_runs_and_hold_a_busy_slot() {
    let mut harness = Harness::new();
    let first = harness.step(Event::Assign { slot: 0, assignment: Box::new(assignment(10, 1)) });
    match first.as_ref() {
        [
            Request::Admitted { task: 10, attempt: 1 },
            Request::Protocol { task: 10, request: smith::Request::Complete { .. }, .. },
        ] => {}
        other => panic!("expected admission and completion: {other:?}"),
    }
    let second = harness.step(Event::Assign { slot: 1, assignment: Box::new(assignment(11, 1)) });
    match second.as_ref() {
        [
            Request::Admitted { task: 11, attempt: 1 },
            Request::Protocol { task: 11, request: smith::Request::Complete { .. }, .. },
        ] => {}
        other => panic!("expected second admission and completion: {other:?}"),
    }
    assert_eq!(harness.host.hosted(), 2);
    assert_eq!(
        harness.step(Event::Assign { slot: 0, assignment: Box::new(assignment(12, 1)) }).as_ref(),
        &[Request::Refused { task: 12, attempt: 1, why: Refusal::Busy }]
    );
}

#[test]
fn cancelled_run_that_does_not_answer_is_stopped_after_the_grace() {
    let mut harness = Harness::new();
    let started = harness.step(Event::Assign { slot: 0, assignment: Box::new(assignment(10, 1)) });
    let owner = match started.as_ref() {
        [Request::Admitted { .. }, Request::Protocol { request: smith::Request::Complete { owner, .. }, .. }] => *owner,
        other => panic!("expected provider request: {other:?}"),
    };
    let cancelled = harness.step(Event::Cancel { task: 10, attempt: 1 });
    harness.env.now = Time::from_nanos(2_000_000_000);
    fire(&mut harness.host, &harness.env, &mut harness.out);
    let stopped = harness.drain();
    assert!(cancelled.is_empty(), "Smith still waits on its provider completion");
    match stopped.as_ref() {
        [
            Request::Protocol { task: 10, attempt: 1, request: smith::Request::Cancel { .. } },
            Request::Stopped { task: 10, attempt: 1 },
        ] => {}
        other => panic!("expected provider cancellation and stopped answer: {other:?}"),
    }
    assert_eq!(harness.host.hosted(), 1);
    assert!(
        harness.step(Event::Completion { task: 10, attempt: 1, terminal: Completion::Cancelled { owner } }).is_empty()
    );
    drop(harness.step(Event::AcknowledgeAnswer { task: 10, attempt: 1 }));
    assert_eq!(harness.host.hosted(), 0);
    assert!(
        harness.step(Event::Completion { task: 10, attempt: 1, terminal: Completion::Cancelled { owner } }).is_empty()
    );
}
