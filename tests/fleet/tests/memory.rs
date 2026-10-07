//! Memory stays within the worst case (programming-model.md, 6.3), measured by
//! a counting allocator: every worker in contact holding as many workstream
//! keys as it may, every attempt tracked, every relayed call kept, and every
//! entry point on the way.

use skein_lib::{Duration, Env, Queue, ReplyTo, Time, Token, Wall};
use jig_core_fleet::{
    Answer, Bounce, Domain, Event, Hello, Hosted, Limits, Phase, Request, fire, max_out, resume, step, worst_case,
};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

const LIMITS: Limits = Limits {
    workers: 3,
    slots: 3,
    workstreams: 3,
    attempts: 12,
    calls: 4,
    turns: 0,
    grace: Duration::from_secs(10),
    facts: 16,
};

/// The fleet under `limits`, measured: each step's peak is checked against
/// the worst case, less what it handed out in requests, which their receivers
/// count.
struct Measured {
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
    meter: Meter,
    bound: u64,
    names: u64,
}

impl Measured {
    fn new(limits: Limits) -> Measured {
        let bound = worst_case(&limits).expect("the test limits fit");
        let meter = Meter::new();
        let domain = Domain::new(&limits);
        let out = Queue::with_capacity(max_out(&limits));
        Measured { domain, env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits }, out, meter, bound, names: 0 }
    }

    fn name(&mut self) -> Token {
        self.names += 1;
        Token::new(self.names)
    }

    fn step(&mut self, event: Event) -> Vec<Request> {
        self.meter.start();
        step(&mut self.domain, &self.env, event, &mut self.out);
        self.drain()
    }

    /// Places what can be placed, one resume at a time.
    fn settle(&mut self) -> Vec<Request> {
        let mut requests = Vec::new();
        while self.domain.is_ready() {
            self.meter.start();
            resume(&mut self.domain, &self.env, &mut self.out);
            requests.extend(self.drain());
        }
        requests
    }

    /// Moves the clock to `secs` and fires every alarm due.
    fn at(&mut self, secs: u64) -> Vec<Request> {
        self.env.now = Time::ZERO.saturating_add(Duration::from_secs(secs));
        let mut requests = Vec::new();
        while self.domain.is_due(self.env.now) {
            self.meter.start();
            fire(&mut self.domain, &self.env, &mut self.out);
            requests.extend(self.drain());
        }
        requests
    }

    fn drain(&mut self) -> Vec<Request> {
        let measured = self.meter.end();
        let mut requests = Vec::with_capacity(self.out.len() as usize);
        while let Some(request) = self.out.pop() {
            requests.push(request);
        }
        self.meter.check(measured, self.bound, self.env.limits);
        // The iteration ends: the reclaim point.
        self.domain.reclaim();
        while self.domain.pop_fact().is_some() {}
        requests
    }

    /// A hello on `channel`, holding as many keys as a worker may, of the
    /// most bytes, and hosting `hosting`.
    fn hello(&mut self, channel: Token, hosting: Vec<Hosted>) -> Vec<Request> {
        let limits = self.env.limits;
        let workstreams = (0..limits.workstreams).map(|nth| channel.raw() * 100 + u64::from(nth)).collect();
        self.step(Event::Hello {
            channel,
            hello: Hello { graces: None, slots: limits.slots, workstreams, hosting: hosting.into() },
        })
    }

    /// Starts an attempt of a run of its own, with a key of the most bytes.
    fn start(&mut self) -> (Token, Token) {
        let (run, attempt) = (self.name(), self.name());
        let workstream = run.raw();
        let asked = self.step(Event::Start { reply_to: ReplyTo::new(attempt), run, attempt, workstream });
        assert!(asked.is_empty(), "a start waits for placement: {asked:?}");
        (run, attempt)
    }
}

/// The channels of the workers, `C1` and on.
fn channel(nth: u32) -> Token {
    Token::new(1_000_000 + u64::from(nth))
}

/// The attempts placed, with their workers' channels.
fn placed(requests: &[Request]) -> Vec<(Token, Token, Token)> {
    let mut placed = Vec::new();
    for request in requests {
        if let Request::Assign { channel, run, attempt } = request {
            placed.push((*channel, *run, *attempt));
        }
    }
    placed
}

