//! Exercise hosted runs through the public boundary (domain/hosts.md, section 6).

use alloc::boxed::Box;
use skein_lib::{Env, List, Queue, ReplyTo, Time, Token, Wall};

use crate::{
    Answer, Ask, Assignment, Bounce, Delivery, DeliveryOutcome, Domain, Event, Failure, Finish, Invalid, Limits,
    Preparation, Reason, Refusal, Reply, Request, Told, Work, Workspace, max_out, step,
};

const LIMITS: Limits = Limits {
    slots: 2,
    accounts: 2,
    charter_bytes: 64,
    snapshot_bytes: 32,
    transcript_bytes: 64,
    turn_bytes: 64,
    outcome_bytes: 32,
    detail_bytes: 8,
    held: 2,
    event_bytes: 16,
    run_calls: 2,
    facts: 64,
    told: 2,
    fact_bytes: 16,
};

struct Harness {
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
}

impl Harness {
    fn new() -> Self {
        Self {
            domain: Domain::new(&LIMITS),
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS },
            out: Queue::with_capacity(max_out(&LIMITS)),
        }
    }

    fn step(&mut self, event: Event) -> Box<[Request]> {
        step(&mut self.domain, &self.env, event, &mut self.out);
        let mut requests = List::with_capacity(max_out(&LIMITS));
        while let Some(request) = self.out.pop() {
            requests.push(request).expect("bounded output");
        }
        requests.into_boxed()
    }

    fn assign(&mut self, run: u64) -> (Token, Token) {
        let assignment = assignment(run);
        let requests = self.step(Event::Assign { reply_to: ReplyTo::new(assignment.run), assignment });
        let [Request::Prepare { owner, workspace }] = &*requests else { panic!("expected prepare: {requests:?}") };
        assert_eq!(
            *workspace,
            Workspace { workstream: run, items: Token::new(run.checked_add(100).expect("small run")) }
        );
        (*owner, Token::new(run.checked_add(200).expect("small run")))
    }

    fn live(&mut self, run: u64) -> (Token, Token, Token) {
        let (owner, workspace) = self.assign(run);
        let requests = self.step(Event::Prepared { owner, workspace });
        let [Request::Start { owner: started, workspace: prepared, .. }] = &*requests else {
            panic!("expected start: {requests:?}")
        };
        assert_eq!((*started, *prepared), (owner, Some(workspace)));
        let agent = Token::new(run.checked_add(300).expect("small run"));
        assert!(self.step(Event::Started { owner, agent }).is_empty());
        (owner, workspace, agent)
    }
}

fn assignment(run: u64) -> Assignment {
    Assignment {
        run: Token::new(run),
        attempt: Token::new(run.checked_add(1000).expect("small run")),
        workspace: Some(Workspace { workstream: run, items: Token::new(run.checked_add(100).expect("small run")) }),
        save: true,
        charter: Box::from(&b"charter"[..]),
        snapshot: None,
        grants: Box::new([]),
    }
}

#[test]
fn a_run_prepares_and_starts_from_opaque_workspace_items() {
    let mut h = Harness::new();
    let (owner, workspace, agent) = h.live(1);
    assert_eq!(h.domain.hosting(owner).expect("run is hosted").run, Token::new(1));
    assert_eq!(workspace, Token::new(201));
    assert_eq!(agent, Token::new(301));
}

#[test]
fn an_itemless_run_starts_and_ends_without_a_workspace() {
    let mut h = Harness::new();
    let mut assignment = assignment(1);
    assignment.workspace = None;
    let run = assignment.run;
    let attempt = assignment.attempt;
    let requests = h.step(Event::Assign { reply_to: ReplyTo::new(run), assignment });
    let [Request::Start { owner, workspace: None, .. }] = &*requests else {
        panic!("an itemless run starts without preparation: {requests:?}");
    };
    let owner = *owner;
    let agent = Token::new(301);
    assert!(h.step(Event::Started { owner, agent }).is_empty());
    let call = Token::new(42);
    assert_eq!(
        &*h.step(Event::Called { owner, call, ask: Ask::Deliver { message: Box::from(&b"work"[..]) } }),
        [Request::Reply { agent, call, reply: Reply::Unavailable }]
    );
    assert_eq!(
        &*h.step(Event::Finished { owner, finish: Finish::Ended { outcome: Box::from(&b"done"[..]) } }),
        [Request::Stop { agent }]
    );
    let requests = h.step(Event::Gone { owner, detail: Box::new([]) });
    let [Request::Answer { run: answered, attempt: answered_attempt, answer: Answer::Ended { work, .. }, .. }] =
        &*requests
    else {
        panic!("the itemless run answers without workspace requests: {requests:?}")
    };
    assert_eq!((*answered, *answered_attempt, work.left, work.saved), (run, attempt, None, None));
}

