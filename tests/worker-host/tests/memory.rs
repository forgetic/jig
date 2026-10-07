//! Memory stays within the worst case (programming-model.md, 6.3), measured by
//! a counting allocator: the host with every slot holding an assignment of
//! exactly its limits, then every run ending with as much as it may hold.

use jig_worker_host::{
    AgentFailure, Answer, Ask, Assignment, Bounce, Delivery, DeliveryOutcome, Domain, Event, Finish, Grant, Invalid,
    Limits, Preparation, Reason, Refusal, Request, Workspace, max_out, resume, step, worst_case,
};
use skein_lib::{Duration, Env, Queue, ReplyTo, Time, Token, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

const LIMITS: Limits = Limits {
    accounts: 4,
    slots: 2,
    charter_bytes: 1024,
    snapshot_bytes: 512,
    transcript_bytes: 0,
    turn_bytes: 0,
    outcome_bytes: 768,
    detail_bytes: 128,
    held: 2,
    event_bytes: 256,
    run_calls: 2,
    facts: 16,
};

fn bytes(len: u64) -> Box<[u8]> {
    vec![b'x'; usize::try_from(len).expect("a test length fits")].into_boxed_slice()
}

/// What a step asked for, without the payload.
#[derive(PartialEq, Eq, Debug)]
enum Asked {
    Prepare { owner: Token },
    Start,
    Delivery { owner: Token },
    Relay { call: Token },
    Save { owner: Token },
    Answer { answer: Answer },
    AnswerV2 { answer: jig_worker_host::AnswerV2 },
    Turn,
    Bounced { bounce: Bounce },
    Hosting { runs: usize },
    Other,
}

/// The host under `limits`, measured: each step's peak is checked against
/// the worst case, less what it handed out in requests, which their
/// receivers count.
struct Measured {
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
    meter: Meter,
    bound: u64,
}

impl Measured {
    fn new(limits: Limits) -> Measured {
        let bound = worst_case(&limits).expect("the test limits fit");
        // The output queue belongs to the parent, not the measured child.
        let out = Queue::with_capacity(max_out(&limits));
        let meter = Meter::new();
        let domain = Domain::new(&limits);
        Measured { domain, env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits }, out, meter, bound }
    }

    fn step(&mut self, event: Event) -> Vec<Asked> {
        self.meter.start();
        step(&mut self.domain, &self.env, event, &mut self.out);
        self.drain()
    }

    fn resume(&mut self) -> Vec<Asked> {
        self.meter.start();
        resume(&mut self.domain, &self.env, &mut self.out);
        self.drain()
    }

    fn drain(&mut self) -> Vec<Asked> {
        let measured = self.meter.end();
        let mut asked = Vec::new();
        let mut cancelled = Vec::new();
        while let Some(request) = self.out.pop() {
            asked.push(match request {
                Request::Turn { .. } => Asked::Turn,
                Request::DeliverV2 { owner, .. } | Request::DeliverWorkspace { owner, .. } => Asked::Delivery { owner },
                Request::RelayV2 { delivery, .. } => Asked::Relay { call: delivery },
                Request::AnswerV2 { answer, .. } => Asked::AnswerV2 { answer },
                Request::StartV2 { .. } | Request::Start { .. } => Asked::Start,

                Request::Prepare { owner, .. } => Asked::Prepare { owner },
                Request::Relay { call, .. } => Asked::Relay { call },
                Request::Save { owner, .. } => Asked::Save { owner },
                Request::Answer { answer, .. } => Asked::Answer { answer },
                Request::Bounced { bounce, .. } => Asked::Bounced { bounce },
                Request::Hosting { runs } => Asked::Hosting { runs: runs.len() },
                Request::CancelRelay { call } => {
                    cancelled.push(call);
                    continue;
                }
                Request::Abort { .. }
                | Request::Deliver { .. }
                | Request::Reply { .. }
                | Request::Stop { .. }
                | Request::Release { .. }
                | Request::Grant { .. } => Asked::Other,
            });
        }
        self.meter.check(measured, self.bound, &self.env.limits);
        // The iteration ends: the reclaim point.
        self.domain.reclaim();
        for call in cancelled {
            asked.extend(self.step(Event::RelayCancelled { call }));
        }
        asked
    }
}

