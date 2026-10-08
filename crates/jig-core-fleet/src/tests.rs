//! Feed the domain events, inspect the requests that come out.

use alloc::boxed::Box;

use skein_lib::{Duration, Env, List, Queue, ReplyTo, Time, Token, Wall};

use crate::{
    Answer, Assignment, Bounce, Call, Domain, Event, Fact, Hello, HostKind, Hosted, Kinds, Limits, Message, Phase,
    Refusal, Request, Undelivered, Withdrawal, fire, max_out, resume, step, worst_case,
};

const LIMITS: Limits = Limits {
    workers: 3,
    engine_slots: 0,
    slots: 2,
    workstreams: 2,
    attempts: 6,
    calls: 2,
    call_name_bytes: 64,
    turns: 0,
    grace: Duration::from_secs(10),
    facts: 64,
};

const C1: Token = Token::new(101);
const C2: Token = Token::new(102);
const C3: Token = Token::new(103);
const C4: Token = Token::new(104);

const R1: Token = Token::new(1);
const R2: Token = Token::new(2);
const R3: Token = Token::new(3);
const A1: Token = Token::new(11);
const A2: Token = Token::new(12);
const A3: Token = Token::new(13);

/// The parent's call for a run's attempt: one per attempt in these tests.
const fn to(attempt: Token) -> ReplyTo {
    ReplyTo::new(attempt)
}

/// A payload of the parent's.
const fn payload(raw: u64) -> Token {
    Token::new(raw.checked_add(1000).unwrap())
}

/// The domain, its environment, and room for one step's output.
struct Harness {
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
}

impl Harness {
    fn new(limits: Limits) -> Harness {
        let out = Queue::with_capacity(max_out(&limits));
        Harness { domain: Domain::new(&limits), env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits }, out }
    }

    /// Steps `event`, returning what it emitted, oldest first.
    fn step(&mut self, event: Event) -> Box<[Request]> {
        step(&mut self.domain, &self.env, event, &mut self.out);
        self.drain()
    }

    /// Resumes placement, returning what it emitted.
    fn resume(&mut self) -> Box<[Request]> {
        assert!(self.domain.is_ready(), "placement is ready");
        resume(&mut self.domain, &self.env, &mut self.out);
        self.drain()
    }

    /// Resumes placement while it is ready, returning what it emitted.
    fn settle(&mut self) -> Box<[Request]> {
        let mut requests = List::with_capacity(16);
        for _ in 0_u32..8 {
            if !self.domain.is_ready() {
                break;
            }
            for request in self.resume() {
                requests.push(request).unwrap();
            }
        }
        assert!(!self.domain.is_ready(), "placement settles");
        requests.into_boxed()
    }

    /// Moves the clock to `secs` and fires every alarm due, returning what
    /// they emitted.
    fn at(&mut self, secs: u64) -> Box<[Request]> {
        self.env.now = Time::ZERO.saturating_add(Duration::from_secs(secs));
        let mut requests = List::with_capacity(16);
        for _ in 0_u32..8 {
            if !self.domain.is_due(self.env.now) {
                break;
            }
            fire(&mut self.domain, &self.env, &mut self.out);
            for request in self.drain() {
                requests.push(request).unwrap();
            }
        }
        assert!(!self.domain.is_due(self.env.now), "every alarm due fired");
        requests.into_boxed()
    }

    fn drain(&mut self) -> Box<[Request]> {
        let mut requests = List::with_capacity(self.out.len());
        for _ in 0..self.out.len() {
            requests.push(self.out.pop().unwrap()).unwrap();
        }
        // The iteration ends: the reclaim point.
        self.domain.reclaim();
        requests.into_boxed()
    }

    /// A worker says hello on `channel`, with `slots`, holding `workstreams`
    /// and hosting `hosting`.
    fn hello(&mut self, channel: Token, slots: u32, workstreams: &[u64], hosting: &[Hosted]) -> Box<[Request]> {
        let mut keys = List::with_capacity(u32::try_from(workstreams.len()).unwrap());
        for key in workstreams {
            keys.push(*key).unwrap();
        }
        let hello = Hello {
            stop_bound: Duration::from_secs(0),
            slots,
            workstreams: keys.into_boxed(),
            hosting: Box::from(hosting),
        };
        self.step(Event::Hello { channel, hello })
    }

    fn start(&mut self, run: Token, attempt: Token, workstream: u64) -> Box<[Request]> {
        self.step(Event::Start {
            reply_to: to(attempt),
            run,
            attempt,
            workstream,
            kinds: Kinds::Workers,
            assignment: references(),
        })
    }

    fn adopt(&mut self, run: Token, attempt: Token) -> Box<[Request]> {
        self.step(Event::Adopt { reply_to: to(attempt), run, attempt, kept: 0, kind: HostKind::Worker, worked: false })
    }

    /// Starts `run`'s `attempt` and has it placed, on the worker the fleet
    /// chooses; returns that worker's channel.
    fn place(&mut self, run: Token, attempt: Token, workstream: u64) -> Token {
        assert!(self.start(run, attempt, workstream).is_empty(), "a start waits for placement");
        let placed = self.settle();
        let [
            Request::Assign { channel, kind: _, run: assigned, attempt: of, .. },
            Request::Placed { run: told, attempt: told_of },
        ] = &*placed
        else {
            panic!("placed: {placed:?}");
        };
        assert_eq!((*assigned, *of, *told, *told_of), (run, attempt, run, attempt));
        *channel
    }

    /// The run's call `call` with the body `raw`, relayed: the parent's
    /// right to answer it.
    fn relay(&mut self, run: Token, attempt: Token, call_name: Token, raw: u64) -> ReplyTo {
        let mut up =
            self.step(Event::Relay { channel: C1, run, attempt, call: host_call(call_name, payload(raw)) }).into_iter();
        let Some(Request::Relay { reply_to, run: of, attempt: by, call }) = up.next() else {
            panic!("a call is relayed up");
        };
        assert_eq!((of, by, call), (run, attempt, host_call(call_name, payload(raw))));
        assert!(up.next().is_none(), "a relay is one request");
        reply_to
    }

    fn answer(&mut self, channel: Token, run: Token, attempt: Token, answer: Answer, raw: u64) -> Box<[Request]> {
        self.step(Event::Answer { channel, run, attempt, answer, payload: payload(raw) })
    }

    /// The parent's answer to an `Answered`: durable, or not wanted.
    fn acknowledge(&mut self, run: Token, attempt: Token) -> Box<[Request]> {
        self.step(Event::Acknowledge { run, attempt })
    }

    /// The parent's cold read is done.
    fn loaded(&mut self) -> Box<[Request]> {
        self.step(Event::Loaded)
    }

    fn facts(&mut self) -> Box<[Fact]> {
        let mut facts = List::with_capacity(64);
        while let Some(fact) = self.domain.pop_fact() {
            facts.push(fact).unwrap();
        }
        facts.into_boxed()
    }
}

