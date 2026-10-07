//! An engine-like assigner, scripted: what the engine's fleet does with this
//! worker, as the top level will route it from the engine link, played from a
//! seed. It speaks the host's vocabulary.
//!
//! - It sends assignments, spaced out: within the limits, beyond them (the
//!   kind of refusal it expects noted), or naming a run it has in flight
//!   under a new attempt; also to a worker shutting down, which refuses
//!   them. Workspace items, saving and snapshots are drawn.
//! - For each assignment it sends inbound events while the run is in flight,
//!   each carrying its place in the run's sequence, some too large; may
//!   cancel it; and may send messages for an attempt it never assigned,
//!   which must change nothing.
//! - It answers each relayed call after a while, unless it cancelled the
//!   attempt, and sometimes sends a stale answer first.
//! - On reconnecting it hears what the worker hosts, and keeps or cancels
//!   each run.
//!
//! It checks as it goes that every assignment is answered exactly once, with
//! the refusal it expects for one beyond the limits and none other, and that
//! what an answer says landed or saved names the workspace's items.

use std::collections::{BTreeMap, BTreeSet};

use jig_worker_host::{
    AgentFailure, Answer, Assignment, Bounce, Event, Failure, Hosting, Invalid, Limits, Preparation, Reason, Refusal,
    Request, RunFailure, Workspace,
};
use skein_lib::{Duration, ReplyTo, Rng, Token};
use skein_world::domain::Span;

/// How the engine behaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Script {
    /// Assignments it sends, and the time between them.
    pub assignments: u32,
    pub spacing: Span,
    /// The chance, per mille, that an assignment is beyond the limits, and
    /// that it names a run in flight under a new attempt.
    pub invalid: u32,
    pub repeats: u32,
    /// The chance, per mille, that an assignment saves unfinished work, and
    /// that it has a snapshot.
    pub saves: u32,
    pub snapshots: u32,
    /// The most inbound events it sends a run, the time between them, and the
    /// chance, per mille, that one is too large.
    pub events: u32,
    pub event_gap: Span,
    pub large: u32,
    /// The chance, per mille, that it cancels a run, and when.
    pub cancels: u32,
    pub cancel_after: Span,
    /// The chance, per mille, that it sends a run messages for an attempt it
    /// never assigned, and a relayed call a stale answer first.
    pub stale: u32,
    /// How long it takes to answer a relayed call.
    pub relay: Span,
    /// The chance, per mille, that it cancels a run it hears of on
    /// reconnecting.
    pub forget: u32,
}

/// What the engine does next.
#[derive(Debug)]
pub enum Act {
    /// An event for the host over the network, `after` from now; `stale`
    /// when it names an attempt never assigned, so it must change nothing.
    Host { after: Duration, event: Event, stale: bool },
    /// Something the engine does itself, `after` from now.
    Later { after: Duration, plan: Plan },
}

/// What the engine does at its time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Plan {
    Assign,
    Inbound { run: Token, attempt: Token },
    Cancel { run: Token, attempt: Token },
    Stale { run: Token },
}

/// What the engine counted.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tally {
    pub assignments: u32,
    pub events: u32,
    /// Inbound events bounced: too large, with the hold full, to a run
    /// ending.
    pub too_large: u32,
    pub full: u32,
    pub ending: u32,
    pub cancels: u32,
    pub stale: u32,
    pub relays: u32,
    /// Relayed calls it did not answer, having cancelled their attempt.
    pub unanswered: u32,
    pub forgotten: u32,
}

/// An assignment in flight, as the engine keeps it.
#[derive(Debug)]
struct Assigned {
    /// The refusal it expects, for an assignment beyond the limits.
    invalid: Option<Invalid>,
    /// Its next inbound event's place in the sequence.
    next: u64,
    cancelled: bool,
}

#[derive(Debug)]
pub struct Engine {
    script: Script,
    limits: Limits,
    rng: Rng,
    names: u64,
    sent: u32,
    /// Assignments in flight: sent and not answered.
    open: BTreeMap<(Token, Token), Assigned>,
    /// Endings, by kind.
    endings: BTreeMap<&'static str, u32>,
    tally: Tally,
}