/// An assignment for `run` of exactly the limits: a charter, a snapshot and
/// names of their limits, as many items as a workspace may list.
fn assignment(run: u64, limits: &Limits) -> Assignment {
    Assignment {
        grants: (0..limits.accounts)
            .map(|account| Grant { account, generation: 7, valid: Duration::from_secs(300) })
            .collect(),
        run: Token::new(run),
        attempt: Token::new(run + 1000),
        workspace: Workspace { workstream: run, items: Token::new(run + 2000) },
        save: true,
        charter: bytes(limits.charter_bytes),
        snapshot: Some(bytes(limits.snapshot_bytes)),
    }
}

/// Fills every slot with an assignment of exactly the limits and a full hold
/// of inbound events of exactly their limit; starts each run, has it land a
/// change in every item and make as many calls as it may, the last a
/// delivery still in flight as it ends with an outcome of exactly the limit; and
/// takes each through its tail. Every step's peak is checked against the
/// worst case.
fn fill(limits: Limits) {
    let delivered = Delivery { outcome: DeliveryOutcome::Delivered, left: Token::new(5000), changed: true };
    let mut host = Measured::new(limits);
    let mut owners = Vec::new();
    for run in 0..u64::from(limits.slots) {
        let [Asked::Prepare { owner }] = host
            .step(Event::Assign { reply_to: ReplyTo::new(Token::new(run)), assignment: assignment(run, &limits) })[..]
        else {
            panic!("an assignment of exactly the limits is admitted");
        };
        for _ in 0..limits.held {
            let event = Event::Inbound {
                name: Token::new(1),
                run: Token::new(run),
                attempt: Token::new(run + 1000),
                event: bytes(limits.event_bytes),
            };
            assert!(host.step(event).is_empty(), "held");
        }
        owners.push(owner);
    }
    let held = host.meter.held();
    let starting = limits.charter_bytes + limits.snapshot_bytes + u64::from(limits.held) * limits.event_bytes;
    assert!(held >= u64::from(limits.slots) * starting, "{limits:?}: every slot holds what it may as it prepares");

    let mut pending = Vec::new();
    for (index, owner) in owners.iter().enumerate() {
        let index = u64::try_from(index).expect("fits");
        let workspace = Token::new(index);
        assert_eq!(host.step(Event::Prepared { owner: *owner, workspace }), [Asked::Start]);
        let agent = Token::new(index);
        assert_eq!(
            host.step(Event::Started { owner: *owner, agent }).len(),
            usize::try_from(limits.held).expect("fits")
        );
        assert!(host.step(Event::Yielded { owner: *owner }).is_empty(), "waiting");
        let [Asked::Delivery { owner: delivery }] = host.step(Event::Called {
            owner: *owner,
            call: Token::new(1),
            ask: Ask::Deliver { message: bytes(64) },
        })[..] else {
            panic!("delivered");
        };
        assert_eq!(
            host.step(Event::Delivered { owner: delivery, delivery: delivered }),
            [Asked::Other],
            "landed everywhere"
        );
        let mut calls = Vec::new();
        for call in 2..=u64::from(limits.run_calls) {
            let ask = Ask::Relay { body: bytes(64) };
            let [Asked::Relay { call }] = host.step(Event::Called { owner: *owner, call: Token::new(call), ask })[..]
            else {
                panic!("relayed");
            };
            calls.push(call);
        }
        // The first relayed call is answered, and a delivery takes its place.
        if let Some(call) = calls.first() {
            let relayed = Event::Relayed {
                run: Token::new(index),
                attempt: Token::new(index + 1000),
                call: *call,
                answer: bytes(64),
            };
            assert_eq!(host.step(relayed), [Asked::Other]);
        }
        let delivery = if limits.run_calls > 1 {
            let ask = Ask::Deliver { message: bytes(64) };
            let [Asked::Delivery { owner: delivery }] =
                host.step(Event::Called { owner: *owner, call: Token::new(99), ask })[..]
            else {
                panic!("delivered again");
            };
            Some(delivery)
        } else {
            None
        };
        let finish = Finish::Ended { outcome: bytes(limits.outcome_bytes) };
        assert!(!host.step(Event::Finished { owner: *owner, finish }).is_empty(), "stopped");
        assert!(host.step(Event::Faulted { owner: *owner, fault: AgentFailure::WallTime }).is_empty(), "decided");
        let detail = bytes(u64::from(limits.detail_bytes) + 1);
        let gone = host.step(Event::Gone { owner: *owner, detail });
        if let Some(delivery) = delivery {
            assert!(gone.is_empty(), "the delivery in flight is waited for");
            pending.push(delivery);
        }
    }
    if limits.run_calls > 1 {
        let held = host.meter.held();
        assert!(held >= u64::from(limits.slots) * limits.outcome_bytes, "{limits:?}: every run holds its outcome");
    }
    for delivery in pending {
        owners_push(&mut host, delivery, delivered);
    }
    assert_eq!(host.domain.hosted(), 0, "every slot came back");

    beyond(&mut host, &limits);
}

