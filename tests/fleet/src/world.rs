use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jig_core_fleet::{
    self as fleet, Answer, Domain, Event, Fact, Hello, Hosted, Limits, Phase, Request, Undelivered, Withdrawal,
};
use skein_lib::{Duration, ReplyTo, Rng, Time, Token};
use skein_world::domain::{Ledger, Referee, Schedule, Span, Stage, Trace};

use crate::referee::{Down, End, Fleet, Kind, Said, Seen, Stimulus};

/// Room in the fleet's output queue beyond what one step may emit. Small, so
/// the loop's flow control is exercised.
const SLACK: u32 = 2;

/// The most lines a trace keeps, and deliveries in flight: a world past
/// either fails with its seed rather than grow.
const MAX_TRACE: usize = 400_000;
const MAX_WIRE: usize = 20_000;

/// The fleet's limits in the calm world.
pub const LIMITS: Limits = Limits {
    workers: 4,
    engine_slots: 0,
    slots: 3,
    workstreams: 3,
    attempts: 16,
    calls: 8,
    call_name_bytes: 64,
    turns: 0,
    grace: Duration::from_secs(20),
    facts: 64,
};

/// How a world's attempts ended and what the fleet did on the way, by kind,
/// for the sweep.
pub const ENDINGS: [&str; 22] = [
    "ended",
    "parked",
    "failed",
    "invalid",
    "lost",
    "cancelled",
    "replaced",
    "refused",
    "busy",
    "found",
    "stray",
    "listed",
    "kept",
    "forgotten",
    "heard again",
    "fenced",
    "duplicate",
    "dropped",
    "undelivered",
    "bounced",
    "adopted",
    "turned away",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// Seeds the world.
    pub seed: u64,
    pub limits: Limits,
    /// Workers, and the most slots each has: each has between one and that.
    pub workers: u32,
    pub slots: u32,
    /// Items the parent works on, the time between their first starts, and
    /// the starts each may take at most (retries and wakes included).
    pub items: u32,
    pub item_gap: Span,
    pub starts: u32,
    /// How long a run takes; per mille those that park, fail, and are
    /// refused as invalid (the rest end).
    pub run: Span,
    pub parks: u32,
    pub fails: u32,
    pub invalid: u32,
    /// Per mille the assignments a worker hosting a run already refuses as
    /// busy, as if momentarily full by its own measure.
    pub busy: u32,
    /// How long the parent takes to make an answer durable before it
    /// acknowledges it.
    pub durable: Span,
    /// The time between a run's host calls; per mille those that come with
    /// a fact; how long a run waits for a call's answer, and how long the
    /// parent takes to give one.
    pub call_gap: Span,
    pub facts: u32,
    pub call_wait: Duration,
    pub serve: Span,
    /// Inbound events the parent sends a run once placed, and when; per
    /// mille those a worker bounces.
    pub inbound: u32,
    pub inbound_after: Span,
    pub bounces: u32,
    /// Per mille the attempts the parent cancels, and those it replaces with
    /// a newer one, and when; how long it waits for an attempt to end before
    /// cancelling it; how long it backs off before a retry.
    pub cancels: u32,
    pub replaces: u32,
    pub act_after: Span,
    pub timeout: Duration,
    pub backoff: Span,
    /// How long a cancelled run takes to wind down.
    pub wind_down: Span,
    /// How long a message takes on a channel, and a channel's closing to be
    /// noticed at either end; how long a worker waits to dial again.
    pub latency: Span,
    pub redial: Span,
    /// Channels dropped, at moments before `horizon`; per mille those whose
    /// worker never comes back, and how long the others stay away.
    pub drops: u32,
    pub horizon: Duration,
    pub never_back: u32,
    pub away: Span,
    /// Per mille the workers back at once, before the fleet hears they left.
    pub quick: u32,
    /// Engine restarts, before `horizon`, and how long the parent takes to
    /// read its claims back after one.
    pub restarts: u32,
    pub read: Span,
    /// How long a worker out of contact keeps its runs: no more than the
    /// fleet's grace less a channel's latency and a wind-down, so that what
    /// the fleet presumes lost has stopped.
    pub worker_grace: Duration,
}

impl Settings {
    /// A world where nothing goes wrong: a few workers, runs of every kind,
    /// calls answered in time, no channel dropping and no restart.
    #[must_use]
    pub fn calm(seed: u64) -> Settings {
        Settings {
            seed,
            limits: LIMITS,
            workers: 3,
            slots: 3,
            items: 12,
            item_gap: Span::millis(10, 2000),
            starts: 3,
            run: Span::millis(500, 5000),
            parks: 150,
            fails: 150,
            invalid: 0,
            busy: 0,
            durable: Span::millis(10, 2000),
            call_gap: Span::millis(200, 2000),
            facts: 300,
            call_wait: Duration::from_secs(5),
            serve: Span::millis(5, 500),
            inbound: 2,
            inbound_after: Span::millis(10, 3000),
            bounces: 0,
            cancels: 0,
            replaces: 0,
            act_after: Span::millis(100, 3000),
            timeout: Duration::from_secs(60),
            backoff: Span::millis(100, 2000),
            wind_down: Span::millis(10, 1000),
            latency: Span::millis(1, 50),
            redial: Span::millis(100, 1000),
            drops: 0,
            horizon: Duration::from_secs(30),
            never_back: 0,
            away: Span::millis(1000, 30_000),
            quick: 0,
            restarts: 0,
            read: Span::millis(100, 5000),
            worker_grace: Duration::from_secs(15),
        }
    }