fn hosted(run: Token, attempt: Token, phase: Phase) -> Hosted {
    Hosted { run, attempt, phase }
}

fn answered(attempt: Token, run: Token, answer: Answer, raw: u64) -> Request {
    Request::Answered { to: to(attempt), run, attempt, answer, payload: payload(raw) }
}

fn ack(channel: Token, run: Token, attempt: Token) -> Request {
    Request::Acknowledge { channel, run, attempt }
}

fn cancel(channel: Token, run: Token, attempt: Token) -> Request {
    Request::Cancel { channel, run, attempt }
}

fn assign(channel: Token, run: Token, attempt: Token) -> Request {
    Request::Assign {
        channel,
        kind: HostKind::Worker,
        run,
        attempt,
        activation: attempt.raw(),
        assignment: references(),
    }
}

fn placed(run: Token, attempt: Token) -> Request {
    Request::Placed { run, attempt }
}

#[test]
fn a_assignment_keeps_its_turn_and_answer_references_through_placement() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    let assignment = Assignment { turns: payload(1), answered: payload(2) };
    assert!(
        h.step(Event::Start {
            reply_to: to(A1),
            run: R1,
            attempt: A1,
            workstream: 1,
            kinds: Kinds::Workers,
            assignment,
        })
        .is_empty()
    );
    assert_eq!(
        &*h.settle(),
        &[
            Request::Assign {
                channel: C1,
                kind: HostKind::Worker,
                run: R1,
                attempt: A1,
                activation: A1.raw(),
                assignment
            },
            placed(R1, A1),
        ]
    );
}

#[test]
fn a_message_and_host_call_keep_their_fields_across_the_fleet() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    h.place(R1, A1, 1);
    let message = Message { name: Token::new(4), sender: payload(5), words: payload(6) };
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message }),
        &[Request::Inbound { channel: C1, run: R1, attempt: A1, message }]
    );
    let call = Call {
        name: Box::from(&b"call-one"[..]),
        tool: Box::from(&b"inspect"[..]),
        writes: true,
        input: Box::from(&b"input words"[..]),
        deadline: Duration::from_secs(3),
    };
    let mut emitted = h.step(Event::Relay { channel: C1, run: R1, attempt: A1, call }).into_iter();
    let Some(Request::Relay { reply_to, run, attempt, call }) = emitted.next() else { panic!("the call is relayed") };
    assert!(emitted.next().is_none());
    assert_eq!((run, attempt), (R1, A1));
    assert_eq!(&*call.name, b"call-one");
    assert_eq!(&*call.tool, b"inspect");
    assert!(call.writes);
    assert_eq!(&*call.input, b"input words");
    assert_eq!(call.deadline, Duration::from_secs(3));
    assert_eq!(
        &*h.step(Event::Relayed { to: reply_to, answer: payload(7) }),
        &[Request::Relayed {
            channel: C1,
            run: R1,
            attempt: A1,
            call: Box::from(&b"call-one"[..]),
            answer: payload(7),
            writes: true,
        }]
    );
}

fn lost(run: Token, attempt: Token) -> Request {
    Request::Lost { to: to(attempt), run, attempt }
}

fn withdrawn(run: Token, attempt: Token, withdrawal: Withdrawal) -> Request {
    Request::Withdrawn { to: to(attempt), run, attempt, withdrawal }
}

fn listed(run: Token, attempt: Token) -> Request {
    Request::Listed { run, attempt }
}

fn drop(raw: u64) -> Request {
    Request::Drop { payload: payload(raw) }
}

// Placement.

#[test]
fn a_start_is_placed_on_a_worker_with_a_free_slot() {
    let mut h = Harness::new(LIMITS);
    assert!(h.hello(C1, 2, &[], &[]).is_empty());
    assert!(h.start(R1, A1, 1).is_empty());
    assert_eq!(&*h.resume(), &[assign(C1, R1, A1), placed(R1, A1)]);
    // Ready until a resume finds nothing more to place.
    assert!(h.resume().is_empty());
    assert!(!h.domain.is_ready());
}

#[test]
fn placement_prefers_a_worker_holding_the_workstream_then_the_freest() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    h.hello(C2, 1, &[1], &[]);
    h.hello(C3, 2, &[], &[]);
    assert_eq!(h.place(R1, A1, 1), C2);
    // None holds w2: the one with the most free slots.
    assert_eq!(h.place(R2, A2, 2), C3);
    // C3 holds w2 from now on, and still has a slot.
    assert_eq!(h.place(R3, A3, 2), C3);
}

#[test]
fn a_worker_s_workstreams_evict_the_one_used_longest_ago() {
    let mut h = Harness::new(LIMITS);
    // Two keys a worker may hold: w1 and w2, w1 used longest ago.
    h.hello(C1, 2, &[1, 2], &[]);
    h.hello(C2, 2, &[], &[]);
    // w3 goes to the freest, C1 (the first of two equal); it evicts w1.
    assert_eq!(h.place(R1, A1, 3), C1);
    // C1 no longer holds w1, so w1 goes to the freest, C2.
    assert_eq!(h.place(R2, A2, 1), C2);
    // C1 holds w2 still.
    assert_eq!(h.place(R3, A3, 2), C1);
}

#[test]
fn a_start_waits_for_a_worker_then_for_a_slot() {
    let mut h = Harness::new(LIMITS);
    assert!(h.start(R1, A1, 1).is_empty());
    assert!(h.settle().is_empty());
    assert!(h.start(R2, A2, 2).is_empty());
    assert_eq!(&*h.hello(C1, 1, &[], &[]), &[]);
    assert_eq!(&*h.settle(), &[assign(C1, R1, A1), placed(R1, A1)]);
    assert_eq!(h.domain.waiting(), 1);
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 1), &[answered(A1, R1, Answer::Ended, 1)]);
    // The worker keeps the answer, and its slot, until the parent has it
    // durably.
    assert!(h.settle().is_empty());
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C1, R1, A1)]);
    assert_eq!(&*h.settle(), &[assign(C1, R2, A2), placed(R2, A2)]);
}