/// Fills the fleet: every worker in contact with its keys, every attempt
/// tracked, as many placed as there are slots, and every relayed call kept.
fn fill(limits: Limits) -> (Measured, Vec<(Token, Token, Token)>, Vec<ReplyTo>) {
    let mut fleet = Measured::new(limits);
    for nth in 0..limits.workers {
        assert!(fleet.hello(channel(nth), Vec::new()).is_empty());
    }
    for _ in 0..limits.attempts {
        fleet.start();
    }
    let placed = placed(&fleet.settle());
    assert_eq!(u32::try_from(placed.len()).expect("a few"), limits.workers * limits.slots, "every slot taken");
    let mut calls = Vec::new();
    for (nth, &(channel, run, attempt)) in placed.iter().enumerate().take(limits.calls as usize) {
        let call = Token::new(nth as u64);
        let up = fleet.step(Event::Relay { channel, run, attempt, call, body: Token::new(nth as u64) });
        let Some(Request::Relay { reply_to, .. }) = up.into_iter().next() else {
            panic!("a call relayed up");
        };
        calls.push(reply_to);
    }
    assert_eq!(fleet.domain.workers(), limits.workers);
    assert_eq!(fleet.domain.attempts(), limits.attempts);
    assert_eq!(fleet.domain.calls(), limits.calls);
    (fleet, placed, calls)
}

#[test]
fn a_fleet_full_to_its_limits_stays_within_its_worst_case() {
    let (mut fleet, placed, calls) = fill(LIMITS);
    // At its fullest, the fleet holds a fair share of the bound: it is not
    // padded beyond use.
    let fullest = fleet.meter.held();
    assert!(fullest.saturating_mul(3) > fleet.bound, "{fullest} held of a worst case of {}", fleet.bound);
    // One more is refused, at the entrance.
    let (run, attempt) = (fleet.name(), fleet.name());
    let workstream = run.raw();
    let refused = fleet.step(Event::Start { reply_to: ReplyTo::new(attempt), run, attempt, workstream });
    assert!(matches!(refused[..], [Request::Refused { .. }]), "{refused:?}");
    let up = fleet.step(Event::Relay {
        channel: placed[5].0,
        run: placed[5].1,
        attempt: placed[5].2,
        call: Token::new(9),
        body: Token::new(9),
    });
    assert!(matches!(up[..], [Request::Drop { .. }]), "a call beyond the room is dropped: {up:?}");
    // Drained: every call answered, every attempt answered or lost.
    for (nth, to) in calls.into_iter().enumerate() {
        let down = fleet.step(Event::Relayed { to, answer: Token::new(100 + nth as u64) });
        assert!(matches!(down[..], [Request::Relayed { .. }]), "{down:?}");
    }
    // Each attempt answered, and its answer made durable, frees a slot for
    // one waiting.
    let mut out = placed;
    while !out.is_empty() {
        for (channel, run, attempt) in out {
            let payload = Token::new(200 + attempt.raw());
            fleet.step(Event::Answer { channel, run, attempt, answer: Answer::Ended, payload });
            fleet.step(Event::Acknowledge { run, attempt });
        }
        out = self::placed(&fleet.settle());
    }
    assert_eq!((fleet.domain.attempts(), fleet.domain.calls()), (0, 0));
}