    /// A world of its own for `seed`: workers that drop and come back within
    /// the grace or past it, or never; engine restarts; runs refused,
    /// cancelled and replaced; at chances drawn from the seed, under limits
    /// that are sometimes tight.
    #[must_use]
    pub fn random(seed: u64) -> Settings {
        let mut rng = Rng::new(seed ^ 0x5EED_0000_F1EE_7000);
        let mut chance = |most: u64| u32::try_from(rng.below(most + 1)).expect("a chance per mille");
        let calm = Settings::calm(seed);
        let workers = 2 + chance(3);
        // As many workers as the fleet takes, half the time: one back before
        // its loss is heard is turned away.
        let fewer = chance(1) == 0;
        Settings {
            workers: if fewer { 1 + chance(u64::from(workers) - 1) } else { workers },
            slots: 1 + chance(2),
            items: 4 + chance(16),
            item_gap: Span::millis(1, 100 + u64::from(chance(3000))),
            starts: 1 + chance(3),
            run: Span::millis(1, 100 + u64::from(chance(8000))),
            parks: chance(300),
            fails: chance(300),
            invalid: chance(100),
            busy: chance(150),
            durable: Span::millis(1, 10 + u64::from(chance(8000))),
            call_gap: Span::millis(10, 100 + u64::from(chance(3000))),
            facts: chance(1000),
            serve: Span::millis(1, 10 + u64::from(chance(6000))),
            inbound: chance(4),
            bounces: chance(300),
            cancels: chance(200),
            replaces: chance(150),
            timeout: Duration::from_secs(10 + u64::from(chance(50))),
            latency: Span::millis(1, 1 + u64::from(chance(500))),
            redial: Span::millis(1, 1 + u64::from(chance(1000))),
            drops: chance(4),
            never_back: chance(300),
            away: Span::millis(1, 1000 + u64::from(chance(30_000))),
            quick: chance(800),
            restarts: chance(1),
            read: Span::millis(1, 100 + u64::from(chance(30_000))),
            limits: Limits { workers, attempts: 4 + chance(12), calls: 1 + chance(6), facts: 4 + chance(60), ..LIMITS },
            ..calm
        }
    }

    /// How long an attempt may take to end, from its start, under the
    /// world's faults: the parent's patience, a read after a restart and the
    /// patience again after its adoption, and a grace and a wind-down for
    /// each channel dropped or restart on the way.
    #[must_use]
    pub fn bound(&self) -> Duration {
        let faults = u64::from(self.drops + self.restarts + 1);
        let fault = self.limits.grace.saturating_add(self.latency.max.saturating_mul(4));
        self.timeout
            .saturating_mul(2)
            .saturating_add(self.read.max)
            .saturating_add(fault.saturating_mul(faults))
            .saturating_add(self.wind_down.max)
            .saturating_add(self.durable.max)
            .saturating_add(self.redial.max)
            .saturating_add(Duration::from_secs(1))
    }
}

/// What the world counted, as it crossed the boundary.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    /// Attempts started and adopted, and how they and the fleet's handling
    /// of them ended, by kind.
    pub starts: u32,
    pub adoptions: u32,
    pub endings: BTreeMap<&'static str, u32>,
    /// Assignments made, runs admitted, relayed calls and their answers,
    /// inbound events sent and delivered, facts told.
    pub assigned: u32,
    pub admitted: u32,
    pub calls: u32,
    pub replies: u32,
    pub inbound: u32,
    pub delivered: u32,
    pub told: u32,
    /// Attempts presumed lost that a worker had admitted.
    pub lost_hosted: u32,
    /// Hellos said, workers turned away, channels dropped, restarts.
    pub hellos: u32,
    pub refused: u32,
    pub drops: u32,
    pub restarts: u32,
    /// The fleet's facts.
    pub facts: u32,
}

/// Something on its way, delivered at its time.
#[derive(Debug)]
pub(crate) enum Delivery {
    /// A worker's message reaches the fleet, if its channel is still open.
    Up {
        channel: u64,
        up: Up,
    },
    /// A message from the fleet reaches a worker, if its channel is still
    /// open.
    Down {
        channel: u64,
        down: Message,
    },
    /// The protocol tells the fleet a channel closed.
    Lost {
        channel: u64,
    },
    /// A worker notices its channel closed.
    Noticed {
        worker: usize,
        channel: u64,
    },
    /// A worker dials: a new channel opens.
    Dial {
        worker: usize,
    },
    /// A hosted run ends as it planned, or, cancelled, has wound down.
    Ends {
        worker: usize,
        run: u64,
        attempt: u64,
    },
    Stops {
        worker: usize,
        run: u64,
        attempt: u64,
    },
    /// A hosted run makes a host call, and withdraws it past its deadline.
    Call {
        worker: usize,
        run: u64,
        attempt: u64,
    },
    Withdraw {
        worker: usize,
        run: u64,
        attempt: u64,
        call: u64,
    },
    /// A worker's grace out of contact passes: it cancels its runs.
    Grace {
        worker: usize,
        epoch: u64,
    },
    /// The parent starts an item's next attempt.
    Start {
        item: usize,
    },
    /// The parent gives up on an attempt, cancels it, replaces it, or sends
    /// it an inbound event.
    Timeout {
        item: usize,
        attempt: u64,
    },
    Act {
        item: usize,
        attempt: u64,
        act: Act,
    },
    Send {
        item: usize,
        attempt: u64,
    },
    /// The parent answers a relayed call of the fleet incarnation `epoch`.
    Reply {
        epoch: u64,
        reply_to: ReplyTo,
    },
    /// The parent has read its claims back after a restart.
    Adopt {
        epoch: u64,
    },
    /// The parent has made an attempt's answer durable.
    Durable {
        epoch: u64,
        item: usize,
        attempt: u64,
    },
}