#[test]
fn a_bad_charter_is_refused_before_a_workspace_is_touched() {
    let mut h = Harness::new();
    let mut assignment = assignment(1);
    assignment.charter = Box::from([0_u8; 65]);
    let requests = h.step(Event::Assign { reply_to: ReplyTo::new(assignment.run), assignment });
    let [Request::Answer { answer: Answer::Refused(Refusal::Invalid(Invalid::Charter)), .. }] = &*requests else {
        panic!("expected invalid refusal: {requests:?}")
    };
}

#[test]
fn a_preparation_failure_answers_without_starting_an_agent() {
    let mut h = Harness::new();
    let (owner, _) = h.assign(1);
    let requests = h.step(Event::Unprepared {
        owner,
        failure: Preparation::Permanent { resource: Some(Token::new(77)) },
        detail: Box::new([]),
    });
    let [
        Request::Answer {
            answer:
                Answer::Failed { failure: Failure::Unprepared(Preparation::Permanent { resource: Some(resource) }), .. },
            ..
        },
    ] = &*requests
    else {
        panic!("expected typed preparation failure: {requests:?}")
    };
    assert_eq!(*resource, Token::new(77));
}

#[test]
fn a_delivery_is_answered_after_the_workspace_reports_what_it_left() {
    let mut h = Harness::new();
    let (owner, workspace, agent) = h.live(1);
    let call = Token::new(8);
    let requests = h.step(Event::Called { owner, call, ask: Ask::Deliver { message: Box::from(&b"ship"[..]) } });
    let [Request::DeliverWorkspace { owner: delivery, workspace: target, .. }] = &*requests else {
        panic!("expected workspace delivery: {requests:?}")
    };
    assert_eq!(*target, workspace);
    let left = Token::new(900);
    let delivered = Delivery { outcome: DeliveryOutcome::Delivered, left, changed: true };
    let requests = h.step(Event::Delivered { owner: *delivery, delivery: delivered });
    assert_eq!(&*requests, [Request::Reply { agent, call, reply: Reply::Delivered(delivered) }]);
    let requests = h.step(Event::Finished { owner, finish: Finish::Ended { outcome: Box::from(&b"done"[..]) } });
    assert_eq!(&*requests, [Request::Stop { agent }]);
    let requests = h.step(Event::Gone { owner, detail: Box::new([]) });
    let [Request::Release { workspace: released }, Request::Answer { answer: Answer::Ended { work, .. }, .. }] =
        &*requests
    else {
        panic!("expected release and answer: {requests:?}")
    };
    assert_eq!((*released, work.left, work.saved), (workspace, Some(left), None));
}

#[test]
fn stopping_waits_for_a_delivery_even_after_the_agent_is_gone() {
    let mut h = Harness::new();
    let (owner, workspace, agent) = h.live(1);
    let call = Token::new(42);
    let requests = h.step(Event::Called { owner, call, ask: Ask::Deliver { message: Box::from(&b"ship"[..]) } });
    let [Request::DeliverWorkspace { owner: delivery, .. }] = &*requests else {
        panic!("delivery is in flight: {requests:?}")
    };
    let delivery = *delivery;
    assert_eq!(
        &*h.step(Event::Finished { owner, finish: Finish::Ended { outcome: Box::from(&b"done"[..]) } }),
        [Request::Stop { agent }]
    );
    assert!(h.step(Event::Gone { owner, detail: Box::new([]) }).is_empty());
    let changed = Delivery { outcome: DeliveryOutcome::Delivered, left: Token::new(900), changed: true };
    let requests = h.step(Event::Delivered { owner: delivery, delivery: changed });
    let [
        Request::Reply { agent: answered_agent, call: answered_call, reply: Reply::Delivered(outcome) },
        Request::Release { workspace: released },
        Request::Answer { answer: Answer::Ended { work, .. }, .. },
    ] = &*requests
    else {
        panic!("the delivery settles before release and answer: {requests:?}")
    };
    assert_eq!((*answered_agent, *answered_call, *outcome), (agent, call, changed));
    assert_eq!((*released, work.left), (workspace, Some(changed.left)));
}