/// Entrance refusals consume the supplied payload without retaining it.
fn beyond(host: &mut Measured, limits: &Limits) {
    // Beyond the limits: refused, and nothing held.
    let mut beyond = assignment(0, limits);
    beyond.charter = bytes(limits.charter_bytes + 1);
    let refused = host.step(Event::Assign { reply_to: ReplyTo::new(Token::new(0)), assignment: beyond });
    assert_eq!(refused, [Asked::Answer { answer: Answer::Refused(Refusal::Invalid(Invalid::Charter)) }]);
    let [Asked::Prepare { owner: _ }] =
        host.step(Event::Assign { reply_to: ReplyTo::new(Token::new(0)), assignment: assignment(0, limits) })[..]
    else {
        panic!("admitted");
    };
    let large = Event::Inbound {
        name: Token::new(1),
        run: Token::new(0),
        attempt: Token::new(1000),
        event: bytes(limits.event_bytes + 1),
    };
    assert_eq!(host.step(large), [Asked::Bounced { bounce: Bounce::TooLarge }]);
}

/// A delivery still in flight as its run ended settles, and the run answers.
fn owners_push(host: &mut Measured, call: Token, delivery: Delivery) {
    let settled = host.step(Event::Delivered { owner: call, delivery });
    let [Asked::Other, Asked::Other, Asked::Answer { answer: Answer::Ended { .. } }] = &settled[..] else {
        panic!("told how it went, released and answered: {settled:?}");
    };
}

/// Every other entry point, each measured: a report, cancels of every run and
/// of one, a prepare that fails, a save, and a fault.
fn paths(limits: Limits) {
    let mut host = Measured::new(limits);
    let mut owners = Vec::new();
    for run in 0..u64::from(limits.slots) {
        let [Asked::Prepare { owner }] = host
            .step(Event::Assign { reply_to: ReplyTo::new(Token::new(run)), assignment: assignment(run, &limits) })[..]
        else {
            panic!("admitted");
        };
        owners.push(owner);
    }
    let reported = host.step(Event::Report);
    assert_eq!(reported, [Asked::Hosting { runs: usize::try_from(limits.slots).expect("fits") }]);
    let first = owners[0];
    let unprepared = Event::Unprepared {
        owner: first,
        failure: Preparation::Permanent { resource: Some(Token::new(7)) },
        detail: bytes(u64::from(limits.detail_bytes) + 1),
    };
    assert_eq!(host.step(unprepared).len(), 1, "answered");
    for owner in owners.iter().skip(1) {
        assert_eq!(host.step(Event::Prepared { owner: *owner, workspace: *owner }), [Asked::Start]);
        assert!(host.step(Event::Started { owner: *owner, agent: *owner }).is_empty());
    }
    assert!(host.step(Event::CancelAll { reason: Reason::Contact }).is_empty());
    while host.domain.is_ready() {
        host.resume();
    }
    for (index, owner) in owners.iter().enumerate().skip(1) {
        let index = u64::try_from(index).expect("fits");
        let cancel = Event::Cancel { run: Token::new(index), attempt: Token::new(index + 1000) };
        assert!(host.step(cancel).is_empty(), "decided");
        let [Asked::Save { owner: saving }] = host.step(Event::Gone { owner: *owner, detail: bytes(10) })[..] else {
            panic!("saved");
        };
        let saved = host.step(Event::Saved { owner: saving, at: Some(Token::new(7)) });
        assert_eq!(saved.len(), 2, "released and answered");
    }
    // A fault, from a run that had started.
    let [Asked::Prepare { owner }] =
        host.step(Event::Assign { reply_to: ReplyTo::new(Token::new(0)), assignment: assignment(0, &limits) })[..]
    else {
        panic!("admitted");
    };
    assert_eq!(host.step(Event::Prepared { owner, workspace: owner }), [Asked::Start]);
    assert!(host.step(Event::Started { owner, agent: owner }).is_empty());
    assert_eq!(host.step(Event::Faulted { owner, fault: AgentFailure::NoProgress }).len(), 1, "stopped");
    assert_eq!(host.step(Event::Gone { owner, detail: bytes(10) }).len(), 1, "saved");
}