#[test]
fn a_hello_is_turned_away_only_when_there_is_no_room_for_a_worker() {
    let mut h = Harness::new(LIMITS);
    for channel in [C1, C2, C3] {
        h.hello(channel, 2, &[], &[]);
    }
    assert_eq!(&*h.hello(C4, 2, &[], &[]), &[Request::Refuse { channel: C4 }]);
    // Its loss is of no worker the fleet knows.
    assert!(h.step(Event::Lost { channel: C4 }).is_empty());
    assert!(h.facts().contains(&Fact::TurnedAway));
}

#[test]
fn a_second_hello_on_a_channel_is_dropped() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    assert!(h.hello(C1, 1, &[], &[hosted(R1, A1, Phase::Active)]).is_empty());
    assert_eq!(h.domain.attempts(), 0);
}

#[test]
fn listings_beyond_a_worker_s_slots_are_cancelled() {
    let mut h = Harness::new(LIMITS);
    let three = [hosted(R1, A1, Phase::Active), hosted(R2, A2, Phase::Active), hosted(R3, A3, Phase::Answered)];
    assert_eq!(&*h.hello(C1, 5, &[], &three), &[listed(R1, A1), listed(R2, A2)]);
    assert_eq!(h.domain.workers(), 1);
    let four = [
        hosted(Token::new(4), Token::new(14), Phase::Active),
        hosted(Token::new(5), Token::new(15), Phase::Active),
        hosted(Token::new(6), Token::new(16), Phase::Active),
        hosted(Token::new(7), Token::new(17), Phase::Active),
        hosted(Token::new(8), Token::new(18), Phase::Active),
    ];
    // Past twice the slots, dropped, so that a hello emits a bounded few.
    assert_eq!(
        &*h.hello(C2, 2, &[], &four),
        &[
            listed(Token::new(4), Token::new(14)),
            listed(Token::new(5), Token::new(15)),
            cancel(C2, Token::new(6), Token::new(16)),
            cancel(C2, Token::new(7), Token::new(17)),
        ]
    );
}

#[test]
fn listings_beyond_the_room_kept_for_them_are_cancelled() {
    // Room for one claim and two workers of one slot.
    let limits = Limits { attempts: 1, workers: 2, slots: 1, ..LIMITS };
    let mut h = Harness::new(limits);
    h.start(R1, A1, 1);
    h.hello(C1, 1, &[], &[hosted(R2, A2, Phase::Active)]);
    h.hello(C2, 1, &[], &[hosted(R3, A3, Phase::Active)]);
    assert_eq!(h.domain.attempts(), 3);
    // A worker back on a new channel before its old one is lost: the room is
    // taken, so what it lists anew is cancelled, and the worker kept.
    h.step(Event::Lost { channel: C2 });
    let hosting = [hosted(Token::new(4), Token::new(14), Phase::Active)];
    assert_eq!(&*h.hello(C3, 1, &[], &hosting), &[cancel(C3, Token::new(4), Token::new(14))]);
    assert_eq!(h.domain.workers(), 2);
}

#[test]
fn a_busy_refusal_places_the_attempt_again_elsewhere() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    // Nothing for the parent but the payload to forget.
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Busy, 1), &[drop(1)]);
    assert!(h.settle().is_empty());
    h.hello(C2, 1, &[], &[]);
    assert_eq!(&*h.settle(), &[assign(C2, R1, A1), placed(R1, A1)]);
    assert!(h.facts().contains(&Fact::Busy));
}

#[test]
fn a_busy_worker_takes_work_again_once_it_frees_a_slot() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.place(R2, A2, 2);
    h.answer(C1, R2, A2, Answer::Busy, 1);
    // A2 waits: C1 takes nothing while draining.
    assert!(h.settle().is_empty());
    h.answer(C1, R1, A1, Answer::Ended, 2);
    assert!(h.settle().is_empty());
    h.acknowledge(R1, A1);
    assert_eq!(&*h.settle(), &[assign(C1, R2, A2), placed(R2, A2)]);
}

#[test]
fn a_cancelled_attempt_refused_as_busy_is_withdrawn() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.step(Event::Cancel { run: R1, attempt: A1 });
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Busy, 1), &[drop(1), withdrawn(R1, A1, Withdrawal::Cancelled)]);
}

#[test]
fn an_invalid_refusal_is_handed_on_and_keeps_no_slot() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Invalid, 1), &[answered(A1, R1, Answer::Invalid, 1)]);
    h.start(R2, A2, 2);
    assert_eq!(&*h.settle(), &[assign(C1, R2, A2), placed(R2, A2)]);
    // The parent's acknowledgement changes nothing.
    assert!(h.acknowledge(R1, A1).is_empty());
}

#[test]
fn starts_are_refused_at_the_entrance() {
    let limits = Limits { attempts: 1, ..LIMITS };
    let mut h = Harness::new(limits);
    let refused = |run, attempt, refusal| Request::Refused { to: to(attempt), run, attempt, refusal };
    assert_eq!(&*h.start(R1, A1, 0), &[refused(R1, A1, Refusal::Workstream)]);
    assert!(h.start(R1, A1, 1).is_empty());
    assert_eq!(&*h.start(R1, A1, 1), &[refused(R1, A1, Refusal::Duplicate)]);
    assert_eq!(&*h.start(R2, A2, 2), &[refused(R2, A2, Refusal::Busy)]);
    assert_eq!(&*h.adopt(R2, A2), &[refused(R2, A2, Refusal::Busy)]);
    assert_eq!(&*h.adopt(R1, A1), &[refused(R1, A1, Refusal::Duplicate)]);
    // A worker's listings take the room kept for them, not the parent's.
    assert_eq!(&*h.hello(C1, 2, &[], &[hosted(R3, A3, Phase::Active)]), &[listed(R3, A3)]);
}

// Answers, acknowledgements and fencing.