impl Engine {
    #[must_use]
    pub fn new(script: Script, limits: Limits, seed: u64) -> Engine {
        Engine {
            script,
            limits,
            rng: Rng::new(seed),
            names: 0,
            sent: 0,
            open: BTreeMap::new(),
            endings: BTreeMap::new(),
            tally: Tally::default(),
        }
    }

    #[must_use]
    pub fn tally(&self) -> Tally {
        self.tally
    }

    #[must_use]
    pub fn endings(&self) -> &BTreeMap<&'static str, u32> {
        &self.endings
    }

    /// The assignments in flight: sent and not answered.
    #[must_use]
    pub fn in_flight(&self) -> BTreeSet<(Token, Token)> {
        self.open.keys().copied().collect()
    }

    pub fn assert_settled(&self) {
        let open: Vec<_> = self.open.keys().collect();
        assert!(open.is_empty(), "every assignment has been answered: {open:?} have not");
    }

    /// What the engine does first.
    #[must_use]
    pub fn begin(&self) -> Vec<Act> {
        if self.script.assignments == 0 {
            return Vec::new();
        }
        vec![Act::Later { after: Duration::ZERO, plan: Plan::Assign }]
    }

    /// Does `plan`, now that its time has come.
    pub fn plan(&mut self, plan: Plan) -> Vec<Act> {
        match plan {
            Plan::Assign => self.assign(),
            Plan::Inbound { run, attempt } => self.inbound(run, attempt),
            Plan::Cancel { run, attempt } => self.cancel(run, attempt),
            Plan::Stale { run } => {
                self.tally.stale += 1;
                let attempt = self.name();
                let event = Box::from(&b"stale"[..]);
                vec![
                    Self::host(Event::Inbound { name: Token::new(1), run, attempt, event }, true),
                    Self::host(Event::Cancel { run, attempt }, true),
                ]
            }
        }
    }

    /// Takes the host's request `request`, which is for the engine.
    pub fn take(&mut self, request: Request) -> Vec<Act> {
        match request {
            jig_worker_host::Request::StartTyped { .. }
            | jig_worker_host::Request::Turn { .. }
            | jig_worker_host::Request::DeliverV2 { .. }
            | jig_worker_host::Request::RelayV2 { .. }
            | jig_worker_host::Request::AnswerV2 { .. }
            | jig_worker_host::Request::StartV2 { .. }
            | Request::TurnCredit { .. } => unreachable!("this script runs version one"),

            Request::Answer { to, run, attempt, answer } => {
                assert_eq!(to, ReplyTo::new(run), "an answer goes to its assignment");
                self.answered(run, attempt, &answer);
                Vec::new()
            }
            Request::Relay { run, attempt, call, body: _ } => self.relay(run, attempt, call),
            Request::Bounced { name: _, run: _, attempt: _, bounce } => {
                match bounce {
                    Bounce::TooLarge => self.tally.too_large += 1,
                    Bounce::Full => self.tally.full += 1,
                    Bounce::Ending => self.tally.ending += 1,
                }
                Vec::new()
            }
            Request::Hosting { .. }
            | Request::CancelRelay { .. }
            | Request::Prepare { .. }
            | Request::Abort { .. }
            | Request::Start { .. }
            | Request::Deliver { .. }
            | Request::Reply { .. }
            | Request::Stop { .. }
            | Request::DeliverWorkspace { .. }
            | Request::Save { .. }
            | Request::Release { .. }
            | Request::Grant { .. } => unreachable!("for the top level or the parent"),
        }
    }

    /// Hears what the worker hosts on reconnecting, and keeps or cancels each.
    pub fn reconnected(&mut self, runs: &[Hosting]) -> Vec<Act> {
        let mut acts = Vec::new();
        for hosting in runs {
            if self.rng.chance(self.script.forget) {
                self.tally.forgotten += 1;
                acts.extend(self.cancel(hosting.run, hosting.attempt));
            }
        }
        acts
    }

    fn assign(&mut self) -> Vec<Act> {
        if self.sent == self.script.assignments {
            return Vec::new();
        }
        self.sent += 1;
        self.tally.assignments += 1;
        let mut acts = Vec::new();
        if self.sent < self.script.assignments {
            let after = self.script.spacing.draw(&mut self.rng);
            acts.push(Act::Later { after, plan: Plan::Assign });
        }
        let in_flight: Vec<Token> = self.open.keys().map(|(run, _)| *run).collect();
        let run = if !in_flight.is_empty() && self.rng.chance(self.script.repeats) {
            let count = u64::try_from(in_flight.len()).expect("fits");
            in_flight[usize::try_from(self.rng.below(count)).expect("fits")]
        } else {
            self.name()
        };
        let attempt = self.name();
        let (assignment, invalid) = self.draw(run, attempt);
        let assigned = Assigned { invalid, next: 0, cancelled: false };
        assert!(self.open.insert((run, attempt), assigned).is_none(), "an attempt is assigned once");
        acts.push(Self::host(Event::Assign { reply_to: ReplyTo::new(run), assignment }, false));
        // What it sends the run while it is in flight.
        let events = self.rng.below(u64::from(self.script.events) + 1);
        let mut at = Duration::ZERO;
        for _ in 0..events {
            at = at.saturating_add(self.script.event_gap.draw(&mut self.rng));
            acts.push(Act::Later { after: at, plan: Plan::Inbound { run, attempt } });
        }
        if self.rng.chance(self.script.cancels) {
            let after = self.script.cancel_after.draw(&mut self.rng);
            acts.push(Act::Later { after, plan: Plan::Cancel { run, attempt } });
        }
        if self.rng.chance(self.script.stale) {
            let after = self.script.cancel_after.draw(&mut self.rng);
            acts.push(Act::Later { after, plan: Plan::Stale { run } });
        }
        acts
    }

    /// An assignment for `run`'s attempt `attempt`, and the refusal it
    /// expects if it is beyond the limits.
    fn draw(&mut self, run: Token, attempt: Token) -> (Assignment, Option<Invalid>) {
        let charter = bytes(self.rng.between(1, self.limits.charter_bytes));
        let snapshot = if self.rng.chance(self.script.snapshots) {
            Some(bytes(self.rng.between(1, self.limits.snapshot_bytes)))
        } else {
            None
        };
        let mut assignment = Assignment {
            grants: Box::new([]),
            run,
            attempt,
            workspace: Some(Workspace { workstream: run.raw(), items: Token::new(run.raw().saturating_add(1000)) }),
            save: self.rng.chance(self.script.saves),
            charter,
            snapshot,
        };
        if !self.rng.chance(self.script.invalid) {
            return (assignment, None);
        }
        let invalid = if self.rng.chance(500) {
            assignment.charter = bytes(self.limits.charter_bytes.saturating_add(1));
            Invalid::Charter
        } else {
            assignment.snapshot = Some(bytes(self.limits.snapshot_bytes.saturating_add(1)));
            Invalid::Snapshot
        };
        (assignment, Some(invalid))
    }

    fn inbound(&mut self, run: Token, attempt: Token) -> Vec<Act> {
        let large = self.rng.chance(self.script.large);
        let Some(assigned) = self.open.get_mut(&(run, attempt)) else {
            return Vec::new();
        };
        let next = assigned.next;
        assigned.next += 1;
        self.tally.events += 1;
        let event = if large {
            bytes(self.limits.event_bytes + 1)
        } else {
            let mut event = next.to_be_bytes().to_vec();
            event.extend_from_slice(b"event");
            event.into_boxed_slice()
        };
        vec![Self::host(Event::Inbound { name: Token::new(1), run, attempt, event }, false)]
    }

    fn cancel(&mut self, run: Token, attempt: Token) -> Vec<Act> {
        let Some(assigned) = self.open.get_mut(&(run, attempt)) else {
            return Vec::new();
        };
        if assigned.cancelled {
            return Vec::new();
        }
        assigned.cancelled = true;
        self.tally.cancels += 1;
        vec![Self::host(Event::Cancel { run, attempt }, false)]
    }

    fn relay(&mut self, run: Token, attempt: Token, call: Token) -> Vec<Act> {
        self.tally.relays += 1;
        let assigned = self.open.get(&(run, attempt)).expect("a relayed call is of an assignment in flight");
        if assigned.cancelled {
            self.tally.unanswered += 1;
            return Vec::new();
        }
        let mut acts = Vec::new();
        if self.rng.chance(self.script.stale) {
            let stale = self.name();
            let event = Event::Relayed { run, attempt: stale, call, answer: Box::from(&b"stale"[..]) };
            acts.push(Self::host(event, true));
        }
        let after = self.script.relay.draw(&mut self.rng);
        let answer = Box::from(&b"the requested record"[..]);
        acts.push(Act::Host { after, event: Event::Relayed { run, attempt, call, answer }, stale: false });
        acts
    }

    fn answered(&mut self, run: Token, attempt: Token, answer: &Answer) {
        let assigned = self.open.remove(&(run, attempt)).expect("an assignment is answered once, while in flight");
        let refused = match answer {
            Answer::Refused(Refusal::Invalid(refused)) => Some(*refused),
            Answer::Refused(Refusal::Busy) | Answer::Ended { .. } | Answer::Parked { .. } | Answer::Failed { .. } => {
                None
            }
        };
        assert_eq!(
            refused, assigned.invalid,
            "an assignment is refused as invalid for what is beyond the limits, and only then"
        );
        let work = match answer {
            Answer::Refused(_) => None,
            Answer::Ended { work, .. } | Answer::Parked { work, .. } | Answer::Failed { work, .. } => Some(work),
        };
        if let Some(work) = work {
            if let Some(left) = work.left {
                assert!(left.raw() > 0, "the workspace names what it left");
            }
            if let Some(saved) = work.saved {
                assert!(saved.raw() > 0, "the workspace names what it saved");
            }
        }
        *self.endings.entry(ending(answer)).or_default() += 1;
    }

    fn host(event: Event, stale: bool) -> Act {
        Act::Host { after: Duration::ZERO, event, stale }
    }

    fn name(&mut self) -> Token {
        self.names += 1;
        Token::new(self.names)
    }
}