#[test]
fn a_host_with_every_slot_full_stays_within_its_worst_case() {
    fill(LIMITS);
    fill(Limits { slots: 16, held: 8, run_calls: 4, ..LIMITS });
    fill(Limits { slots: 200, charter_bytes: 65_536, snapshot_bytes: 16_384, event_bytes: 4096, ..LIMITS });
    fill(Limits { run_calls: 1, held: 0, ..LIMITS });
    // The outcome dominates what a run holds: the ending's side of the max.
    fill(Limits { charter_bytes: 16, snapshot_bytes: 16, held: 0, outcome_bytes: 4096, run_calls: 4, ..LIMITS });
}

#[test]
fn every_entry_point_stays_within_the_worst_case() {
    paths(LIMITS);
    paths(Limits { slots: 8, ..LIMITS });
}

#[test]
fn v2_full_transcripts_and_delivery_feedback_fit_the_hosts_bound() {
    use jig_worker_host::{AssignmentV2, EndingV2, FinishV2, Turn};
    let limits = Limits { transcript_bytes: 2048, turn_bytes: 128, ..LIMITS };
    let mut owners = Vec::with_capacity(usize::try_from(limits.slots).expect("bounded"));
    let mut host = Measured::new(limits);
    for run in 0..u64::from(limits.slots) {
        let mut assignment = assignment(run, &limits);
        assignment.snapshot = None;
        let next = AssignmentV2 { assignment, transcript: Some(bytes(limits.transcript_bytes)) };
        let [Asked::Prepare { owner }] =
            host.step(Event::AssignV2 { reply_to: ReplyTo::new(Token::new(run)), assignment: next })[..]
        else {
            panic!("v2 prepare")
        };
        owners.push(owner);
    }
    assert!(host.meter.held() >= u64::from(limits.slots) * (limits.charter_bytes + limits.transcript_bytes));
    for owner in owners {
        assert_eq!(host.step(Event::Prepared { owner, workspace: owner }), [Asked::Start]);
        assert!(host.step(Event::Started { owner, agent: owner }).is_empty());
        let [Asked::Delivery { owner: call }] = host.step(Event::Called {
            owner,
            call: Token::new(51),
            ask: Ask::DeliverV2 { title: bytes(9), body: bytes(23) },
        })[..] else {
            panic!("v2 delivery")
        };
        let delivery = Delivery { outcome: DeliveryOutcome::Refused, left: Token::new(7), changed: false };
        assert_eq!(host.step(Event::Delivered { owner: call, delivery }), [Asked::Other]);
        assert_eq!(
            host.step(Event::Turn {
                owner,
                turn: Turn { turn: 1, spent: 23, read: None, body: bytes(limits.turn_bytes) }
            }),
            [Asked::Turn]
        );
        host.step(Event::FinishedV2 { owner, turns: 1, spent: 29, finish: FinishV2::Parked });
        let [Asked::Save { owner: saving }] =
            host.step(Event::Gone { owner, detail: bytes(u64::from(limits.detail_bytes)) })[..]
        else {
            panic!("save ordinary unfinished work")
        };
        let asked = host.step(Event::Saved { owner: saving, at: Some(Token::new(7)) });
        let [Asked::Other, Asked::AnswerV2 { answer }] = &*asked else { panic!("snapshot-free answer: {asked:?}") };
        assert_eq!((answer.turns, answer.spent), (1, 29));
        let EndingV2::Parked { work } = &answer.ending else { panic!("parked") };
        assert!(work.saved.is_some());
    }
    assert_eq!(host.domain.hosted(), 0);
}