#[test]
fn an_answer_is_handed_on_once_and_acknowledged_once_the_parent_has_it() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Parked, 1), &[answered(A1, R1, Answer::Parked, 1)]);
    // Sent again after a hello, before the parent has it durably: dropped,
    // not acknowledged.
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Parked, 2), &[drop(2)]);
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C1, R1, A1)]);
    // Its acknowledgement lost, it comes again: acknowledged again.
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Parked, 3), &[ack(C1, R1, A1), drop(3)]);
    assert_eq!(h.domain.attempts(), 0);
}

#[test]
fn an_answer_from_a_channel_not_in_contact_is_dropped_unacknowledged() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Ended, 1), &[drop(1)]);
    h.step(Event::Lost { channel: C1 });
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 2), &[drop(2)]);
    // Still the claim, adrift: the hello brings its answer.
    h.hello(C2, 2, &[], &[hosted(R1, A1, Phase::Answered)]);
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Ended, 3), &[answered(A1, R1, Answer::Ended, 3)]);
}

#[test]
fn an_answer_acknowledged_while_its_worker_is_away_is_acknowledged_once_it_is_back() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.answer(C1, R1, A1, Answer::Ended, 1);
    h.step(Event::Lost { channel: C1 });
    assert!(h.acknowledge(R1, A1).is_empty());
    assert_eq!(&*h.hello(C2, 2, &[], &[hosted(R1, A1, Phase::Answered)]), &[ack(C2, R1, A1)]);
    // Sent again after the hello, as it was: acknowledged again.
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Ended, 2), &[ack(C2, R1, A1), drop(2)]);
    assert_eq!(h.domain.attempts(), 0);
}

#[test]
fn an_answer_acknowledged_while_its_worker_is_away_is_forgotten_past_the_grace() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.answer(C1, R1, A1, Answer::Ended, 1);
    h.step(Event::Lost { channel: C1 });
    h.acknowledge(R1, A1);
    assert!(h.at(10).is_empty());
    assert_eq!(h.domain.attempts(), 0);
}

#[test]
fn a_cancelled_attempt_is_fenced_but_its_answer_ends_its_call() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(&*h.step(Event::Cancel { run: R1, attempt: A1 }), &[cancel(C1, R1, A1)]);
    assert!(h.step(Event::Cancel { run: R1, attempt: A1 }).is_empty());
    let event = payload(5);
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message: message(event) }),
        &[Request::Undelivered { run: R1, attempt: A1, message: message(event), undelivered: Undelivered::Gone }]
    );
    assert_eq!(
        &*h.step(Event::Relay { channel: C1, run: R1, attempt: A1, call: host_call(Token::new(7), payload(6)) }),
        &[Request::DropCall { call: host_call(Token::new(7), payload(6)) }]
    );
    assert!(
        h.step(Event::Bounced { channel: C1, name: Token::new(0), run: R1, attempt: A1, bounce: Bounce::Ending })
            .is_empty()
    );
    assert_eq!(&*h.step(Event::Told { channel: C1, run: R1, attempt: A1, fact: payload(7) }), &[drop(7)]);
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Failed, 8), &[answered(A1, R1, Answer::Failed, 8)]);
    // Answered: a cancel changes nothing.
    assert!(h.step(Event::Cancel { run: R1, attempt: A1 }).is_empty());
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C1, R1, A1)]);
}

#[test]
fn a_cancel_before_placement_withdraws_the_attempt() {
    let mut h = Harness::new(LIMITS);
    h.start(R1, A1, 1);
    assert_eq!(&*h.step(Event::Cancel { run: R1, attempt: A1 }), &[withdrawn(R1, A1, Withdrawal::Cancelled)]);
    // Its run is gone too.
    assert!(h.step(Event::Cancel { run: R1, attempt: A1 }).is_empty());
    h.hello(C1, 1, &[], &[]);
    assert!(h.settle().is_empty());
    assert_eq!(h.domain.attempts(), 0);
}

#[test]
fn a_newer_attempt_replaces_the_claim_and_waits_for_it_to_be_gone() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(&*h.start(R1, A2, 1), &[cancel(C1, R1, A1), withdrawn(R1, A1, Withdrawal::Replaced)]);
    // Never two attempts of a run on the workers: A2 waits for A1.
    assert!(h.settle().is_empty());
    assert_eq!(
        &*h.step(Event::Relay { channel: C1, run: R1, attempt: A1, call: host_call(Token::new(7), payload(1)) }),
        &[Request::DropCall { call: host_call(Token::new(7), payload(1)) }]
    );
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 2), &[ack(C1, R1, A1), drop(2)]);
    assert_eq!(&*h.settle(), &[assign(C1, R1, A2), placed(R1, A2)]);
}

#[test]
fn a_newer_attempt_replaces_a_waiting_one_at_once() {
    let mut h = Harness::new(LIMITS);
    h.start(R1, A1, 1);
    assert_eq!(&*h.start(R1, A2, 1), &[withdrawn(R1, A1, Withdrawal::Replaced)]);
    h.hello(C1, 2, &[], &[]);
    assert_eq!(&*h.settle(), &[assign(C1, R1, A2), placed(R1, A2)]);
}

#[test]
fn a_waiting_attempt_a_worker_lists_is_not_assigned_again() {
    let mut h = Harness::new(LIMITS);
    h.start(R1, A1, 1);
    assert_eq!(&*h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[placed(R1, A1)]);
    assert!(h.settle().is_empty());
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 1), &[answered(A1, R1, Answer::Ended, 1)]);
}

// Relaying.

#[test]
fn a_relayed_call_goes_up_once_and_its_answer_down_once() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    let call = Token::new(7);
    let reply_to = h.relay(R1, A1, call, 1);
    assert_eq!(
        &*h.step(Event::Relayed { to: reply_to, answer: payload(2) }),
        &[Request::Relayed {
            channel: C1,
            run: R1,
            attempt: A1,
            call: Box::from(call.raw().to_be_bytes()),
            answer: payload(2),
            writes: false,
        }]
    );
    assert_eq!(h.domain.calls(), 0);
}

#[test]
fn a_relayed_answer_for_an_attempt_gone_is_dropped() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    let reply_to = h.relay(R1, A1, Token::new(7), 1);
    h.answer(C1, R1, A1, Answer::Ended, 2);
    assert_eq!(&*h.step(Event::Relayed { to: reply_to, answer: payload(3) }), &[drop(3)]);
}

