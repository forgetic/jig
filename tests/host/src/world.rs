use std::collections::{BTreeMap, BTreeSet};

use jig_host::{self as host, AgentFailure, Event, Fact, Failure, Limits, Reason, Reply, Request, RunFailure};
use skein_lib::{Duration, ReplyTo, Rng, Time, Token};
use skein_world::domain::{Ledger, Schedule, Span, Stage, Trace};

use crate::engine::{self, Act, Engine, Plan};
use crate::parent::{self, Out, Parent};

/// Room in the host's output queue beyond what one step may emit. Small, so
/// the loop's flow control (take an event only while there is room for what
/// it may produce) is exercised.
const SPARE: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// Seeds the world, which seeds the engine and the parent.
    pub seed: u64,
    pub host: Limits,
    pub engine: engine::Script,
    pub parent: parent::Script,
    /// One-way latency between the engine and the worker.
    pub network: Span,
    /// One-way latency between the host and its siblings, through the top
    /// level.
    pub hop: Span,
    /// A spell without contact with the engine, if there is one.
    pub outage: Option<Outage>,
    /// When the worker shuts down, if it does.
    pub shutdown: Option<Span>,
    /// The chance, per mille, at the end of an iteration, that the engine's
    /// assignment of a run hosted is sent again: the host drops it.
    pub duplicates: u32,
}

/// A spell without contact with the engine: when it starts, how long it
/// lasts, and the grace past which the top level cancels every run. Messages
/// either way wait for contact to come back; on coming back, the top level
/// reports what the host hosts, and the engine keeps or cancels each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outage {
    pub at: Span,
    pub length: Span,
    pub grace: Duration,
}

impl Settings {
    /// A world where nothing goes wrong: room for every assignment, all
    /// within the limits, workspaces that prepare, agents that start, work a
    /// little and end (or park, idle past their time after a yield), and no
    /// cancels.
    #[must_use]
    pub const fn calm(seed: u64) -> Settings {
        Settings {
            seed,
            host: Limits {
                accounts: 4,
                slots: 4,
                charter_bytes: 4096,
                transcript_bytes: 1040,
                delivery_evidence_bytes: 0,
                turn_bytes: 1024,
                outcome_bytes: 512,
                detail_bytes: 64,
                held: 2,
                event_bytes: 64,
                run_calls: 2,
                facts: 64,
                told: 8,
                fact_bytes: 64,
                turns: 0,
                turn_queue_bytes: 0,
            },
            engine: engine::Script {
                assignments: 8,
                spacing: Span::millis(1_000, 20_000),
                invalid: 0,
                repeats: 0,
                saves: 500,
                transcripts: 200,
                events: 3,
                event_gap: Span::millis(100, 10_000),
                large: 0,
                cancels: 0,
                cancel_after: Span::millis(0, 30_000),
                stale: 0,
                relay: Span::millis(10, 2_000),
                forget: 0,
            },
            parent: parent::Script {
                prepare: Span::millis(100, 5_000),
                transient: 0,
                permanent: 0,
                start: Span::millis(50, 500),
                unstarted: 0,
                steps: 6,
                step: Span::millis(100, 5_000),
                relays: 300,
                deliveries: 200,
                yields: 200,
                idle: Span::millis(5_000, 60_000),
                fates: parent::Fates {
                    ended: 1,
                    parked: 0,
                    failed: 0,
                    exited: 0,
                    hung: 0,
                    overrun: 0,
                    rules: 0,
                    oversized: 0,
                },
                exit: Span::millis(10, 500),
                kill: Span::millis(100, 2_000),
                wind: Span::millis(100, 3_000),
                watchdog: Span::millis(10_000, 60_000),
                late: 0,
                words: 0,
                delivery: Span::millis(100, 3_000),
                changes: 600,
                moved: 0,
                failed: 0,
            },
            network: Span::millis(1, 50),
            hop: Span::millis(0, 2),
            outage: None,
            shutdown: None,
            duplicates: 0,
        }
    }