/// What a worker says up its channel.
#[derive(Debug)]
pub(crate) enum Up {
    Hello { slots: u32, workstreams: Vec<u64>, hosting: Vec<(u64, u64, Phase)> },
    Answer { run: u64, attempt: u64, said: Said },
    Relay { run: u64, attempt: u64, call: u64 },
    Bounced { run: u64, attempt: u64 },
    Told { run: u64, attempt: u64 },
}

/// What the fleet says down a worker's channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Message {
    Assign { run: u64, attempt: u64 },
    Inbound { run: u64, attempt: u64 },
    Cancel { run: u64, attempt: u64 },
    Relayed { run: u64, attempt: u64, call: u64 },
    Acknowledge { run: u64, attempt: u64 },
}

/// What the parent does to an attempt, by chance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Act {
    Cancel,
    Replace,
}

/// A payload the parent handed the fleet, named by token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Payload {
    Answer(Said),
    Inbound,
    Body { call: u64 },
    Reply,
    Fact,
}

/// A channel, as the network keeps it: whose it is, whether it is open, the
/// fleet incarnation it was opened under, and when the last message each way
/// is due, which keeps them in order.
#[derive(Debug)]
pub(crate) struct Channel {
    pub(crate) worker: usize,
    pub(crate) open: bool,
    pub(crate) epoch: u64,
    pub(crate) up_at: Time,
    pub(crate) down_at: Time,
}

/// A scripted worker.
#[derive(Debug)]
pub(crate) struct Worker {
    pub(crate) slots: u32,
    /// The workstreams its workspaces hold, the latest last.
    pub(crate) cache: VecDeque<u64>,
    /// The runs it hosts: live, or winding down.
    pub(crate) runs: BTreeMap<(u64, u64), Hosting>,
    /// The answers it keeps until the engine acknowledges them.
    pub(crate) held: BTreeMap<(u64, u64), Said>,
    /// Its channel, while it knows it is open.
    pub(crate) channel: Option<u64>,
    /// Counts its channels lost, which names its graces.
    pub(crate) epoch: u64,
    /// When it dials again after its next loss.
    pub(crate) back: Back,
    /// It will never dial again.
    pub(crate) gone: bool,
}

/// When a worker dials again once it notices its channel closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Back {
    /// After a backoff drawn from `Settings::redial`.
    Soon,
    /// After this long.
    After(Duration),
    /// Never: the worker is gone for good.
    Never,
}

/// A run a worker hosts.
#[derive(Debug)]
pub(crate) struct Hosting {
    /// How it ends, unless it is cancelled first.
    pub(crate) plan: Kind,
    pub(crate) cancelled: bool,
    /// Its host calls waiting for an answer.
    pub(crate) calls: Vec<u64>,
}

/// An item of the parent's: a run, the attempts whose calls are open, the
/// newest of which is its claim, and the starts it has left.
#[derive(Debug)]
struct Item {
    run: u64,
    starts: u32,
    open: BTreeMap<u64, Open>,
    claim: Option<u64>,
    done: bool,
}

/// An attempt whose call is open, as the parent's record keeps it.
#[derive(Clone, Copy, Debug)]
struct Open {
    cancelled: bool,
    /// Its answer, heard and not yet durable.
    answered: Option<Kind>,
    /// Made before the engine restarted, and not adopted since: the fleet
    /// does not know it.
    stale: bool,
}

pub struct World {
    pub(crate) now: Time,
    pub(crate) rng: Rng,
    pub(crate) settings: Settings,

    domain: Domain,
    pub(crate) stage: Stage<Limits, Event, Request>,
    /// Counts the engine's restarts: the fleet incarnation.
    pub(crate) epoch: u64,
    /// The parent has loaded its claims in this incarnation: it starts
    /// attempts only once it has.
    loaded: bool,
    /// Attempts adopted in this iteration, whose kept answer comes at once.
    adopting: Vec<u64>,

    /// Deliveries in flight, whose count names runs, attempts, channels,
    /// calls, payloads and answers too.
    pub(crate) wire: Schedule<Delivery>,
    pub(crate) in_flight: usize,
    pub(crate) channels: BTreeMap<u64, Channel>,
    pub(crate) workers: Vec<Worker>,
    items: Vec<Item>,
    /// The parent's starts and adoptions not yet ended, by attempt; the
    /// fleet's relayed calls not yet answered; the payloads not yet echoed.
    calls: Ledger<u64, ()>,
    relays: Ledger<u64, ()>,
    pub(crate) payloads: Ledger<u64, Payload>,
    /// Attempts a worker admitted.
    pub(crate) admitted: BTreeSet<(u64, u64)>,

    pub(crate) referee: Referee<Fleet>,
    pub(crate) stats: Stats,
    trace: Trace,
}