#[test]
fn a_relayed_call_beyond_the_room_is_dropped() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    for raw in 0..2 {
        let _call: ReplyTo = h.relay(R1, A1, Token::new(raw), raw);
    }
    assert_eq!(
        &*h.step(Event::Relay { channel: C1, run: R1, attempt: A1, call: host_call(Token::new(9), payload(9)) }),
        &[Request::DropCall { call: host_call(Token::new(9), payload(9)) }]
    );
}

fn unowned_worker_inputs(h: &mut Harness, channel: Token) {
    assert_eq!(
        &*h.step(Event::Relay { channel, run: R1, attempt: A1, call: host_call(Token::new(8), payload(8)) }),
        &[Request::DropCall { call: host_call(Token::new(8), payload(8)) }]
    );
    assert_eq!(&*h.step(Event::Told { channel, run: R1, attempt: A1, fact: payload(9) }), &[drop(9)]);
    assert!(
        h.step(Event::Bounced { channel, run: R1, attempt: A1, name: Token::new(3), bounce: Bounce::Full }).is_empty()
    );
    assert!(h.step(Event::Rejected { channel, run: R1, attempt: A1, account: 7, generation: 4 }).is_empty());
    assert!(
        h.step(Event::Exhausted { channel, run: R1, attempt: A1, account: 7, retry_after: Duration::from_secs(5) })
            .is_empty()
    );
}

fn owned_worker_notices(h: &mut Harness, channel: Token) {
    assert_eq!(
        &*h.step(Event::Told { channel, run: R1, attempt: A1, fact: payload(9) }),
        &[Request::Told { run: R1, attempt: A1, fact: payload(9) }]
    );
    assert_eq!(
        &*h.step(Event::Bounced { channel, run: R1, attempt: A1, name: Token::new(3), bounce: Bounce::Full }),
        &[Request::Bounced { run: R1, attempt: A1, name: Token::new(3), bounce: Bounce::Full }]
    );
    assert_eq!(
        &*h.step(Event::Rejected { channel, run: R1, attempt: A1, account: 7, generation: 4 }),
        &[Request::Rejected { run: R1, attempt: A1, account: 7, generation: 4 }]
    );
    assert_eq!(
        &*h.step(Event::Exhausted { channel, run: R1, attempt: A1, account: 7, retry_after: Duration::from_secs(5) }),
        &[Request::Exhausted { run: R1, attempt: A1, account: 7, retry_after: Duration::from_secs(5) }]
    );
}

#[test]
fn worker_inputs_require_the_current_host_but_accepted_calls_survive_its_channel() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    assert_eq!(h.place(R1, A1, 1), C1);
    h.hello(C2, 2, &[], &[]);
    unowned_worker_inputs(&mut h, C2);
    owned_worker_notices(&mut h, C1);
    let call = Token::new(7);
    let in_flight = h.relay(R1, A1, call, 1);
    h.step(Event::Lost { channel: C1 });
    unowned_worker_inputs(&mut h, C1);
    unowned_worker_inputs(&mut h, C2);
    h.hello(C3, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    unowned_worker_inputs(&mut h, C1);
    unowned_worker_inputs(&mut h, C2);
    owned_worker_notices(&mut h, C3);
    assert_eq!(
        &*h.step(Event::Relayed { to: in_flight, answer: payload(2) }),
        &[Request::Relayed {
            channel: C3,
            run: R1,
            attempt: A1,
            call: Box::from(call.raw().to_be_bytes()),
            answer: payload(2),
            writes: false,
        }]
    );
    let up = h.step(Event::Relay { channel: C3, run: R1, attempt: A1, call: host_call(Token::new(8), payload(8)) });
    let [Request::Relay { run, attempt, call, .. }] = &*up else { panic!("the current host's call is relayed") };
    assert_eq!((*run, *attempt, &call.input), (R1, A1, &host_call(Token::new(8), payload(8)).input));
}

#[test]
fn inbound_events_bounces_and_facts_pass_while_the_claim_is_live() {
    let mut h = Harness::new(LIMITS);
    let event = payload(1);
    h.start(R1, A1, 1);
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message: message(event) }),
        &[Request::Undelivered { run: R1, attempt: A1, message: message(event), undelivered: Undelivered::Unplaced }]
    );
    h.hello(C1, 2, &[], &[]);
    h.settle();
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message: message(event) }),
        &[Request::Inbound { channel: C1, run: R1, attempt: A1, message: message(event) }]
    );
    assert_eq!(
        &*h.step(Event::Bounced { channel: C1, name: Token::new(0), run: R1, attempt: A1, bounce: Bounce::Full }),
        &[Request::Bounced { name: Token::new(0), run: R1, attempt: A1, bounce: Bounce::Full }]
    );
    assert_eq!(
        &*h.step(Event::Told { channel: C1, run: R1, attempt: A1, fact: payload(2) }),
        &[Request::Told { run: R1, attempt: A1, fact: payload(2) }]
    );
    h.step(Event::Lost { channel: C1 });
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message: message(event) }),
        &[Request::Undelivered { run: R1, attempt: A1, message: message(event), undelivered: Undelivered::Adrift }]
    );
    assert_eq!(
        &*h.step(Event::Inbound { run: R2, attempt: A2, message: message(event) }),
        &[Request::Undelivered { run: R2, attempt: A2, message: message(event), undelivered: Undelivered::Gone }]
    );
}

// Losing contact.

#[test]
fn a_lost_worker_s_runs_are_kept_for_the_grace_then_presumed_lost() {
    let mut h = Harness::new(LIMITS);
    h.loaded();
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.place(R2, A2, 2);
    h.step(Event::Cancel { run: R2, attempt: A2 });
    h.at(5);
    assert!(h.step(Event::Lost { channel: C1 }).is_empty());
    assert!(h.at(14).is_empty());
    assert_eq!(&*h.at(15), &[lost(R1, A1), lost(R2, A2)]);
    // Back past the grace: an attempt listed then is a stray, which the
    // parent hears of.
    assert_eq!(&*h.hello(C2, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[listed(R1, A1)]);
}

#[test]
fn a_worker_back_within_the_grace_keeps_what_is_claimed_and_cancels_the_rest() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.place(R2, A2, 2);
    h.step(Event::Lost { channel: C1 });
    // Cancelled while adrift: the cancel goes once a hello lists it.
    assert!(h.step(Event::Cancel { run: R2, attempt: A2 }).is_empty());
    let hosting = [hosted(R1, A1, Phase::Waiting), hosted(R2, A2, Phase::Active)];
    assert_eq!(&*h.hello(C2, 2, &[], &hosting), &[cancel(C2, R2, A2)]);
    assert!(h.at(20).is_empty());
    let event = payload(1);
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message: message(event) }),
        &[Request::Inbound { channel: C2, run: R1, attempt: A1, message: message(event) }]
    );
    assert_eq!(&*h.answer(C2, R2, A2, Answer::Failed, 2), &[answered(A2, R2, Answer::Failed, 2)]);
}