    /// A world where everything that can go wrong does, now and then: a
    /// world for the random sweep.
    #[must_use]
    pub const fn rough(seed: u64) -> Settings {
        let calm = Settings::calm(seed);
        Settings {
            host: Limits { accounts: 4, slots: 2, held: 1, run_calls: 1, facts: 8, ..calm.host },
            engine: engine::Script {
                assignments: 12,
                spacing: Span::millis(0, 10_000),
                invalid: 100,
                repeats: 100,
                events: 4,
                large: 50,
                cancels: 250,
                stale: 300,
                forget: 300,
                ..calm.engine
            },
            parent: parent::Script {
                transient: 60,
                permanent: 60,
                unstarted: 60,
                relays: 350,
                deliveries: 250,
                fates: parent::Fates {
                    ended: 4,
                    parked: 3,
                    failed: 6,
                    exited: 1,
                    hung: 1,
                    overrun: 1,
                    rules: 1,
                    oversized: 1,
                },
                late: 300,
                words: 400,
                moved: 150,
                failed: 150,
                ..calm.parent
            },
            outage: Some(Outage {
                at: Span::millis(0, 120_000),
                length: Span::millis(1_000, 90_000),
                grace: Duration::from_secs(30),
            }),
            shutdown: None,
            duplicates: 2,
            ..calm
        }
    }
}

/// What the world counted.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    pub engine: engine::Tally,
    pub parent: parent::Tally,
    /// Answers, by kind.
    pub endings: BTreeMap<&'static str, u32>,
    /// Facts the host told, by kind, and how many it dropped.
    pub facts: BTreeMap<&'static str, u32>,
    pub facts_lost: u64,
    /// Stale messages the host took, each changing nothing; reports it made;
    /// replies it gave as unavailable; cancels of every run.
    pub stale: u32,
    pub reports: u32,
    pub unavailable: u32,
    pub busy: u32,
    pub cancel_alls: u32,
    /// The most runs hosted at once.
    pub peak: u32,
    /// The less common paths runs took, by name, and how many times.
    pub paths: BTreeMap<&'static str, u32>,
}

/// Something on its way, delivered at its time.
#[derive(Debug)]
enum Delivery {
    /// An event reaching the host: `stale` when it names an attempt never
    /// assigned, so it must change nothing.
    Host { event: Event, stale: bool },
    /// A request of the host's reaching the engine.
    Engine(Request),
    /// The engine's own plan.
    Plan(Plan),
    /// An agent's wake.
    Wake { agent: Token, wake: u64 },
    /// Contact with the engine comes back.
    Back,
    /// The worker shuts down.
    Shutdown,
}

/// An event on its way into the host, with what the world knows of it:
/// `stale` when it names an attempt never assigned, `duplicate` when it
/// assigns the attempt hosted again. Either must change nothing.
#[derive(Debug)]
struct Arrival {
    event: Event,
    stale: bool,
    duplicate: bool,
}

/// The three one-way channels whose order matters, each delivering in the
/// order it was given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    EngineToHost,
    HostToEngine,
    ParentToHost,
}

/// What one step of the host took, for the world to note as it routes what
/// the step made: in the order the steps were taken, so that each request is
/// judged against what came before it.
#[derive(Clone, Copy, Debug)]
enum Taken {
    Assign { run: Token, attempt: Token },
    Stale,
    Duplicate,
    Prepared { owner: Token, prepared: bool },
    Finished { owner: Token, word: Word },
    Shutdown,
    Started { owner: Token, agent: Token },
    Called { owner: Token, call: Token, delivery: bool },
    Gone { owner: Token },
    Delivered { call: Token },
    Saved { owner: Token },
    Other,
}

/// A host call in flight, as the world keeps it.
#[derive(Debug)]
struct Open {
    agent: Token,
    call: Token,
    delivery: bool,
}

/// A hosted run as the world sees it.
#[derive(Debug)]
struct Hosted {
    agent: Option<Token>,
    /// The place of the last inbound event delivered to it.
    delivered: Option<u64>,
    /// Whether its prepare was taken, and how it went; how its agent's start
    /// went.
    prepared: Option<bool>,
    launch: Launch,
    /// How its run first said it finishes, and whether it had been stopped
    /// by then.
    word: Option<Word>,
    stopped_first: bool,
}

/// How a run's agent's start went, as far as the world has seen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Launch {
    Unasked,
    Asked,
    /// It could not be started.
    Unstarted,
    /// It was stopped in the step that took its start.
    Stopped,
}

/// How a run said it finishes, as the host must judge it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Word {
    Ended,
    Parked,
    /// It said more than the limits allow.
    Rules,
    Failed(RunFailure),
}

pub struct World {
    now: Time,
    /// Draws the latencies.
    rng: Rng,
    settings: Settings,
    host: host::Domain,
    stage: Stage<Limits, Arrival, Request>,
    engine: Engine,
    parent: Parent,
    wire: Schedule<Delivery>,
    lanes: [Time; 3],
    /// When contact is lost and comes back, if it is.
    outage: Option<(Time, Time)>,