#[test]
fn every_entry_point_stays_within_the_worst_case() {
    let mut fleet = Measured::new(LIMITS);
    // Hellos: a worker with strays and answers it holds, before the parent
    // has loaded its claims.
    let (r1, a1, r2, a2) = (fleet.name(), fleet.name(), fleet.name(), fleet.name());
    let hosting = vec![
        Hosted { run: r1, attempt: a1, phase: Phase::Active },
        Hosted { run: r2, attempt: a2, phase: Phase::Answered },
    ];
    let listed = fleet.hello(channel(0), hosting);
    assert!(matches!(listed[..], [Request::Listed { .. }, Request::Listed { .. }]), "{listed:?}");
    let answer =
        Event::Answer { channel: channel(0), run: r2, attempt: a2, answer: Answer::Ended, payload: Token::new(1) };
    fleet.step(answer);
    fleet.step(Event::Loaded);
    fleet.step(Event::Adopt { reply_to: ReplyTo::new(a2), run: r2, attempt: a2, kept: 0 });
    fleet.step(Event::Acknowledge { run: r2, attempt: a2 });
    fleet.step(Event::Adopt { reply_to: ReplyTo::new(a1), run: r1, attempt: a1, kept: 0 });
    let (r3, a3) = fleet.start();
    let placed = placed(&fleet.settle());
    assert_eq!(placed.len(), 1);
    // Calls, answers, inbound events, bounces and facts.
    let up = fleet.step(Event::Relay {
        channel: placed[0].0,
        run: r3,
        attempt: a3,
        call: Token::new(1),
        body: Token::new(2),
    });
    let Some(Request::Relay { reply_to, .. }) = up.into_iter().next() else {
        panic!("a call relayed up");
    };
    fleet.step(Event::Relayed { to: reply_to, answer: Token::new(3) });
    fleet.step(Event::Inbound { run: r3, attempt: a3, event: Token::new(4) });
    fleet.step(Event::Bounced {
        channel: placed[0].0,
        name: Token::new(0),
        run: r3,
        attempt: a3,
        bounce: Bounce::Full,
    });
    fleet.step(Event::Told { channel: placed[0].0, run: r3, attempt: a3, fact: Token::new(5) });
    fleet.step(Event::Cancel { run: r1, attempt: a1 });
    // A busy refusal, placed again.
    let (channel_of, ..) = placed[0];
    let busy =
        Event::Answer { channel: channel_of, run: r3, attempt: a3, answer: Answer::Busy, payload: Token::new(6) };
    fleet.step(busy);
    fleet.settle();
    // A worker lost, back on another channel, and lost for good.
    fleet.step(Event::Lost { channel: channel(0) });
    let hosting = vec![Hosted { run: r3, attempt: a3, phase: Phase::Active }];
    fleet.hello(channel(1), hosting);
    for raw in [7, 8] {
        let answer = Event::Answer {
            channel: channel(1),
            run: r3,
            attempt: a3,
            answer: Answer::Failed,
            payload: Token::new(raw),
        };
        fleet.step(answer);
    }
    fleet.step(Event::Acknowledge { run: r3, attempt: a3 });
    fleet.at(30);
    fleet.settle();
    assert_eq!((fleet.domain.attempts(), fleet.domain.calls()), (0, 0));
}

#[test]
fn held_and_handed_turn_admissions_stay_within_the_worst_case() {
    let limits = Limits { turns: 8, ..LIMITS };
    let mut fleet = Measured::new(limits);
    let (run, attempt) = (fleet.name(), fleet.name());
    fleet.hello(channel(0), vec![Hosted { run, attempt, phase: Phase::Active }]);
    for turn in 1..=limits.turns {
        let out =
            fleet.step(Event::Turn { channel: channel(0), run, attempt, turn, body: Token::new(u64::from(turn)) });
        assert!(out.is_empty(), "a stray holds its turn for adoption");
    }
    let refused = fleet.step(Event::Turn { channel: channel(0), run, attempt, turn: 9, body: Token::new(9) });
    assert!(matches!(refused[..], [Request::TurnBusy { .. }, Request::Drop { .. }]));
    fleet.step(Event::Adopt { reply_to: ReplyTo::new(attempt), run, attempt, kept: 3 });
    let released = fleet.settle();
    assert_eq!(released.iter().filter(|request| matches!(request, Request::Turned { .. })).count(), 5);
    for turn in 4..=limits.turns {
        fleet.step(Event::TurnKept { run, attempt, turn });
    }
    // Parent pressure frees the remaining admission for retry as well.
    fleet.step(Event::Turn { channel: channel(0), run, attempt, turn: 9, body: Token::new(90) });
    fleet.step(Event::TurnBusy { run, attempt, turn: 9 });
    fleet.step(Event::Turn { channel: channel(0), run, attempt, turn: 9, body: Token::new(91) });
    fleet.step(Event::TurnKept { run, attempt, turn: 9 });
    fleet.step(Event::Answer { channel: channel(0), run, attempt, answer: Answer::Ended, payload: Token::new(99) });
    fleet.step(Event::Acknowledge { run, attempt });
    fleet.settle();
    assert_eq!(fleet.domain.attempts(), 0);
}