impl World {
    #[must_use]
    pub fn new(settings: Settings) -> World {
        assert!(fleet::worst_case(&settings.limits).is_some(), "the shell refuses limits it cannot provision");
        let unseen = settings.latency.max.saturating_add(settings.wind_down.max);
        assert!(
            settings.worker_grace.saturating_add(unseen) <= settings.limits.grace,
            "a worker cancels its runs, and they stop, before the fleet presumes them lost"
        );
        assert!(settings.slots <= settings.limits.slots, "a worker hosts no more runs than the fleet's limit");
        assert!(settings.workers <= settings.limits.workers, "a worker turned away would dial for ever");
        let max_out = fleet::max_out(&settings.limits);
        let mut world = World {
            now: Time::ZERO,
            rng: Rng::new(settings.seed),
            domain: Domain::new(&settings.limits),
            stage: Stage::new(settings.limits, max_out, max_out + SLACK),
            epoch: 0,
            loaded: true,
            adopting: Vec::new(),
            wire: Schedule::new(),
            in_flight: 0,
            channels: BTreeMap::new(),
            workers: Vec::new(),
            items: Vec::new(),
            calls: Ledger::new("start or adoption"),
            relays: Ledger::new("relayed call"),
            payloads: Ledger::new("payload"),
            admitted: BTreeSet::new(),
            referee: Referee::new(Fleet::new(settings.bound())),
            stats: Stats::default(),
            trace: Trace::default(),
            settings,
        };
        for worker in 0..settings.workers {
            let slots = 1 + world.below(settings.slots);
            world.workers.push(Worker {
                slots,
                cache: VecDeque::new(),
                runs: BTreeMap::new(),
                held: BTreeMap::new(),
                channel: None,
                epoch: 0,
                back: Back::Soon,
                gone: false,
            });
            let at = settings.redial.draw(&mut world.rng);
            let worker = usize::try_from(worker).expect("a worker's index");
            world.send(Time::ZERO.saturating_add(at), Delivery::Dial { worker });
        }
        let mut at = Time::ZERO;
        for item in 0..settings.items {
            let run = world.wire.name();
            world.items.push(Item { run, starts: settings.starts, open: BTreeMap::new(), claim: None, done: false });
            at = at.saturating_add(settings.item_gap.draw(&mut world.rng));
            world.send(at, Delivery::Start { item: usize::try_from(item).expect("an item's index") });
        }
        for _ in 0..settings.drops {
            let at = world.moment();
            let worker = usize::try_from(world.below(settings.workers)).expect("a worker's index");
            let back = if world.rng.chance(settings.never_back) {
                None
            } else if world.rng.chance(settings.quick) {
                Some(Duration::from_millis(1))
            } else {
                Some(settings.away.draw(&mut world.rng))
            };
            world.referee.inject(at, Stimulus::Drop { worker, back });
        }
        for _ in 0..settings.restarts {
            let at = world.moment();
            world.referee.inject(at, Stimulus::Restart);
        }
        // A first start's cold read finds no claim.
        world.stage.push(Event::Loaded);
        world
    }

    #[must_use]
    pub fn now(&self) -> Time {
        self.now
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats.clone()
    }

    /// What crossed between the fleet and the world, in order, with times.
    #[must_use]
    pub fn trace(&self) -> &[String] {
        self.trace.lines()
    }

    /// Ends and messages the referee judged.
    #[must_use]
    pub fn judged(&self) -> (u64, u64) {
        let fleet = self.referee.expectations();
        (fleet.ends, fleet.sent)
    }

    /// Runs until nothing is left to happen, then checks the invariants of a
    /// settled world. Panics if it takes more than `iterations`.
    pub fn run(&mut self, iterations: u32) {
        for _ in 0..iterations {
            self.iterate();
            if self.has_work_now() {
                continue;
            }
            let Some(next) = self.next_time() else {
                self.assert_settled();
                return;
            };
            assert!(next > self.now, "time moves forward");
            self.now = next;
        }
        panic!("seed {}: the world did not settle in {iterations} iterations", self.settings.seed);
    }

    /// One iteration of the loop, as the shell would run it.
    fn iterate(&mut self) {
        let now = self.now;
        self.stage.tick(now);
        while let Some(delivery) = self.wire.next(now) {
            self.in_flight -= 1;
            self.deliver(delivery);
        }
        if self.referee.is_due(now) {
            let mut stimuli = Vec::new();
            self.referee.fire(now, &mut stimuli);
            self.referee.assert_holding(self.settings.seed);
            for stimulus in stimuli {
                self.inject(stimulus);
            }
        }
        while self.stage.has_room() && self.domain.is_ready() {
            fleet::resume(&mut self.domain, &self.stage.env, &mut self.stage.out);
        }
        while let Some(event) = self.stage.next_event() {
            self.log(format!("fleet <- {}", describe(&event)));
            fleet::step(&mut self.domain, &self.stage.env, event, &mut self.stage.out);
        }
        while self.stage.has_room() && self.domain.is_due(now) {
            fleet::fire(&mut self.domain, &self.stage.env, &mut self.stage.out);
        }
        // What the steps asked for, at the end of the iteration.
        while let Some(request) = self.stage.out.pop() {
            self.log(format!("fleet -> {request:?}"));
            self.request(request);
        }
        while let Some(fact) = self.domain.pop_fact() {
            self.stats.facts += 1;
            self.fact(fact);
        }
        self.referee.assert_holding(self.settings.seed);
        self.adopting.clear();
        // The reclaim point.
        self.domain.reclaim();
    }

    fn has_work_now(&self) -> bool {
        self.stage.has_events() || self.domain.is_ready() || self.wire.is_due(self.now)
    }

    fn next_time(&self) -> Option<Time> {
        [self.wire.next_time(), self.domain.next_deadline(), self.referee.next_deadline()].into_iter().flatten().min()
    }

    /// Checks the invariants of a settled world.
    fn assert_settled(&self) {
        let seed = self.settings.seed;
        assert!(self.wire.is_empty() && !self.stage.has_events(), "seed {seed}: nothing is in flight");
        assert_eq!(self.domain.attempts(), 0, "seed {seed}: the fleet tracks no attempt once settled");
        assert_eq!(self.domain.calls(), 0, "seed {seed}: the fleet keeps no relayed call once settled");
        assert!(self.domain.next_deadline().is_none(), "seed {seed}: no alarm is left");
        self.calls.assert_settled();
        self.relays.assert_settled();
        self.payloads.assert_settled();
        for (index, worker) in self.workers.iter().enumerate() {
            assert!(worker.runs.is_empty(), "seed {seed}: worker {index} hosts nothing once settled");
            if !worker.gone {
                assert!(worker.held.is_empty(), "seed {seed}: worker {index} in contact holds no answer");
            }
        }
        for item in &self.items {
            assert!(item.open.is_empty() && item.done, "seed {seed}: every item is done: {item:?}");
        }
        self.referee.assert_passed(seed);
    }