/// An answer's kind, for counting endings.
#[must_use]
pub fn ending(answer: &Answer) -> &'static str {
    match answer {
        Answer::Refused(Refusal::Busy) => "refused: busy",
        Answer::Refused(Refusal::Invalid(_)) => "refused: invalid",
        Answer::Ended { .. } => "ended",
        Answer::Parked { .. } => "parked",
        Answer::Failed { failure, .. } => failure_kind(*failure),
    }
}

/// A failure's kind.
#[must_use]
pub fn failure_kind(failure: Failure) -> &'static str {
    match failure {
        Failure::Unprepared(Preparation::Transient) => "failed: unprepared, transient",
        Failure::Unprepared(Preparation::Permanent { .. }) => "failed: unprepared, permanent",
        Failure::Run(RunFailure::Model) => "failed: run, model",
        Failure::Run(RunFailure::Budget) => "failed: run, budget",
        Failure::Run(RunFailure::Policy) => "failed: run, policy",
        Failure::Run(RunFailure::Cancelled) => "failed: run, cancelled",
        Failure::Run(RunFailure::Stale) => "failed: run, stale",
        Failure::Run(RunFailure::Exhausted) => "failed: run, exhausted",
        Failure::Agent(AgentFailure::Unstarted) => "failed: agent, unstarted",
        Failure::Agent(AgentFailure::Exited) => "failed: agent, exited",
        Failure::Agent(AgentFailure::Rules) => "failed: agent, rules",
        Failure::Agent(AgentFailure::NoProgress) => "failed: agent, no progress",
        Failure::Agent(AgentFailure::WallTime) => "failed: agent, wall time",
        Failure::Cancelled(Reason::Engine) => "failed: cancelled, engine",
        Failure::Cancelled(Reason::Contact) => "failed: cancelled, contact",
        Failure::Cancelled(Reason::Shutdown) => "failed: cancelled, shutdown",
    }
}

/// Every ending a hosted run can reach, refusals included.
pub const ENDINGS: [&str; 20] = [
    "refused: busy",
    "refused: invalid",
    "ended",
    "parked",
    "failed: unprepared, transient",
    "failed: unprepared, permanent",
    "failed: run, model",
    "failed: run, budget",
    "failed: run, policy",
    "failed: run, cancelled",
    "failed: run, stale",
    "failed: run, exhausted",
    "failed: agent, unstarted",
    "failed: agent, exited",
    "failed: agent, rules",
    "failed: agent, no progress",
    "failed: agent, wall time",
    "failed: cancelled, engine",
    "failed: cancelled, contact",
    "failed: cancelled, shutdown",
];

fn bytes(len: u64) -> Box<[u8]> {
    vec![b'x'; usize::try_from(len).expect("a test length fits")].into_boxed_slice()
}