    /// Hosted runs, by the host's tokens for them; those admitted and not yet
    /// answered, by the engine's names; agents' runs; agents whose runs left
    /// live; and host calls in flight, by agent and call.
    hosted: BTreeMap<Token, Hosted>,
    admitted: BTreeMap<(Token, Token), Token>,
    agents: BTreeMap<Token, Token>,
    left: BTreeSet<Token>,
    calls: Ledger<(Token, Token), Open>,
    /// Engine deliveries still awaited by the host, cancelled at this boundary.
    relays: BTreeMap<Token, (Token, Token)>,
    /// Pushes in flight as their runs left live: answered with how they went.
    kept: BTreeSet<(Token, Token)>,
    /// Whether the host has taken a shutdown.
    shut: bool,
    stats: Stats,
    trace: Trace,
}

impl World {
    #[must_use]
    pub fn new(settings: Settings) -> World {
        assert!(host::worst_case(&settings.host).is_some(), "the shell refuses limits it cannot provision");
        let mut rng = Rng::new(settings.seed);
        let engine = Engine::new(settings.engine, settings.host, rng.next_u64());
        let parent = Parent::new(settings.parent, settings.host, rng.next_u64());
        let max_out = host::max_out(&settings.host);
        let latencies = Rng::new(rng.next_u64());
        let mut world = World {
            now: Time::ZERO,
            rng: latencies,
            settings,
            host: host::Domain::new(&settings.host),
            stage: Stage::new(settings.host, max_out, max_out + SPARE),
            engine,
            parent,
            wire: Schedule::new(),
            lanes: [Time::ZERO; 3],
            outage: None,
            hosted: BTreeMap::new(),
            admitted: BTreeMap::new(),
            agents: BTreeMap::new(),
            left: BTreeSet::new(),
            calls: Ledger::new("host call"),
            relays: BTreeMap::new(),
            kept: BTreeSet::new(),
            shut: false,
            stats: Stats::default(),
            trace: Trace::default(),
        };
        if let Some(outage) = settings.outage {
            let lost = Time::ZERO.saturating_add(outage.at.draw(&mut rng));
            let length = outage.length.draw(&mut rng);
            let back = lost.saturating_add(length);
            world.outage = Some((lost, back));
            if length > outage.grace {
                let event = Event::CancelAll { reason: Reason::Contact };
                world.wire.send(lost.saturating_add(outage.grace), Delivery::Host { event, stale: false });
            }
            world.wire.send(back, Delivery::Back);
        }
        if let Some(at) = settings.shutdown {
            let at = Time::ZERO.saturating_add(at.draw(&mut rng));
            world.wire.send(at, Delivery::Shutdown);
        }
        let begin = world.engine.begin();
        world.acts(begin);
        world
    }