    // The world's own machinery.

    /// Sends `delivery`, due at `at`; a world with too much in flight fails.
    pub(crate) fn send(&mut self, at: Time, delivery: Delivery) {
        self.in_flight += 1;
        assert!(
            self.in_flight <= MAX_WIRE,
            "seed {}: no more than {MAX_WIRE} deliveries in flight",
            self.settings.seed
        );
        self.wire.send(at, delivery);
    }

    pub(crate) fn after(&mut self, span: Span, delivery: Delivery) {
        let at = self.now.saturating_add(span.draw(&mut self.rng));
        self.send(at, delivery);
    }

    pub(crate) fn log(&mut self, line: String) {
        assert!(
            self.trace.lines().len() < MAX_TRACE,
            "seed {}: a trace keeps no more than {MAX_TRACE} lines",
            self.settings.seed
        );
        self.trace.log(self.now, line);
    }

    pub(crate) fn observe(&mut self, seen: Seen) {
        let mut stimuli = Vec::new();
        self.referee.observe(self.now, seen, &mut stimuli);
        for stimulus in stimuli {
            self.inject(stimulus);
        }
    }

    pub(crate) fn end(&mut self, ending: &'static str) {
        *self.stats.endings.entry(ending).or_default() += 1;
    }

    pub(crate) fn below(&mut self, bound: u32) -> u32 {
        u32::try_from(self.rng.below(u64::from(bound))).expect("below a u32")
    }

    /// A moment before the horizon, for a stimulus.
    fn moment(&mut self) -> Time {
        let horizon = self.settings.horizon.as_nanos();
        Time::from_nanos(self.rng.between(horizon / 10, horizon))
    }

    /// Names a payload the parent hands the fleet.
    pub(crate) fn payload(&mut self, payload: Payload) -> Token {
        let token = self.wire.name();
        self.payloads.open(token, payload);
        Token::new(token)
    }

    // What the stimuli do.

    fn inject(&mut self, stimulus: Stimulus) {
        self.log(format!("referee: {stimulus:?}"));
        match stimulus {
            Stimulus::Drop { worker, back } => {
                let Some(channel) = self.workers[worker].channel else {
                    return;
                };
                if !self.channels[&channel].open {
                    return;
                }
                self.stats.drops += 1;
                self.workers[worker].back = match back {
                    Some(after) => Back::After(after),
                    None => Back::Never,
                };
                self.close(channel);
            }
            Stimulus::Restart => self.restart(),
        }
    }

    /// The engine restarts: its fleet knows nothing, every channel closes,
    /// and the parent reads its claims back after a while.
    fn restart(&mut self) {
        self.stats.restarts += 1;
        self.epoch += 1;
        self.loaded = false;
        self.domain = Domain::new(&self.settings.limits);
        self.stage.inbox.clear();
        self.calls = Ledger::new("start or adoption");
        self.relays = Ledger::new("relayed call");
        self.payloads = Ledger::new("payload");
        for item in &mut self.items {
            for open in item.open.values_mut() {
                open.stale = true;
            }
        }
        let open: Vec<u64> =
            self.channels.iter().filter(|(_, channel)| channel.open).map(|(&token, _)| token).collect();
        for channel in open {
            self.close(channel);
        }
        let epoch = self.epoch;
        self.after(self.settings.read, Delivery::Adopt { epoch });
    }

    /// Closes `channel`: what is in flight on it is lost, the fleet and the
    /// worker hear of it after a latency.
    pub(crate) fn close(&mut self, channel: u64) {
        let entry = self.channels.get_mut(&channel).expect("a channel the world opened");
        assert!(entry.open, "a channel closes once");
        entry.open = false;
        let worker = entry.worker;
        self.after(self.settings.latency, Delivery::Lost { channel });
        self.after(self.settings.latency, Delivery::Noticed { worker, channel });
    }

    // Deliveries.