#[test]
fn live_agent_facts_wait_within_the_hosts_bound_and_drop_when_full() {
    let mut h = Harness::new();
    let (owner, _, _) = h.live(1);
    assert!(h.step(Event::Facts { owner, fact: Box::from(&b"one"[..]) }).is_empty());
    assert!(h.step(Event::Facts { owner, fact: Box::from(&b"two"[..]) }).is_empty());
    assert!(h.step(Event::Facts { owner, fact: Box::from(&b"three"[..]) }).is_empty());
    assert_eq!(h.domain.told_lost(), 1);
    assert_eq!(
        h.domain.pop_told(),
        Some(Told { run: Token::new(1), attempt: Token::new(1001), fact: Box::from(&b"one"[..]) })
    );
    assert_eq!(
        h.domain.pop_told(),
        Some(Told { run: Token::new(1), attempt: Token::new(1001), fact: Box::from(&b"two"[..]) })
    );
    assert_eq!(h.domain.pop_told(), None);
}

#[test]
fn cancelling_a_preparing_run_waits_for_the_workspace_terminal() {
    let mut h = Harness::new();
    let (owner, _) = h.assign(1);
    let requests = h.step(Event::Cancel { run: Token::new(1), attempt: Token::new(1001) });
    assert_eq!(&*requests, [Request::Abort { owner }]);
    let requests = h.step(Event::Unprepared { owner, failure: Preparation::Transient, detail: Box::new([]) });
    let [Request::Answer { answer: Answer::Failed { failure: Failure::Cancelled(Reason::Engine), .. }, .. }] =
        &*requests
    else {
        panic!("expected cancelled answer: {requests:?}")
    };
}

#[test]
fn messages_held_during_a_failed_prepare_are_returned_by_name() {
    let mut h = Harness::new();
    let (owner, _) = h.assign(1);
    let run = Token::new(1);
    let attempt = Token::new(1001);
    let name = Token::new(42);
    assert!(h.step(Event::Inbound { run, attempt, name, event: Box::from(&b"question"[..]) }).is_empty());
    let requests = h.step(Event::Unprepared { owner, failure: Preparation::Transient, detail: Box::new([]) });
    assert_eq!(
        &*requests,
        [
            Request::Bounced { run, attempt, name, bounce: Bounce::Ending },
            Request::Answer {
                to: ReplyTo::new(run),
                run,
                attempt,
                answer: Answer::Failed {
                    failure: Failure::Unprepared(Preparation::Transient),
                    detail: Box::new([]),
                    work: Work { left: None, saved: None },
                },
            },
        ]
    );
}

#[test]
fn cancellation_returns_messages_held_before_the_agent_starts() {
    let mut h = Harness::new();
    let (owner, workspace) = h.assign(1);
    let run = Token::new(1);
    let attempt = Token::new(1001);
    let name = Token::new(42);
    let requests = h.step(Event::Prepared { owner, workspace });
    let [Request::Start { .. }] = &*requests else { panic!("the agent starts: {requests:?}") };
    assert!(h.step(Event::Inbound { run, attempt, name, event: Box::from(&b"question"[..]) }).is_empty());
    assert_eq!(
        &*h.step(Event::Cancel { run, attempt }),
        [Request::Bounced { run, attempt, name, bounce: Bounce::Ending }]
    );
    let agent = Token::new(301);
    assert_eq!(&*h.step(Event::Started { owner, agent }), [Request::Stop { agent }]);
    let requests = h.step(Event::Gone { owner, detail: Box::new([]) });
    assert_eq!(&*requests, [Request::Save { owner, workspace }]);
    let requests = h.step(Event::Saved { owner, at: None });
    let [Request::Release { workspace: released }, Request::Answer { answer: Answer::Failed { failure, .. }, .. }] =
        &*requests
    else {
        panic!("the run releases and answers after save: {requests:?}")
    };
    assert_eq!((*released, *failure), (workspace, Failure::Cancelled(Reason::Engine)));
}