    #[must_use]
    pub fn now(&self) -> Time {
        self.now
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            engine: self.engine.tally(),
            parent: self.parent.tally(),
            endings: self.engine.endings().clone(),
            facts_lost: self.host.facts_lost(),
            ..self.stats.clone()
        }
    }

    /// What crossed between the host and the world, in order, with times.
    #[must_use]
    pub fn trace(&self) -> &[String] {
        self.trace.lines()
    }

    /// Runs until nothing is left to happen, then checks the invariants of a
    /// settled world. Panics if it takes more than `iterations`.
    pub fn run(&mut self, iterations: u32) {
        for _ in 0..iterations {
            self.iterate();
            if self.has_work_now() {
                continue;
            }
            let Some(next) = self.wire.next_time() else {
                self.assert_settled();
                return;
            };
            assert!(next > self.now, "time moves forward");
            self.now = next;
        }
        panic!("the world did not settle in {iterations} iterations");
    }

    /// One iteration of the loop, as the shell would run it.
    fn iterate(&mut self) {
        self.stage.tick(self.now);
        self.deliver();
        let mut made = Vec::new();
        // The ready list first, at the start of the stage, then the events.
        while self.stage.has_room() && self.host.is_ready() {
            self.trace.log(self.now, "host resumes");
            let before = self.stage.out.len();
            host::resume(&mut self.host, &self.stage.env, &mut self.stage.out);
            made.push((Taken::Other, self.stage.out.len() - before));
        }
        while let Some(Arrival { event, stale, duplicate }) = self.stage.next_event() {
            self.trace.log(self.now, format!("host <- {event:?}"));
            let taken = if duplicate { Taken::Duplicate } else { self.take(&event, stale) };
            let before = self.stage.out.len();
            host::step(&mut self.host, &self.stage.env, event, &mut self.stage.out);
            made.push((taken, self.stage.out.len() - before));
        }
        for (taken, count) in made {
            match taken {
                Taken::Stale => {
                    assert_eq!(count, 0, "a stale attempt never acts");
                    self.stats.stale += 1;
                }
                Taken::Duplicate => {
                    assert_eq!(count, 1, "a fresh duplicate assignment call is answered once");
                    self.duplicate_answered();
                    self.path("duplicate assignments");
                }
                Taken::Assign { run, attempt } => {
                    assert_eq!(count, 1, "an assignment is refused or prepared");
                    let request = self.stage.out.pop().expect("counted");
                    self.assigned(run, attempt, &request);
                    self.route(request);
                }
                Taken::Started { .. }
                | Taken::Called { .. }
                | Taken::Gone { .. }
                | Taken::Delivered { .. }
                | Taken::Saved { .. }
                | Taken::Prepared { .. }
                | Taken::Finished { .. }
                | Taken::Shutdown
                | Taken::Other => {
                    let leaving = self.note(taken);
                    let mut requests = Vec::new();
                    for _ in 0..count {
                        requests.push(self.stage.out.pop().expect("counted"));
                    }
                    // A run whose agent this step stops leaves live in it: what
                    // the step answers it is judged as such.
                    let mut left = Vec::from_iter(leaving);
                    for request in &requests {
                        if let Request::Stop { agent } = request {
                            self.leave(*agent);
                            left.push(*agent);
                            if let Taken::Started { owner, agent: started } = taken
                                && started == *agent
                            {
                                self.hosted.get_mut(&owner).expect("a start is of a hosted run").launch =
                                    Launch::Stopped;
                            }
                        }
                    }
                    for request in requests {
                        self.route(request);
                    }
                    for agent in left {
                        for open in self.calls.values() {
                            let kept = self.kept.contains(&(open.agent, open.call));
                            assert!(
                                open.agent != agent || kept,
                                "a run's relayed calls are answered as it leaves live"
                            );
                        }
                    }
                }
            }
        }
        assert!(self.stage.out.is_empty(), "every request was routed");
        while let Some(fact) = self.host.pop_fact() {
            *self.stats.facts.entry(fact_kind(fact)).or_default() += 1;
        }
        self.host.reclaim();
        let hosted = u32::try_from(self.admitted.len()).expect("fits");
        assert_eq!(self.host.hosted(), hosted, "a slot is taken from admission until its run has answered");
        self.stats.peak = self.stats.peak.max(hosted);
        // The engine sends the assignment of a run hosted again: it is the
        // next thing the host takes, so the run is still hosted then.
        let hosted: Vec<(Token, Token)> = self.admitted.keys().copied().collect();
        for (run, attempt) in hosted {
            if self.rng.chance(self.settings.duplicates) {
                let assignment = host::Assignment {
                    grants: Box::new([]),
                    run,
                    attempt,
                    workspace: Some(host::Workspace { workstream: run.raw(), items: Token::new(0) }),
                    save: false,
                    charter: Box::from(&b"again"[..]),
                };
                let event = crate::fixtures::assign(ReplyTo::new(run), assignment);
                self.stage.inbox.push_front(Arrival { event, stale: false, duplicate: true });
            }
        }
    }

    fn path(&mut self, path: &'static str) {
        *self.stats.paths.entry(path).or_default() += 1;
    }

    fn duplicate_answered(&mut self) {
        let request = self.stage.out.pop().expect("counted");
        assert!(
            matches!(
                request,
                Request::AnswerV2 {
                    answer: host::AnswerV2 { ending: host::EndingV2::Refused(host::Refusal::Busy), .. },
                    ..
                }
            ),
            "a duplicate call is refused without changing its hosted attempt: {request:?}"
        );
    }

    /// What the host takes as `event`, for the world to note.
    fn take(&mut self, event: &Event, stale: bool) -> Taken {
        if stale {
            return Taken::Stale;
        }
        match event {
            Event::AssignTyped { reply_to: _, assignment } => {
                Taken::Assign { run: assignment.assignment.run, attempt: assignment.assignment.attempt }
            }
            Event::Started { owner, agent } => Taken::Started { owner: *owner, agent: *agent },
            Event::CalledTyped { owner, call, ask } => {
                let delivery = match ask {
                    host::Ask::DeliverV2 { .. } => true,
                    host::Ask::RelayTyped { .. } => false,
                };
                Taken::Called { owner: *owner, call: crate::fixtures::callback(call), delivery }
            }
            Event::Gone { owner, detail: _ } => Taken::Gone { owner: *owner },
            Event::Delivered { owner, delivery: _ } => Taken::Delivered { call: *owner },
            Event::Saved { owner, at: _ } => Taken::Saved { owner: *owner },
            Event::Prepared { owner, workspace: _ } => Taken::Prepared { owner: *owner, prepared: true },
            Event::Unprepared { owner, .. } => Taken::Prepared { owner: *owner, prepared: false },
            Event::FinishedV2 { owner, finish, .. } => {
                let limits = &self.settings.host;
                let word = match finish {
                    host::FinishV2::Ended { outcome } if len(outcome) > limits.outcome_bytes => {
                        self.path("oversized outcomes");
                        Word::Rules
                    }
                    host::FinishV2::Ended { .. } => Word::Ended,
                    host::FinishV2::Parked => Word::Parked,
                    host::FinishV2::Failed { failure } => Word::Failed(*failure),
                };
                Taken::Finished { owner: *owner, word }
            }
            Event::CancelAll { reason } => {
                self.stats.cancel_alls += 1;
                match reason {
                    Reason::Shutdown => Taken::Shutdown,
                    Reason::Engine | Reason::Contact => Taken::Other,
                }
            }
            Event::Report => {
                self.stats.reports += 1;
                Taken::Other
            }
            Event::InboundTyped { .. }
            | Event::Facts { .. }
            | Event::Grant { .. }
            | Event::Cancel { .. }
            | Event::Unacknowledged { .. }
            | Event::Relayed { .. }
            | Event::RelayCancelled { .. }
            | Event::WithdrawnTyped { .. }
            | Event::Bounced { .. }
            | Event::Yielded { .. }
            | Event::Turn { .. }
            | Event::AcknowledgeTurn { .. }
            | Event::Faulted { .. } => Taken::Other,
        }
    }

    /// The agent `agent`'s run leaves live: its delivery in flight, if it has
    /// one, is kept through the stop.
    fn leave(&mut self, agent: Token) {
        if !self.left.insert(agent) {
            return;
        }
        for open in self.calls.values() {
            if open.agent == agent && open.delivery {
                self.kept.insert((open.agent, open.call));
            }
        }
    }

    /// Notes what a step took, before routing what it made: the agent whose
    /// run it makes leave live, if it does.
    fn note(&mut self, taken: Taken) -> Option<Token> {
        match taken {
            Taken::Started { owner, agent } => {
                self.hosted.get_mut(&owner).expect("a start is of a hosted run").agent = Some(agent);
                self.agents.insert(agent, owner);
            }
            Taken::Called { owner, call, delivery } => {
                let agent = self.hosted.get(&owner).expect("a call is of a hosted run").agent;
                let agent = agent.expect("a call is made by a started agent");
                self.calls.open((agent, call), Open { agent, call, delivery });
            }
            // Its run leaves live, if it was: what it had in flight is
            // answered now.
            Taken::Gone { owner } => {
                let hosted = self.hosted.get_mut(&owner).expect("a gone agent is of a hosted run");
                let agent = hosted.agent;
                match agent {
                    Some(agent) => self.leave(agent),
                    None => hosted.launch = Launch::Unstarted,
                }
                return agent;
            }
            Taken::Delivered { call } => self.parent.delivered(call),
            Taken::Saved { owner } => self.parent.saved(owner),
            Taken::Prepared { owner, prepared } => {
                self.hosted.get_mut(&owner).expect("a prepare is of a hosted run").prepared = Some(prepared);
                self.parent.prepared(owner);
            }
            Taken::Finished { owner, word } => {
                let hosted = self.hosted.get_mut(&owner).expect("a finish is of a hosted run");
                let agent = hosted.agent.expect("a run that finishes was started");
                // Only its first word counts: the host ignores the rest.
                if hosted.word.is_none() {
                    hosted.word = Some(word);
                    hosted.stopped_first = self.left.contains(&agent);
                    if hosted.stopped_first {
                        self.path("endings said during a stop");
                    }
                }
            }
            Taken::Shutdown => self.shut = true,
            Taken::Assign { .. } | Taken::Stale | Taken::Duplicate | Taken::Other => {}
        }
        None
    }

    /// The host's one request in the step that took the assignment for
    /// `run`'s attempt `attempt`: a refusal, or a prepare that admits it.
    fn assigned(&mut self, run: Token, attempt: Token, request: &Request) {
        match request {
            Request::AnswerV2 { to: _, run: answered, attempt: of, answer } => {
                assert_eq!((*answered, *of), (run, attempt), "an assignment is answered as itself");
                match &answer.ending {
                    host::EndingV2::Refused(_) => {}
                    host::EndingV2::Ended { .. } | host::EndingV2::Parked { .. } | host::EndingV2::Failed { .. } => {
                        panic!("an assignment answered at once is refused: {answer:?}")
                    }
                }
            }
            Request::Prepare { owner, workspace: _ } => {
                let entry = Hosted {
                    agent: None,
                    delivered: None,
                    prepared: None,
                    launch: Launch::Unasked,
                    word: None,
                    stopped_first: false,
                };
                assert!(self.hosted.insert(*owner, entry).is_none(), "a hosted run's token is its own");
                assert!(self.admitted.insert((run, attempt), *owner).is_none(), "an attempt is admitted once");
            }
            Request::RelayTyped { .. }
            | Request::CancelRelay { .. }
            | Request::Bounced { .. }
            | Request::Grant { .. }
            | Request::Hosting { .. }
            | Request::Abort { .. }
            | Request::StartTyped { .. }
            | Request::DeliverTyped { .. }
            | Request::ReplyTyped { .. }
            | Request::Stop { .. }
            | Request::DeliverV2 { .. }
            | Request::Save { .. }
            | Request::Turn { .. }
            | Request::AcknowledgeAgentTurn { .. }
            | Request::Release { .. } => panic!("an assignment is refused or prepared: {request:?}"),
        }
    }

    /// Hands the host's `request` to whom it is for, checking it on the way.
    fn route(&mut self, request: Request) {
        self.trace.log(self.now, format!("host -> {request:?}"));
        match request {
            Request::AnswerV2 { to, run, attempt, answer } => {
                if let Some(owner) = self.admitted.remove(&(run, attempt)) {
                    assert!(self.parent.settled(owner), "a run answers once all of it has settled and is released");
                    let hosted = self.hosted.get(&owner).expect("admitted runs are hosted");
                    if let Some(agent) = hosted.agent {
                        assert!(self.left.contains(&agent), "a run answers once it has left live");
                    }
                    check_word(hosted, &answer.ending);
                    if let Some(path) = cancel_path(hosted, &answer.ending) {
                        self.path(path);
                    }
                }
                self.send_engine(Request::AnswerV2 { to, run, attempt, answer });
            }
            Request::RelayTyped { run, attempt, delivery: call, .. } => {
                assert!(self.relays.insert(call, (run, attempt)).is_none(), "a relay starts once");
                self.send_engine(request);
            }
            Request::Turn { .. } | Request::AcknowledgeAgentTurn { .. } => panic!("turns are exercised by turn_world"),
            Request::CancelRelay { call } => {
                if self.relays.remove(&call).is_some() {
                    let event = Event::RelayCancelled { call };
                    self.stage.push(Arrival { event, stale: false, duplicate: false });
                }
            }
            Request::Bounced { .. } => self.send_engine(request),
            Request::Hosting { runs } => {
                let mut hosting = BTreeSet::new();
                for entry in &runs {
                    hosting.insert((entry.run, entry.attempt));
                }
                let admitted: BTreeSet<(Token, Token)> = self.admitted.keys().copied().collect();
                assert_eq!(hosting, admitted, "the report lists every run admitted and not answered");
                let acts = self.engine.reconnected(&runs);
                self.acts(acts);
            }
            Request::DeliverTyped { agent, ref words, .. } => {
                let owner = *self.agents.get(&agent).expect("a delivery is to a started agent");
                let len = u64::try_from(words.len()).expect("fits");
                assert!(len <= self.settings.host.event_bytes, "an event within the limits");
                let hosted = self.hosted.get_mut(&owner).expect("an agent's run is hosted");
                let head = words.get(..8).expect("an event begins with its place");
                let place = u64::from_be_bytes(head.try_into().expect("eight bytes"));
                assert!(hosted.delivered < Some(place), "inbound events are delivered once, in the order sent");
                hosted.delivered = Some(place);
                self.parcel(request);
            }
            Request::ReplyTyped { agent, ref call, ref reply } => {
                let call = crate::fixtures::callback(call);
                self.calls.end((agent, call));
                if self.kept.remove(&(agent, call)) {
                    self.path("deliveries settled during a stop");
                    match reply {
                        Reply::Delivered(_) => {}
                        Reply::Relayed { .. } | Reply::Unavailable | Reply::Busy | Reply::Withdrawn => {
                            panic!("a delivery kept through the stop says how it went: {reply:?}")
                        }
                    }
                } else if self.left.contains(&agent) {
                    assert_eq!(*reply, Reply::Unavailable, "a call of a run that has left live is unavailable");
                }
                match reply {
                    Reply::Unavailable => self.stats.unavailable += 1,
                    Reply::Busy => self.stats.busy += 1,
                    Reply::Relayed { .. } | Reply::Delivered(_) | Reply::Withdrawn => {}
                }
                self.parcel(request);
            }
            Request::Stop { agent } => {
                assert!(self.left.contains(&agent), "a stop is of a run that leaves live");
                self.parcel(Request::Stop { agent });
            }
            Request::Prepare { .. } => {
                assert!(!self.shut, "a worker shutting down admits no more runs");
                self.parcel(request);
            }
            Request::Abort { owner } => {
                let hosted = self.hosted.get(&owner).expect("an abort is of a hosted run");
                assert!(hosted.prepared.is_none(), "an abort is of a prepare in flight");
                self.parcel(request);
            }
            Request::StartTyped { owner, .. } => {
                self.hosted.get_mut(&owner).expect("a start is of a hosted run").launch = Launch::Asked;
                self.parcel(request);
            }
            Request::DeliverV2 { .. } | Request::Save { .. } | Request::Release { .. } | Request::Grant { .. } => {
                self.parcel(request);
            }
        }
    }

    fn send_engine(&mut self, request: Request) {
        let at = self.lane(Lane::HostToEngine, self.settings.network);
        self.wire.send(at, Delivery::Engine(request));
    }

    /// Hands `request` to the parent, and sends on what it does.
    fn parcel(&mut self, request: Request) {
        let outs = self.parent.take(request);
        self.outs(outs);
    }

    fn outs(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Host { after, event } => {
                    let at = self.lane_after(Lane::ParentToHost, self.settings.hop, after);
                    self.wire.send(at, Delivery::Host { event, stale: false });
                }
                Out::Wake { after, agent, wake } => {
                    self.wire.send(self.now.saturating_add(after), Delivery::Wake { agent, wake });
                }
            }
        }
    }

    fn acts(&mut self, acts: Vec<Act>) {
        for act in acts {
            match act {
                Act::Host { after, event, stale } => {
                    let at = self.lane_after(Lane::EngineToHost, self.settings.network, after);
                    self.wire.send(at, Delivery::Host { event, stale });
                }
                Act::Later { after, plan } => {
                    self.wire.send(self.now.saturating_add(after), Delivery::Plan(plan));
                }
            }
        }
    }

    /// Hands over what is due now.
    fn deliver(&mut self) {
        while let Some(delivery) = self.wire.next(self.now) {
            match delivery {
                Delivery::Host { event, stale } => {
                    if let Event::Relayed { run, attempt, call, .. } = &event {
                        if stale || self.relays.get(call) != Some(&(*run, *attempt)) {
                            continue;
                        }
                        self.relays.remove(call);
                    }
                    self.stage.push(Arrival { event, stale, duplicate: false });
                }
                Delivery::Engine(request) => {
                    if let Request::RelayTyped { delivery: call, .. } = &request
                        && !self.relays.contains_key(call)
                    {
                        continue;
                    }
                    let acts = self.engine.take(request);
                    self.acts(acts);
                }
                Delivery::Plan(plan) => {
                    let acts = self.engine.plan(plan);
                    self.acts(acts);
                }
                Delivery::Wake { agent, wake } => {
                    let outs = self.parent.wake(agent, wake);
                    self.outs(outs);
                }
                Delivery::Back => self.stage.push(Arrival { event: Event::Report, stale: false, duplicate: false }),
                Delivery::Shutdown => {
                    let event = Event::CancelAll { reason: Reason::Shutdown };
                    self.stage.push(Arrival { event, stale: false, duplicate: false });
                }
            }
        }
    }

    /// When something sent now on `lane` arrives, `span` after now and after
    /// what was sent on it before.
    fn lane(&mut self, lane: Lane, span: Span) -> Time {
        self.lane_after(lane, span, Duration::ZERO)
    }

    fn lane_after(&mut self, lane: Lane, span: Span, after: Duration) -> Time {
        let mut at = self.now.saturating_add(after).saturating_add(span.draw(&mut self.rng));
        let (index, engine) = match lane {
            Lane::EngineToHost => (0, true),
            Lane::HostToEngine => (1, true),
            Lane::ParentToHost => (2, false),
        };
        // Messages to and from the engine wait for contact to come back.
        if let Some((lost, back)) = self.outage
            && engine
            && at >= lost
            && at < back
        {
            at = back;
        }
        at = at.max(self.lanes[index]);
        self.lanes[index] = at;
        at
    }

    fn has_work_now(&self) -> bool {
        self.stage.has_events() || self.host.is_ready() || self.wire.is_due(self.now)
    }

    /// Checks the invariants of a world with nothing left to happen.
    fn assert_settled(&self) {
        assert!(self.wire.is_empty(), "nothing is in flight");
        assert!(!self.stage.has_events(), "the host has taken everything");
        assert!(!self.host.is_ready(), "no run waits on the ready list");
        assert_eq!(self.host.hosted(), 0, "every slot is free");
        assert_eq!(self.host.calls(), 0, "no host call is in flight");
        assert!(self.admitted.is_empty(), "every admitted run has answered");
        self.calls.assert_settled();
        assert!(self.relays.is_empty(), "every engine delivery ended or was cancelled");
        self.engine.assert_settled();
        self.parent.assert_settled();
        let facts = &self.stats.facts;
        if self.host.facts_lost() == 0 {
            let admitted = facts.get("admitted").copied().unwrap_or(0);
            let answered: u32 =
                ["ended", "parked", "failed"].iter().map(|kind| facts.get(kind).copied().unwrap_or(0)).sum();
            assert_eq!(admitted, answered, "every admitted run tells how it answered");
        }
    }
}