    fn deliver(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Up { channel, up } => {
                if self.channels[&channel].open {
                    self.up(channel, up);
                }
            }
            Delivery::Down { channel, down } => {
                if self.channels[&channel].open {
                    let worker = self.channels[&channel].worker;
                    self.receive(worker, channel, down);
                }
            }
            Delivery::Lost { channel } => {
                if self.channels[&channel].epoch == self.epoch {
                    self.stage.push(Event::Lost { channel: Token::new(channel) });
                }
            }
            Delivery::Noticed { worker, channel } => self.noticed(worker, channel),
            Delivery::Dial { worker } => self.dial(worker),
            Delivery::Ends { worker, run, attempt } => self.ends(worker, run, attempt),
            Delivery::Stops { worker, run, attempt } => self.stops(worker, run, attempt),
            Delivery::Call { worker, run, attempt } => self.calls_up(worker, run, attempt),
            Delivery::Withdraw { worker, run, attempt, call } => self.withdraw(worker, run, attempt, call),
            Delivery::Grace { worker, epoch } => self.grace(worker, epoch),
            Delivery::Start { item } => self.start(item),
            Delivery::Timeout { item, attempt } => self.give_up(item, attempt),
            Delivery::Act { item, attempt, act } => self.act(item, attempt, act),
            Delivery::Send { item, attempt } => self.send_inbound(item, attempt),
            Delivery::Reply { epoch, reply_to } => {
                if epoch == self.epoch {
                    let token = reply_to.into_token();
                    self.relays.end(token.raw());
                    self.stats.replies += 1;
                    let answer = self.payload(Payload::Reply);
                    self.stage.push(Event::Relayed { to: ReplyTo::new(token), answer });
                }
            }
            Delivery::Adopt { epoch } => {
                if epoch == self.epoch {
                    self.adopt();
                }
            }
            Delivery::Durable { epoch, item, attempt } => {
                if epoch == self.epoch {
                    self.durable(item, attempt);
                }
            }
        }
    }

    /// A worker's message reaches the fleet, as the protocol would decode it.
    fn up(&mut self, channel: u64, up: Up) {
        let token = Token::new(channel);
        let event = match up {
            Up::Hello { slots, workstreams, hosting } => {
                let workstreams = workstreams.into();
                let hosting = hosting
                    .into_iter()
                    .map(|(run, attempt, phase)| Hosted { run: Token::new(run), attempt: Token::new(attempt), phase })
                    .collect();
                Event::Hello {
                    channel: token,
                    hello: Hello { slots, workstreams, hosting, stop_bound: Duration::from_secs(0) },
                }
            }
            Up::Answer { run, attempt, said } => {
                let payload = self.payload(Payload::Answer(said));
                let answer = match said.kind {
                    Kind::Ended => Answer::Ended,
                    Kind::Parked => Answer::Parked,
                    Kind::Failed => Answer::Failed,
                    Kind::Busy => Answer::Busy,
                    Kind::Invalid => Answer::Invalid,
                };
                Event::Answer { channel: token, run: Token::new(run), attempt: Token::new(attempt), answer, payload }
            }
            Up::Relay { run, attempt, call } => {
                let body = self.payload(Payload::Body { call });
                Event::Relay {
                    channel: token,
                    run: Token::new(run),
                    attempt: Token::new(attempt),
                    call: Token::new(call),
                    body,
                }
            }
            Up::Bounced { run, attempt } => Event::Bounced {
                channel: token,
                name: Token::new(0),
                run: Token::new(run),
                attempt: Token::new(attempt),
                bounce: jig_core_fleet::Bounce::Full,
            },
            Up::Told { run, attempt } => {
                let fact = self.payload(Payload::Fact);
                Event::Told { channel: token, run: Token::new(run), attempt: Token::new(attempt), fact }
            }
        };
        self.stage.push(event);
    }

    /// Sends `down` to the worker of `channel`, after those sent before it.
    fn down(&mut self, channel: Token, down: Message) {
        let channel = channel.raw();
        let latency = self.settings.latency.draw(&mut self.rng);
        let entry = self.channels.get_mut(&channel).expect("the fleet names channels the world opened");
        let at = self.now.saturating_add(latency).max(entry.down_at);
        entry.down_at = at;
        self.send(at, Delivery::Down { channel, down });
    }

    // The fleet's requests.

    fn request(&mut self, request: Request) {
        match request {
            Request::AssignTyped { .. }
            | Request::InboundTyped { .. }
            | Request::RelayTyped { .. }
            | Request::RelayedTyped { .. }
            | Request::DropTyped { .. }
            | Request::UndeliveredTyped { .. }
            | Request::Turned { .. }
            | Request::AcknowledgeTurn { .. }
            | Request::TurnBusy { .. } => {
                unreachable!("the first-version world sends no typed records or turns")
            }
            Request::Grant { .. } | Request::Rejected { .. } | Request::Exhausted { .. } => {}
            Request::Assign { channel, kind: _, run, attempt } => {
                self.stats.assigned += 1;
                self.sent(run, attempt, Down::Assign);
                self.down(channel, Message::Assign { run: run.raw(), attempt: attempt.raw() });
            }
            Request::Inbound { channel, run, attempt, event } => {
                assert_eq!(self.payloads.end(event.raw()), Payload::Inbound, "an inbound event goes down as it is");
                self.stats.delivered += 1;
                self.sent(run, attempt, Down::Inbound);
                self.down(channel, Message::Inbound { run: run.raw(), attempt: attempt.raw() });
            }
            Request::Cancel { channel, run, attempt } => {
                self.sent(run, attempt, Down::Cancel);
                self.down(channel, Message::Cancel { run: run.raw(), attempt: attempt.raw() });
            }
            Request::Relayed { channel, run, attempt, call, answer } => {
                assert_eq!(self.payloads.end(answer.raw()), Payload::Reply, "a call's answer goes down as it is");
                self.sent(run, attempt, Down::Relayed);
                self.down(channel, Message::Relayed { run: run.raw(), attempt: attempt.raw(), call: call.raw() });
            }
            Request::Acknowledge { channel, run, attempt } => {
                self.down(channel, Message::Acknowledge { run: run.raw(), attempt: attempt.raw() });
            }
            Request::Refuse { channel } => {
                self.stats.refused += 1;
                if self.channels[&channel.raw()].open {
                    self.close(channel.raw());
                }
            }
            Request::Placed { run, attempt } => self.placed(run.raw(), attempt.raw()),
            Request::Answered { to, run, attempt, answer: _, payload } => {
                let said = match self.payloads.end(payload.raw()) {
                    Payload::Answer(said) => said,
                    Payload::Inbound | Payload::Body { .. } | Payload::Reply | Payload::Fact => {
                        panic!("seed {}: an answer's payload is the worker's answer", self.settings.seed)
                    }
                };
                if self.adopting.contains(&attempt.raw()) {
                    self.end("kept");
                }
                self.ended(to, run, attempt, End::Answered(said));
            }
            Request::Listed { .. } => self.end("listed"),
            Request::NotStarted { .. } => panic!("worker-only world cannot lose an engine slot"),
            Request::Lost { to, run, attempt } => self.ended(to, run, attempt, End::Lost),
            Request::Withdrawn { to, run, attempt, withdrawal } => {
                let end = match withdrawal {
                    Withdrawal::Cancelled => End::Cancelled,
                    Withdrawal::Replaced => End::Replaced,
                };
                self.ended(to, run, attempt, end);
            }
            Request::Refused { to, run, attempt, refusal: _ } => self.ended(to, run, attempt, End::Refused),
            Request::Relay { reply_to, run: _, attempt: _, body } => {
                match self.payloads.end(body.raw()) {
                    Payload::Body { .. } => {}
                    Payload::Answer(_) | Payload::Inbound | Payload::Reply | Payload::Fact => {
                        panic!("seed {}: a relayed call's payload is its body", self.settings.seed)
                    }
                }
                self.stats.calls += 1;
                let token = reply_to.into_token();
                self.relays.open(token.raw(), ());
                let epoch = self.epoch;
                self.after(self.settings.serve, Delivery::Reply { epoch, reply_to: ReplyTo::new(token) });
            }
            Request::Bounced { .. } => self.end("bounced"),
            Request::Undelivered { run: _, attempt: _, event, undelivered } => {
                assert_eq!(self.payloads.end(event.raw()), Payload::Inbound, "an undelivered event comes back");
                match undelivered {
                    Undelivered::Unplaced | Undelivered::Adrift | Undelivered::Gone => self.end("undelivered"),
                }
            }
            Request::Told { run: _, attempt: _, fact } => {
                assert_eq!(self.payloads.end(fact.raw()), Payload::Fact, "a fact goes up as it is");
                self.stats.told += 1;
            }
            Request::Drop { payload } => {
                self.payloads.end(payload.raw());
            }
        }
    }

    fn sent(&mut self, run: Token, attempt: Token, down: Down) {
        self.observe(Seen::Sent { run: run.raw(), attempt: attempt.raw(), down });
    }

    fn fact(&mut self, fact: Fact) {
        match fact {
            Fact::Found => self.end("found"),
            Fact::Stray => self.end("stray"),
            Fact::Fenced => self.end("fenced"),
            Fact::Duplicate => self.end("duplicate"),
            Fact::Dropped => self.end("dropped"),
            Fact::Busy => self.end("busy"),
            Fact::Forgotten => self.end("forgotten"),
            Fact::TurnedAway => self.end("turned away"),
            Fact::Hello { .. }
            | Fact::Lost { .. }
            | Fact::Refused { .. }
            | Fact::Placed
            | Fact::Answered { .. }
            | Fact::PresumedLost => {}
        }
    }

    // The parent.

    fn item_of(&self, run: u64) -> usize {
        self.items.iter().position(|item| item.run == run).expect("the fleet names the parent's runs")
    }

    /// The parent starts an item's next attempt, while it has starts left.
    /// An attempt of the item still open is replaced by it.
    fn start(&mut self, item: usize) {
        let entry = &self.items[item];
        if entry.done || entry.claim.is_some() {
            return;
        }
        if !self.loaded {
            // Claims first: nothing starts before the cold read is done.
            self.after(self.settings.backoff, Delivery::Start { item });
            return;
        }
        if entry.starts == 0 {
            self.items[item].done = true;
            return;
        }
        let run = entry.run;
        let attempt = self.wire.name();
        let entry = &mut self.items[item];
        entry.starts -= 1;
        entry.open.insert(attempt, Open { cancelled: false, answered: None, stale: false });
        entry.claim = Some(attempt);
        self.stats.starts += 1;
        self.calls.open(attempt, ());
        self.observe(Seen::Started { run, attempt });
        self.stage.push(Event::Start {
            reply_to: ReplyTo::new(Token::new(attempt)),
            run: Token::new(run),
            attempt: Token::new(attempt),
            workstream: run,
            kinds: fleet::Kinds::Workers,
        });
        let at = self.now.saturating_add(self.settings.timeout);
        self.send(at, Delivery::Timeout { item, attempt });
        if self.rng.chance(self.settings.cancels) {
            self.after(self.settings.act_after, Delivery::Act { item, attempt, act: Act::Cancel });
        } else if self.rng.chance(self.settings.replaces) {
            self.after(self.settings.act_after, Delivery::Act { item, attempt, act: Act::Replace });
        }
    }

    /// The parent cancels `attempt`, if its call is open and it has not
    /// cancelled it yet.
    fn cancel(&mut self, item: usize, attempt: u64) {
        let run = self.items[item].run;
        let Some(open) = self.items[item].open.get_mut(&attempt) else {
            return;
        };
        if open.cancelled || open.answered.is_some() {
            return;
        }
        open.cancelled = true;
        if open.stale {
            // Not adopted yet: the adoption cancels it.
            return;
        }
        self.observe(Seen::Cancelled { run, attempt });
        self.stage.push(Event::Cancel { run: Token::new(run), attempt: Token::new(attempt) });
    }

    fn give_up(&mut self, item: usize, attempt: u64) {
        self.cancel(item, attempt);
    }

    fn act(&mut self, item: usize, attempt: u64, act: Act) {
        match act {
            Act::Cancel => self.cancel(item, attempt),
            Act::Replace => {
                if self.is_live(item, attempt) && self.items[item].starts > 0 {
                    // The claim moves to a newer attempt, which replaces this
                    // one unless it is refused.
                    self.items[item].claim = None;
                    self.start(item);
                }
            }
        }
    }

    /// Whether `attempt` is the item's claim, not cancelled, and known to the
    /// fleet.
    fn is_live(&self, item: usize, attempt: u64) -> bool {
        let entry = &self.items[item];
        let Some(open) = entry.open.get(&attempt) else {
            return false;
        };
        entry.claim == Some(attempt) && !open.cancelled && !open.stale && open.answered.is_none()
    }

    /// The fleet placed the parent's claim: it sends inbound events to it.
    fn placed(&mut self, run: u64, attempt: u64) {
        let item = self.item_of(run);
        for _ in 0..self.settings.inbound {
            self.after(self.settings.inbound_after, Delivery::Send { item, attempt });
        }
    }

    fn send_inbound(&mut self, item: usize, attempt: u64) {
        if !self.is_live(item, attempt) {
            return;
        }
        let run = self.items[item].run;
        self.stats.inbound += 1;
        let event = self.payload(Payload::Inbound);
        self.stage.push(Event::Inbound { run: Token::new(run), attempt: Token::new(attempt), event });
    }

    /// An attempt's call ended. An answer is made durable before the record
    /// moves on, and acknowledged then; any other end moves it on at once.
    fn ended(&mut self, to: ReplyTo, run: Token, attempt: Token, end: End) {
        let (run, attempt) = (run.raw(), attempt.raw());
        assert_eq!(to.into_token().raw(), attempt, "a start's call is named by its attempt");
        self.calls.end(attempt);
        self.observe(Seen::Ended { run, attempt, end });
        let ending = match end {
            End::Answered(Said { kind: Kind::Ended, .. }) => "ended",
            End::Answered(Said { kind: Kind::Parked, .. }) => "parked",
            End::Answered(Said { kind: Kind::Failed, .. }) => "failed",
            End::Answered(Said { kind: Kind::Busy, .. }) => "busy answered",
            End::Answered(Said { kind: Kind::Invalid, .. }) => "invalid",
            End::Lost => "lost",
            End::Cancelled => "cancelled",
            End::Replaced => "replaced",
            End::Refused => "refused",
        };
        self.end(ending);
        if end == End::Lost && self.admitted.contains(&(run, attempt)) {
            self.stats.lost_hosted += 1;
        }
        let item = self.item_of(run);
        match end {
            End::Answered(said) => {
                let open = self.items[item].open.get_mut(&attempt).expect("an answer ends an attempt the record holds");
                let again = open.answered.is_some();
                open.answered = Some(said.kind);
                if again {
                    // Heard again after a restart, not yet durable then.
                    self.end("heard again");
                }
                let epoch = self.epoch;
                self.after(self.settings.durable, Delivery::Durable { epoch, item, attempt });
            }
            End::Lost | End::Cancelled | End::Replaced | End::Refused => self.release(item, attempt, false),
        }
    }

    /// The answer of `attempt` is durable: the record moves on, and the
    /// fleet hears that its worker may forget it.
    fn durable(&mut self, item: usize, attempt: u64) {
        let run = self.items[item].run;
        let Some(Open { answered: Some(kind), .. }) = self.items[item].open.get(&attempt).copied() else {
            return;
        };
        self.observe(Seen::Durable { run, attempt });
        self.stage.push(Event::Acknowledge { run: Token::new(run), attempt: Token::new(attempt) });
        let done = match kind {
            Kind::Ended => true,
            Kind::Parked | Kind::Failed | Kind::Busy | Kind::Invalid => false,
        };
        self.release(item, attempt, done);
    }

    /// The record lets `attempt` go, and if it was the claim, the item is
    /// `done`, or starts again after a backoff.
    fn release(&mut self, item: usize, attempt: u64, done: bool) {
        let entry = &mut self.items[item];
        entry.open.remove(&attempt);
        if entry.claim != Some(attempt) {
            return;
        }
        entry.claim = None;
        if done {
            entry.done = true;
        } else {
            self.after(self.settings.backoff, Delivery::Start { item });
        }
    }

    /// After a restart, the parent adopts every attempt its records hold
    /// open, the oldest first, and cancels again those it had cancelled.
    fn adopt(&mut self) {
        for item in 0..self.items.len() {
            let run = Token::new(self.items[item].run);
            let stale: Vec<(u64, bool)> = self.items[item]
                .open
                .iter_mut()
                .filter(|(_, open)| open.stale)
                .map(|(&attempt, open)| {
                    open.stale = false;
                    (attempt, open.cancelled)
                })
                .collect();
            for (attempt, cancelled) in stale {
                self.stats.adoptions += 1;
                self.end("adopted");
                self.observe(Seen::Adopted { run: run.raw(), attempt });
                self.adopting.push(attempt);
                self.calls.open(attempt, ());
                let token = Token::new(attempt);
                self.stage.push(Event::Adopt {
                    reply_to: ReplyTo::new(token),
                    run,
                    attempt: token,
                    kept: 0,
                    kind: fleet::HostKind::Worker,
                    worked: false,
                });
                if cancelled {
                    self.observe(Seen::Cancelled { run: run.raw(), attempt });
                    self.stage.push(Event::Cancel { run, attempt: token });
                }
                let at = self.now.saturating_add(self.settings.timeout);
                self.send(at, Delivery::Timeout { item, attempt });
            }
        }
        // The cold read is done: strays' graces run from now, and the parent
        // starts attempts again.
        self.stage.push(Event::Loaded);
        self.loaded = true;
    }
}

/// An event, for the trace, without its payload's bytes.
fn describe(event: &Event) -> String {
    match event {
        Event::Start { run, attempt, .. } => format!("Start {} {}", run.raw(), attempt.raw()),
        Event::Hello { channel, hello } => {
            format!("Hello {} slots {} hosting {:?}", channel.raw(), hello.slots, hello.hosting)
        }
        Event::StartTyped { .. }
        | Event::InboundTyped { .. }
        | Event::RelayTyped { .. }
        | Event::Grant { .. }
        | Event::Rejected { .. }
        | Event::Exhausted { .. }
        | Event::Turn { .. }
        | Event::TurnKept { .. }
        | Event::TurnBusy { .. }
        | Event::Adopt { .. }
        | Event::Cancel { .. }
        | Event::Inbound { .. }
        | Event::Relayed { .. }
        | Event::Lost { .. }
        | Event::Answer { .. }
        | Event::Relay { .. }
        | Event::Bounced { .. }
        | Event::Told { .. }
        | Event::Acknowledge { .. }
        | Event::Loaded => format!("{event:?}"),
    }
}