#[test]
fn an_answer_held_while_the_channel_was_down_follows_the_hello() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    h.place(R1, A1, 1);
    h.step(Event::Cancel { run: R1, attempt: A1 });
    h.step(Event::Lost { channel: C1 });
    // Answered: no cancel again.
    assert!(h.hello(C2, 1, &[], &[hosted(R1, A1, Phase::Answered)]).is_empty());
    h.start(R2, A2, 2);
    // Its answer keeps the slot until the parent has it durably.
    assert!(h.settle().is_empty());
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Failed, 1), &[answered(A1, R1, Answer::Failed, 1)]);
    assert!(h.settle().is_empty());
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C2, R1, A1)]);
    assert_eq!(&*h.settle(), &[assign(C2, R2, A2), placed(R2, A2)]);
}

#[test]
fn a_handed_answer_listed_again_keeps_its_slot_on_the_new_channel() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    h.place(R1, A1, 1);
    h.answer(C1, R1, A1, Answer::Ended, 1);
    h.step(Event::Lost { channel: C1 });
    assert!(h.hello(C2, 1, &[], &[hosted(R1, A1, Phase::Answered)]).is_empty());
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Ended, 2), &[drop(2)]);
    h.start(R2, A2, 2);
    assert!(h.settle().is_empty());
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C2, R1, A1)]);
    assert_eq!(&*h.settle(), &[assign(C2, R2, A2), placed(R2, A2)]);
}

#[test]
fn a_fenced_attempt_listed_again_is_cancelled_again() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h.step(Event::Lost { channel: C1 });
    assert_eq!(&*h.start(R1, A2, 1), &[withdrawn(R1, A1, Withdrawal::Replaced)]);
    assert_eq!(&*h.hello(C2, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[cancel(C2, R1, A1)]);
    // A2 waits for A1, wherever it is.
    assert!(h.settle().is_empty());
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Failed, 1), &[ack(C2, R1, A1), drop(1)]);
    assert_eq!(&*h.settle(), &[assign(C2, R1, A2), placed(R1, A2)]);
}

#[test]
fn a_fenced_attempt_adrift_holds_its_run_back_until_the_grace_passes() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[]);
    h.place(R1, A1, 1);
    h.step(Event::Lost { channel: C1 });
    h.start(R1, A2, 1);
    h.hello(C2, 1, &[], &[]);
    assert!(h.settle().is_empty());
    assert!(h.at(10).is_empty());
    assert_eq!(&*h.settle(), &[assign(C2, R1, A2), placed(R1, A2)]);
}

#[test]
fn an_attempt_listed_for_a_run_claimed_otherwise_is_fenced_at_once() {
    let mut h = Harness::new(LIMITS);
    h.start(R1, A2, 1);
    assert_eq!(&*h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[cancel(C1, R1, A1)]);
    assert!(h.settle().is_empty());
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Failed, 1), &[ack(C1, R1, A1), drop(1)]);
    assert_eq!(&*h.settle(), &[assign(C1, R1, A2), placed(R1, A2)]);
}

// Restarts.

#[test]
fn an_adopted_claim_no_worker_lists_within_the_grace_is_lost() {
    let mut h = Harness::new(LIMITS);
    assert!(h.adopt(R1, A1).is_empty());
    let event = payload(1);
    assert_eq!(
        &*h.step(Event::Inbound { run: R1, attempt: A1, message: message(event) }),
        &[Request::Undelivered { run: R1, attempt: A1, message: message(event), undelivered: Undelivered::Adrift }]
    );
    assert!(h.at(9).is_empty());
    assert_eq!(&*h.at(10), &[lost(R1, A1)]);
}

#[test]
fn an_adopted_claim_a_worker_lists_is_placed() {
    let mut h = Harness::new(LIMITS);
    h.adopt(R1, A1);
    assert_eq!(&*h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[placed(R1, A1)]);
    assert!(h.at(30).is_empty());
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 1), &[answered(A1, R1, Answer::Ended, 1)]);
}

#[test]
fn strays_wait_for_the_load_then_for_the_grace() {
    let mut h = Harness::new(LIMITS);
    assert_eq!(&*h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[listed(R1, A1)]);
    // The parent still reads its claims: the stray waits.
    assert!(h.at(100).is_empty());
    assert!(h.loaded().is_empty());
    assert!(h.at(109).is_empty());
    assert_eq!(&*h.at(110), &[cancel(C1, R1, A1)]);
    // Listed after the load: the grace from its listing.
    assert_eq!(&*h.hello(C2, 2, &[], &[hosted(R2, A2, Phase::Active)]), &[listed(R2, A2)]);
    assert_eq!(&*h.at(120), &[cancel(C2, R2, A2)]);
}

#[test]
fn a_stray_adopted_within_the_grace_is_the_claim() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    h.loaded();
    h.at(5);
    assert_eq!(&*h.adopt(R1, A1), &[placed(R1, A1)]);
    assert!(h.at(30).is_empty());
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 1), &[answered(A1, R1, Answer::Ended, 1)]);
}

#[test]
fn a_stray_s_answer_is_kept_unacknowledged_for_its_adoption() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Answered), hosted(R2, A2, Phase::Answered)]);
    assert!(h.answer(C1, R1, A1, Answer::Ended, 1).is_empty());
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Ended, 2), &[drop(2)]);
    assert!(h.answer(C1, R2, A2, Answer::Parked, 3).is_empty());
    h.loaded();
    assert_eq!(&*h.adopt(R1, A1), &[answered(A1, R1, Answer::Ended, 1)]);
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C1, R1, A1)]);
    // Not adopted in time: forgotten, by the parent and by its worker.
    assert_eq!(&*h.at(10), &[drop(3), ack(C1, R2, A2)]);
    assert_eq!(h.domain.attempts(), 0);
}