fn fact_kind(fact: Fact) -> &'static str {
    match fact {
        Fact::Admitted { .. } => "admitted",
        Fact::Prepared { .. } => "prepared",
        Fact::Started { .. } => "started",
        Fact::Parked { .. } => "parked",
        Fact::Ended { .. } => "ended",
        Fact::Failed { .. } => "failed",
    }
}

/// Checks that a run is answered as it first said it finishes: its own ending
/// stands even after a stop, a cancel it reports after a stop being the
/// worker's; a run that said nothing is answered as failed.
fn check_word(hosted: &Hosted, answer: &host::EndingV2) {
    let failure = match answer {
        host::EndingV2::Failed { failure, .. } => Some(*failure),
        host::EndingV2::Refused(_) | host::EndingV2::Ended { .. } | host::EndingV2::Parked { .. } => None,
    };
    match hosted.word {
        Some(Word::Ended) => assert!(matches_ended(answer), "a run that said it ended is answered so: {answer:?}"),
        Some(Word::Parked) => assert!(matches_parked(answer), "a run that said it parked is answered so: {answer:?}"),
        Some(Word::Rules) => {
            assert_eq!(failure, Some(Failure::Agent(AgentFailure::Rules)), "saying too much breaks the rules");
        }
        Some(Word::Failed(RunFailure::Cancelled)) if hosted.stopped_first => match failure {
            Some(Failure::Cancelled(_) | Failure::Agent(_)) => {}
            Some(Failure::Run(_) | Failure::Unprepared(_)) | None => {
                panic!("a cancel a stopped run reports is the worker's: {answer:?}")
            }
        },
        Some(Word::Failed(reported)) => {
            assert_eq!(failure, Some(Failure::Run(reported)), "a run's failure is as it reports it");
        }
        None => assert!(failure.is_some(), "a run that said nothing is answered as failed: {answer:?}"),
    }
}

