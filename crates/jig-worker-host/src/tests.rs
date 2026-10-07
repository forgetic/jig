//! Exercise hosted runs through the public boundary (domain/hosts.md, section 6).

use alloc::boxed::Box;
use alloc::vec::Vec;
use skein_lib::{Env, Queue, ReplyTo, Time, Token, Wall};

use crate::{
    Answer, Ask, Assignment, Delivery, DeliveryOutcome, Domain, Event, Failure, Finish, Invalid, Limits,
    Preparation, Reason, Refusal, Reply, Request, Workspace, max_out, step,
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
        let mut requests = Vec::new();
        while let Some(request) = self.out.pop() {
            requests.push(request);
        }
        requests.into_boxed_slice()
    }

    fn assign(&mut self, run: u64) -> (Token, Token) {
        let assignment = assignment(run);
        let requests = self.step(Event::Assign { reply_to: ReplyTo::new(assignment.run), assignment });
        let [Request::Prepare { owner, workspace }] = &*requests else { panic!("expected prepare: {requests:?}") };
        assert_eq!(*workspace, Workspace { workstream: run, items: Token::new(run + 100) });
        (*owner, Token::new(run + 200))
    }

    fn live(&mut self, run: u64) -> (Token, Token, Token) {
        let (owner, workspace) = self.assign(run);
        let requests = self.step(Event::Prepared { owner, workspace });
        let [Request::Start { owner: started, workspace: prepared, .. }] = &*requests else {
            panic!("expected start: {requests:?}")
        };
        assert_eq!((*started, *prepared), (owner, workspace));
        let agent = Token::new(run + 300);
        assert!(self.step(Event::Started { owner, agent }).is_empty());
        (owner, workspace, agent)
    }
}

fn assignment(run: u64) -> Assignment {
    Assignment {
        run: Token::new(run),
        attempt: Token::new(run + 1000),
        workspace: Workspace { workstream: run, items: Token::new(run + 100) },
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
    let [Request::Answer { answer: Answer::Failed { failure: Failure::Unprepared(Preparation::Permanent { resource: Some(resource) }), .. }, .. }] = &*requests else {
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
    let [Request::Release { workspace: released }, Request::Answer { answer: Answer::Ended { work, .. }, .. }] = &*requests else {
        panic!("expected release and answer: {requests:?}")
    };
    assert_eq!((*released, work.left, work.saved), (workspace, Some(left), None));
}

#[test]
fn cancelling_a_preparing_run_waits_for_the_workspace_terminal() {
    let mut h = Harness::new();
    let (owner, _) = h.assign(1);
    let requests = h.step(Event::Cancel { run: Token::new(1), attempt: Token::new(1001) });
    assert_eq!(&*requests, [Request::Abort { owner }]);
    let requests = h.step(Event::Unprepared { owner, failure: Preparation::Transient, detail: Box::new([]) });
    let [Request::Answer { answer: Answer::Failed { failure: Failure::Cancelled(Reason::Engine), .. }, .. }] = &*requests else {
        panic!("expected cancelled answer: {requests:?}")
    };
}