#[test]
fn a_kept_answer_listed_again_keeps_its_slot() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[hosted(R1, A1, Phase::Answered)]);
    h.answer(C1, R1, A1, Answer::Ended, 1);
    h.step(Event::Lost { channel: C1 });
    assert!(h.hello(C2, 1, &[], &[hosted(R1, A1, Phase::Answered)]).is_empty());
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Ended, 2), &[drop(2)]);
    // The worker keeps the answer, so its one slot is taken.
    h.start(R2, A2, 2);
    assert!(h.settle().is_empty());
    assert_eq!(&*h.adopt(R1, A1), &[answered(A1, R1, Answer::Ended, 1)]);
    assert_eq!(&*h.acknowledge(R1, A1), &[ack(C2, R1, A1)]);
    assert_eq!(&*h.settle(), &[assign(C2, R2, A2), placed(R2, A2)]);
}

#[test]
fn a_kept_answer_forgotten_while_its_worker_is_away_is_acknowledged_once_it_is_back() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 1, &[], &[hosted(R1, A1, Phase::Answered)]);
    h.answer(C1, R1, A1, Answer::Ended, 1);
    h.loaded();
    h.at(5);
    h.step(Event::Lost { channel: C1 });
    assert_eq!(&*h.at(10), &[drop(1)]);
    assert!(h.hello(C2, 1, &[], &[hosted(R1, A1, Phase::Answered)]).is_empty());
    assert_eq!(&*h.answer(C2, R1, A1, Answer::Ended, 2), &[ack(C2, R1, A1), drop(2)]);
}

#[test]
fn a_run_no_claim_adopts_is_cancelled_after_the_grace() {
    let mut h = Harness::new(LIMITS);
    h.loaded();
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    assert_eq!(&*h.at(10), &[cancel(C1, R1, A1)]);
    // Adopted too late: lost, as far as the parent can tell.
    assert_eq!(&*h.adopt(R1, A1), &[lost(R1, A1)]);
    h.start(R1, A2, 1);
    assert!(h.settle().is_empty());
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Failed, 1), &[ack(C1, R1, A1), drop(1)]);
    assert_eq!(&*h.settle(), &[assign(C1, R1, A2), placed(R1, A2)]);
}

#[test]
fn a_stray_whose_channel_is_lost_is_fenced_at_its_deadline_and_gone_past_the_grace() {
    let mut h = Harness::new(LIMITS);
    h.loaded();
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    h.at(4);
    h.step(Event::Lost { channel: C1 });
    assert!(h.at(10).is_empty());
    assert!(h.facts().contains(&Fact::Fenced));
    h.start(R1, A2, 1);
    h.hello(C2, 2, &[], &[]);
    assert!(h.settle().is_empty());
    assert!(h.at(14).is_empty());
    assert_eq!(&*h.settle(), &[assign(C2, R1, A2), placed(R1, A2)]);
}

#[test]
fn a_stray_or_a_kept_answer_can_be_cancelled() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active), hosted(R2, A2, Phase::Answered)]);
    assert_eq!(&*h.step(Event::Cancel { run: R1, attempt: A1 }), &[cancel(C1, R1, A1)]);
    h.answer(C1, R2, A2, Answer::Ended, 1);
    assert_eq!(&*h.step(Event::Cancel { run: R2, attempt: A2 }), &[drop(1), ack(C1, R2, A2)]);
}

#[test]
fn an_adoption_replaces_the_parent_s_own_claim() {
    let mut h = Harness::new(LIMITS);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(&*h.adopt(R1, A2), &[cancel(C1, R1, A1), withdrawn(R1, A1, Withdrawal::Replaced)]);
    assert_eq!(&*h.at(10), &[lost(R1, A2)]);
}

#[test]
fn an_adopted_claim_cancelled_is_cancelled_once_found() {
    let mut h = Harness::new(LIMITS);
    h.adopt(R1, A1);
    assert!(h.step(Event::Cancel { run: R1, attempt: A1 }).is_empty());
    assert_eq!(&*h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]), &[cancel(C1, R1, A1)]);
    assert_eq!(&*h.answer(C1, R1, A1, Answer::Failed, 1), &[answered(A1, R1, Answer::Failed, 1)]);
}

// Facts and memory.

#[test]
fn facts_beyond_the_room_are_dropped_and_counted() {
    let limits = Limits { facts: 1, ..LIMITS };
    let mut h = Harness::new(limits);
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    assert_eq!(h.domain.facts_lost(), 1);
    assert_eq!(&*h.facts(), &[Fact::Hello { listed: 0 }]);
}

#[test]
fn the_worst_case_is_bounded_or_refused() {
    assert!(worst_case(&LIMITS).is_some());
    let huge = Limits { workers: u32::MAX, slots: u32::MAX, ..LIMITS };
    assert_eq!(worst_case(&huge), None);
    assert_eq!(max_out(&LIMITS), 4);
    assert_eq!(max_out(&Limits { slots: 1, ..LIMITS }), 3);
    assert_eq!(max_out(&Limits { slots: 8, ..LIMITS }), 16);
}

fn turns() -> Harness {
    let mut h = Harness::new(Limits { turns: 2, ..LIMITS });
    h.hello(C1, 2, &[], &[]);
    h.place(R1, A1, 1);
    h
}

fn turn(h: &mut Harness, channel: Token, nth: u32, raw: u64) -> Box<[Request]> {
    h.step(Event::Turn { channel, run: R1, attempt: A1, turn: nth, body: payload(raw) })
}

fn kept(h: &mut Harness, nth: u32) -> Box<[Request]> {
    h.step(Event::TurnKept { run: R1, attempt: A1, turn: nth })
}

fn turn_ack(channel: Token, nth: u32) -> Request {
    Request::AcknowledgeTurn { channel, run: R1, attempt: A1, turn: nth }
}

#[test]
fn a_turn_is_handed_once_and_acknowledged_only_after_commitment() {
    let mut h = turns();
    assert_eq!(&*turn(&mut h, C1, 1, 1), &[Request::Turned { run: R1, attempt: A1, turn: 1, body: payload(1) }]);
    assert_eq!(&*turn(&mut h, C1, 1, 2), &[drop(2)]);
    assert_eq!(&*kept(&mut h, 1), &[turn_ack(C1, 1)]);
    assert_eq!(&*turn(&mut h, C1, 1, 3), &[turn_ack(C1, 1), drop(3)]);
}