fn matches_ended(answer: &host::EndingV2) -> bool {
    match answer {
        host::EndingV2::Ended { .. } => true,
        host::EndingV2::Refused(_) | host::EndingV2::Parked { .. } | host::EndingV2::Failed { .. } => false,
    }
}

fn matches_parked(answer: &host::EndingV2) -> bool {
    match answer {
        host::EndingV2::Parked { .. } => true,
        host::EndingV2::Refused(_) | host::EndingV2::Ended { .. } | host::EndingV2::Failed { .. } => false,
    }
}

/// Where a cancelled run was when its cancel came, if before it was live.
fn cancel_path(hosted: &Hosted, answer: &host::EndingV2) -> Option<&'static str> {
    match answer {
        host::EndingV2::Failed { failure: Failure::Cancelled(_), .. } => {}
        host::EndingV2::Refused(_)
        | host::EndingV2::Ended { .. }
        | host::EndingV2::Parked { .. }
        | host::EndingV2::Failed { .. } => {
            return None;
        }
    }
    match hosted.launch {
        Launch::Unasked => match hosted.prepared {
            Some(true) => Some("cancels as a workspace was prepared"),
            Some(false) => Some("cancels as a workspace failed to prepare"),
            None => unreachable!("a run answers once its prepare has ended"),
        },
        Launch::Unstarted => Some("cancels as an agent failed to start"),
        Launch::Stopped => Some("cancels as an agent started"),
        Launch::Asked => None,
    }
}

fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).expect("fits")
}
