//! A turn scenario world beside the unchanged first-version world. Its fake
//! worker owns numbered opaque turns until acknowledged; the fake parent
//! commits them in order, refuses under pressure, and restores its durable
//! prefix after restart. The referee sees only their boundary observations.

use std::collections::{BTreeMap, BTreeSet};

use skein_lib::{Duration, Env, Queue, ReplyTo, Rng, Time, Token, Wall};
use jig_core_fleet::{self as fleet, Answer, Domain, Event, Hello, Hosted, Limits, Phase, Request};
use skein_world::domain::{Expectations, Judge, Referee};

#[derive(Clone, Copy, Debug)]
pub enum Seen {
    Produced { turn: u32, body: u64 },
    Handed { turn: u32, body: u64 },
    Busy { turn: u32 },
    Committed { turn: u32 },
    Forgot { turn: u32 },
    Restart,
    Fenced,
    Hello { bound: Duration, grace: Duration },
    Refused,
}

#[derive(Debug, Default)]
pub struct Turns {
    bodies: BTreeMap<u32, u64>,
    pending: BTreeSet<u32>,
    kept: u32,
    fenced: bool,
    refuses: bool,
}

impl Expectations for Turns {
    type Seen = Seen;
    type Name = u32;
    type Stimulus = ();

    fn observe(&mut self, seen: Seen, judge: &mut Judge<u32, ()>) {
        match seen {
            Seen::Produced { turn, body } => {
                self.bodies.insert(turn, body);
                judge.expect(turn, Duration::from_secs(5));
            }
            Seen::Handed { turn, body } => {
                judge.check(!self.fenced, format_args!("no fenced turn is handed to the parent"));
                judge.check(turn > self.kept, format_args!("a committed turn is not handed again, across restart too"));
                judge.check(
                    self.pending.insert(turn),
                    format_args!("an admitted turn is handed once until kept or busy"),
                );
                judge.check(self.bodies.get(&turn) == Some(&body), format_args!("the opaque turn body is preserved"));
            }
            Seen::Busy { turn } => {
                self.pending.remove(&turn);
            }
            Seen::Committed { turn } => {
                judge.check(self.pending.remove(&turn), format_args!("commitment follows admission"));
                judge.check(turn == self.kept + 1, format_args!("the parent's transcript is a contiguous prefix"));
                self.kept = turn;
            }
            Seen::Forgot { turn } => {
                judge.check(
                    turn <= self.kept || self.fenced,
                    format_args!("a worker forgets only a committed or fenced turn"),
                );
                judge.meet(&turn);
            }
            Seen::Restart => {
                self.pending.clear();
            }
            Seen::Hello { bound, grace } => {
                self.refuses = bound >= grace;
                if self.refuses {
                    judge.expect(0, Duration::from_secs(1));
                }
            }
            Seen::Refused => {
                judge.check(self.refuses, format_args!("a worker within the declared grace is accepted"));
                judge.meet(&0);
            }
            Seen::Fenced => {
                self.fenced = true;
                self.pending.clear();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub seed: u64,
    pub drops: bool,
    pub restart: bool,
    pub cancel: bool,
    pub facts: u32,
    pub capacity: u32,
    pub stop_bound: Duration,
}

impl Settings {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            seed,
            drops: true,
            restart: true,
            cancel: false,
            facts: 4,
            capacity: 2,
            stop_bound: Duration::from_secs(10),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
pub struct Report {
    pub trace: Vec<String>,
    pub committed: u32,
    pub capacity_busy: u32,
    pub parent_busy: u32,
    pub replays: u32,
    pub refused: bool,
}

#[derive(Debug)]
struct Held {
    body: u64,
    send_at: Option<u64>,
}

struct World {
    settings: Settings,
    rng: Rng,
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
    worker: BTreeMap<u32, Held>,
    parent: BTreeMap<u32, u64>,
    payloads: BTreeMap<Token, (u32, u64)>,
    serial: u64,
    channel: Option<Token>,
    channels: u64,
    loaded: bool,
    fenced: bool,
    kept: u32,
    tick: u64,
    referee: Referee<Turns>,
    report: Report,
}

const RUN: Token = Token::new(1);
const ATTEMPT: Token = Token::new(11);
const COUNT: u32 = 8;

impl World {
    fn new(settings: Settings) -> Self {
        let limits = Limits {
            turns: settings.capacity,
            facts: settings.facts,
            workers: 1,
            slots: 1,
            attempts: 1,
            ..super::LIMITS
        };
        Self {
            rng: Rng::new(settings.seed),
            domain: Domain::new(&limits),
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            out: Queue::with_capacity(fleet::max_out(&limits)),
            worker: BTreeMap::new(),
            parent: BTreeMap::new(),
            payloads: BTreeMap::new(),
            serial: 100,
            channel: None,
            channels: 20,
            loaded: false,
            fenced: false,
            kept: 0,
            tick: 0,
            referee: Referee::new(Turns::default()),
            report: Report {
                trace: Vec::new(),
                committed: 0,
                capacity_busy: 0,
                parent_busy: 0,
                replays: 0,
                refused: false,
            },
            settings,
        }
    }

    fn see(&mut self, seen: Seen) {
        self.referee.observe(self.env.now, seen, &mut Vec::new());
        self.referee.assert_holding(self.settings.seed);
    }

    fn step(&mut self, event: Event) {
        self.report.trace.push(format!("{} {event:?}", self.tick));
        fleet::step(&mut self.domain, &self.env, event, &mut self.out);
        self.drain();
    }

    fn drain(&mut self) {
        while let Some(request) = self.out.pop() {
            self.report.trace.push(format!("{} {request:?}", self.tick));
            match request {
                Request::Turned { turn, body, .. } => {
                    let (number, words) = self.payloads.remove(&body).expect("one body handoff");
                    assert_eq!(number, turn);
                    self.see(Seen::Handed { turn, body: words });
                    // The parent cannot commit out of order. It refuses this
                    // admission at once rather than hold room ahead of a gap.
                    if turn != self.kept + 1 || self.rng.chance(300) {
                        self.report.parent_busy += 1;
                        self.see(Seen::Busy { turn });
                        fleet::step(
                            &mut self.domain,
                            &self.env,
                            Event::TurnBusy { run: RUN, attempt: ATTEMPT, turn },
                            &mut self.out,
                        );
                    } else {
                        assert!(self.parent.insert(turn, words).is_none());
                    }
                }
                Request::AcknowledgeTurn { turn, .. } => {
                    if self.worker.remove(&turn).is_some() {
                        self.see(Seen::Forgot { turn });
                    }
                }
                Request::TurnBusy { turn, .. } => {
                    self.report.capacity_busy += 1;
                    if let Some(held) = self.worker.get_mut(&turn) {
                        held.send_at = Some(self.tick + 1 + self.rng.below(5));
                    }
                }
                Request::Drop { payload } => {
                    self.payloads.remove(&payload).expect("one body handoff");
                }
                Request::Placed { .. }
                | Request::Listed { .. }
                | Request::Cancel { .. }
                | Request::Answered { .. }
                | Request::Acknowledge { .. } => {}
                Request::Refuse { .. } => {
                    self.report.refused = true;
                    self.channel = None;
                    self.see(Seen::Refused);
                }
                Request::Assign { .. }
                | Request::Grant { .. }
                | Request::Rejected { .. }
                | Request::Exhausted { .. }
                | Request::Inbound { .. }
                | Request::Relayed { .. }
                | Request::Lost { .. }
                | Request::Withdrawn { .. }
                | Request::Refused { .. }
                | Request::Relay { .. }
                | Request::Bounced { .. }
                | Request::Undelivered { .. }
                | Request::Told { .. } => panic!("unexpected turn-world request: {request:?}"),
            }
        }
        self.domain.reclaim();
        while self.domain.pop_fact().is_some() {}
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            if !self.domain.is_ready() {
                return;
            }
            fleet::resume(&mut self.domain, &self.env, &mut self.out);
            self.drain();
        }
        panic!("seed {}: ready work did not settle", self.settings.seed);
    }

    fn hello(&mut self) {
        self.channels += 1;
        let channel = Token::new(self.channels);
        self.channel = Some(channel);
        let hello = Hello {
            graces: Some(self.settings.stop_bound),
            slots: 1,
            workstreams: Box::default(),
            hosting: Box::from([Hosted { run: RUN, attempt: ATTEMPT, phase: Phase::Active }]),
        };
        self.see(Seen::Hello { bound: self.settings.stop_bound, grace: self.env.limits.grace });
        self.step(Event::Hello { channel, hello });
        for held in self.worker.values_mut() {
            held.send_at = Some(self.tick);
        }
    }

    fn adopt(&mut self) {
        self.step(Event::Adopt { reply_to: ReplyTo::new(ATTEMPT), run: RUN, attempt: ATTEMPT, kept: self.kept });
        self.step(Event::Loaded);
        self.loaded = true;
    }

    fn send(&mut self, turn: u32, body: u64) {
        let Some(channel) = self.channel else { return };
        self.serial += 1;
        let token = Token::new(self.serial);
        assert!(self.payloads.insert(token, (turn, body)).is_none());
        self.step(Event::Turn { channel, run: RUN, attempt: ATTEMPT, turn, body: token });
    }

    fn run(mut self) -> Report {
        self.hello();
        if self.report.refused {
            self.referee.assert_passed(self.settings.seed);
            return self.report;
        }
        for turn in 1..=COUNT {
            let body = 1000 + u64::from(turn);
            self.see(Seen::Produced { turn, body });
            self.worker.insert(turn, Held { body, send_at: Some(0) });
        }
        // Hello and replay may precede the store's claims, as at restart.
        for tick in 0..2000 {
            self.tick = tick;
            self.env.now = Time::ZERO.saturating_add(Duration::from_millis(tick));
            if tick == 2 {
                self.adopt();
            }
            if self.settings.drops && tick == 4 {
                let channel = self.channel.take().expect("contact before drop");
                self.step(Event::Lost { channel });
            }
            if self.settings.drops && tick == 7 {
                self.hello();
                self.report.replays += 1;
            }
            if self.settings.restart && tick == 10 {
                self.domain = Domain::new(&self.env.limits);
                self.channel = None;
                self.loaded = false;
                self.parent.clear();
                self.payloads.clear();
                self.see(Seen::Restart);
                self.hello();
                self.report.replays += 1;
            }
            if self.settings.restart && tick == 13 {
                self.adopt();
            }
            if self.settings.cancel && tick == 15 {
                self.fenced = true;
                self.parent.clear();
                self.see(Seen::Fenced);
                self.step(Event::Cancel { run: RUN, attempt: ATTEMPT });
                for held in self.worker.values_mut() {
                    held.send_at = Some(tick);
                }
            }
            self.settle();
            if self.channel.is_some() {
                let sends: Vec<_> = self
                    .worker
                    .iter_mut()
                    .filter_map(|(&number, held)| {
                        if held.send_at.is_some_and(|at| at <= tick) {
                            held.send_at = None;
                            Some((number, held.body))
                        } else {
                            None
                        }
                    })
                    .collect();
                for (number, body) in sends {
                    self.send(number, body);
                    if self.rng.chance(500) && self.worker.contains_key(&number) {
                        self.send(number, body);
                    }
                }
            }
            if self.loaded && !self.fenced && self.rng.chance(200) && self.parent.remove(&(self.kept + 1)).is_some() {
                self.kept += 1;
                self.see(Seen::Committed { turn: self.kept });
                self.step(Event::TurnKept { run: RUN, attempt: ATTEMPT, turn: self.kept });
            }
            if self.referee.is_due(self.env.now) {
                self.referee.fire(self.env.now, &mut Vec::new());
            }
            self.referee.assert_holding(self.settings.seed);
            if tick > 15 && self.worker.is_empty() {
                break;
            }
        }
        self.referee.assert_passed(self.settings.seed);
        assert!(self.worker.is_empty());
        assert!(self.parent.is_empty());
        self.step(Event::Answer {
            channel: self.channel.expect("contact at the end"),
            run: RUN,
            attempt: ATTEMPT,
            answer: Answer::Ended,
            payload: Token::new(9999),
        });
        self.step(Event::Acknowledge { run: RUN, attempt: ATTEMPT });
        self.settle();
        assert!(self.payloads.is_empty(), "every body was handed or dropped");
        assert_eq!(self.domain.attempts(), 0);
        assert_eq!(self.domain.turns(), 0);
        self.report.committed = self.kept;
        self.report
    }
}

#[must_use]
pub fn run(settings: Settings) -> Report {
    World::new(settings).run()
}