#[test]
fn turn_inputs_require_the_current_hosting_channel() {
    let mut h = turns();
    h.hello(C2, 2, &[], &[]);
    assert_eq!(&*turn(&mut h, C2, 1, 1), &[drop(1)]);
    h.step(Event::Lost { channel: C1 });
    assert_eq!(&*turn(&mut h, C1, 1, 2), &[drop(2)]);
    assert_eq!(&*turn(&mut h, C2, 1, 3), &[drop(3)]);
    h.hello(C3, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    assert_eq!(&*turn(&mut h, C2, 1, 4), &[drop(4)]);
    assert_eq!(&*turn(&mut h, C3, 1, 5), &[Request::Turned { run: R1, attempt: A1, turn: 1, body: payload(5) }]);
}

#[test]
fn turns_committed_out_of_contact_are_acknowledged_on_replay() {
    let mut h = turns();
    turn(&mut h, C1, 1, 1);
    h.step(Event::Lost { channel: C1 });
    assert!(kept(&mut h, 1).is_empty());
    h.hello(C2, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    assert_eq!(&*turn(&mut h, C2, 1, 2), &[turn_ack(C2, 1), drop(2)]);
}

#[test]
fn turns_from_fenced_attempts_are_acknowledged_without_freeing_the_host() {
    let mut h = turns();
    h.step(Event::Cancel { run: R1, attempt: A1 });
    assert_eq!(&*turn(&mut h, C1, 1, 1), &[turn_ack(C1, 1), drop(1)]);
    assert_eq!(h.domain.attempts(), 1, "turns do not end the attempt");
    h.answer(C1, R1, A1, Answer::Ended, 2);
    h.acknowledge(R1, A1);
    assert_eq!(&*turn(&mut h, C1, 1, 3), &[turn_ack(C1, 1), drop(3)]);
}

#[test]
fn capacity_and_parent_busy_both_release_turns_for_retry() {
    let mut h = turns();
    turn(&mut h, C1, 1, 1);
    turn(&mut h, C1, 2, 2);
    assert_eq!(&*turn(&mut h, C1, 3, 3), &[Request::TurnBusy { channel: C1, run: R1, attempt: A1, turn: 3 }, drop(3)]);
    assert_eq!(
        &*h.step(Event::TurnBusy { run: R1, attempt: A1, turn: 1 }),
        &[Request::TurnBusy { channel: C1, run: R1, attempt: A1, turn: 1 }]
    );
    assert_eq!(&*turn(&mut h, C1, 1, 4), &[Request::Turned { run: R1, attempt: A1, turn: 1, body: payload(4) }]);
    kept(&mut h, 1);
    assert!(h.step(Event::TurnBusy { run: R1, attempt: A1, turn: 1 }).is_empty());
    assert_eq!(&*turn(&mut h, C1, 1, 5), &[turn_ack(C1, 1), drop(5)]);
}

#[test]
fn adoption_restores_the_prefix_before_releasing_stray_turns() {
    let mut h = Harness::new(Limits { turns: 2, ..LIMITS });
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    assert!(turn(&mut h, C1, 1, 1).is_empty());
    assert!(turn(&mut h, C1, 2, 2).is_empty());
    assert_eq!(&*turn(&mut h, C1, 2, 3), &[drop(3)]);
    h.step(Event::Adopt { reply_to: to(A1), run: R1, attempt: A1, kept: 1, kind: HostKind::Worker, worked: false });
    assert_eq!(
        &*h.settle(),
        &[turn_ack(C1, 1), drop(1), Request::Turned { run: R1, attempt: A1, turn: 2, body: payload(2) }]
    );
    assert_eq!(&*kept(&mut h, 2), &[turn_ack(C1, 2)]);
}

#[test]
fn a_turn_sent_again_after_a_restart_is_acknowledged_again_and_dropped() {
    let mut h = Harness::new(Limits { turns: 2, ..LIMITS });
    h.step(Event::Adopt { reply_to: to(A1), run: R1, attempt: A1, kept: 7, kind: HostKind::Worker, worked: false });
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    assert_eq!(&*turn(&mut h, C1, 7, 1), &[turn_ack(C1, 7), drop(1)]);
    assert_eq!(&*turn(&mut h, C1, 8, 2), &[Request::Turned { run: R1, attempt: A1, turn: 8, body: payload(2) }]);
}

#[test]
fn stray_turn_bodies_are_released_when_the_claim_is_fenced_or_lost() {
    let mut h = Harness::new(Limits { turns: 2, ..LIMITS });
    h.hello(C1, 2, &[], &[hosted(R1, A1, Phase::Active)]);
    turn(&mut h, C1, 1, 1);
    h.loaded();
    h.at(10);
    assert_eq!(&*h.settle(), &[turn_ack(C1, 1), drop(1)]);
    assert!(h.domain.turns.is_empty());
}

#[test]
fn declared_stop_bounds_must_be_strictly_below_the_engine_grace() {
    for (stop_bound, accepted) in [
        (Duration::from_secs(0), true),
        (Duration::from_secs(9), true),
        (Duration::from_secs(10), false),
        (Duration::from_secs(11), false),
    ] {
        let mut h = Harness::new(LIMITS);
        let hello = Hello { stop_bound, slots: 2, workstreams: Box::default(), hosting: Box::default() };
        let out = h.step(Event::Hello { channel: C1, hello });
        if accepted {
            assert!(out.is_empty());
            assert_eq!(h.domain.workers(), 1);
        } else {
            assert_eq!(&*out, &[Request::Refuse { channel: C1 }]);
            assert_eq!(h.domain.workers(), 0);
        }
    }
}

fn references() -> Assignment {
    Assignment { turns: payload(0), answered: payload(0) }
}
fn message(name: Token) -> Message {
    Message { name, sender: name, words: name }
}
fn host_call(name: Token, body: Token) -> Call {
    Call {
        name: Box::from(name.raw().to_be_bytes()),
        tool: Box::from(&b"tool"[..]),
        writes: false,
        input: Box::from(body.raw().to_be_bytes()),
        deadline: Duration::ZERO,
    }
}
