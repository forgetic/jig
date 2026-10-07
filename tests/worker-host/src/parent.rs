//! The host's parent's capabilities, scripted: what the workspace child domain
//! does with workspaces and the agent child domain with agents and their runs,
//! played from a seed. It speaks the host's vocabulary, as the top level will
//! once it translates the two siblings', and plays their contracts:
//!
//! - A prepare is answered once, after its latency: prepared, as a workspace
//!   of its own, or not, transiently or for good. A delivery or a save is
//!   answered once, after its latency, with an outcome for each item:
//!   a read-only one is unchanged, a writable one drawn (a change landed, no
//!   change, its workspace changed, or the delivery failed). A release is a notice.
//! - A start is answered after its latency: `Started` with an agent of its
//!   own, or `Gone` at once if the agent could not be started. A started
//!   agent's run follows its script: a number of steps, each a relayed call,
//!   a delivery, a yield (then waiting for an inbound event, and parking past its
//!   idle time) or some work, then its fate: it ends, parks or fails as it
//!   says, exits without a word, hangs or overruns until the watchdog faults
//!   it, breaks the rules, or says more than the limits allow (an outcome or a
//!   snapshot). It does not
//!   wait for its calls' replies, so it may finish with calls in flight.
//! - A stop before its word winds it down, maybe with one late call, and it
//!   goes after a while, sometimes saying how its run finishes first (ended,
//!   parked, or cancelled). After its word or a fault, it goes after a while
//!   whatever it is told. `Gone` is its last event. What comes for an agent
//!   that has gone (deliveries, replies, stops) is dropped, as a stale handle
//!   is.
//!
//! It checks what it is asked as it goes: a start only in a prepared
//! workspace with no agent yet, and nothing saved or released while the
//! workspace's agent may still be running or a delivery or save is in flight.

use std::collections::{BTreeMap, BTreeSet};

use jig_worker_host::{
    AgentFailure, Ask, Delivery, DeliveryOutcome, Event, Finish, Limits, Preparation, Request, RunFailure, Workspace,
};
use skein_lib::{Duration, Rng, Token};
use skein_world::domain::Span;

/// How the parent's capabilities behave.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Script {
    /// How long a prepare takes, and the chance, per mille, that it fails
    /// transiently, and for good.
    pub prepare: Span,
    pub transient: u32,
    pub permanent: u32,
    /// How long a start takes, and the chance, per mille, that the agent
    /// cannot be started.
    pub start: Span,
    pub unstarted: u32,
    /// The most steps a run takes before its fate, and the time between them.
    pub steps: u32,
    pub step: Span,
    /// The chance, per mille, that a step is a relayed call, a delivery or a
    /// yield; otherwise it is work.
    pub relays: u32,
    pub deliveries: u32,
    pub yields: u32,
    /// How long a yielded run waits for an inbound event before it parks.
    pub idle: Span,
    /// How runs end, by weight.
    pub fates: Fates,
    /// How long an agent takes to go after its word, after a fault (the
    /// kill), and after a stop.
    pub exit: Span,
    pub kill: Span,
    pub wind: Span,
    /// How long a hung or overrunning run lasts until the watchdog faults it.
    pub watchdog: Span,
    /// The chance, per mille, that a stopped agent makes one more call, and
    /// that its run says how it finishes as it winds down.
    pub late: u32,
    pub words: u32,
    /// How long a delivery or a save takes, and the chance, per mille, for each
    /// writable item, that it has a change, that its workspace changed, and
    /// that the delivery fails.
    pub delivery: Span,
    pub changes: u32,
    pub moved: u32,
    pub failed: u32,
}

/// The weights of a run's fates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fates {
    pub ended: u32,
    pub parked: u32,
    pub failed: u32,
    pub exited: u32,
    pub hung: u32,
    pub overrun: u32,
    pub rules: u32,
    pub oversized: u32,
}

/// What the parent does next.
#[derive(Debug)]
pub enum Out {
    /// An event for the host, `after` from now.
    Host { after: Duration, event: Event },
    /// Wake the agent `agent` for its wake `wake`, `after` from now.
    Wake { after: Duration, agent: Token, wake: u64 },
}

/// What the parent counted.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tally {
    pub prepares: u32,
    pub unprepared: u32,
    pub starts: u32,
    pub unstarted: u32,
    pub relays: u32,
    pub deliveries: u32,
    pub saves: u32,
    pub releases: u32,
    pub yields: u32,
    pub messages: u32,
    pub stops: u32,
    pub late_calls: u32,
    /// Runs that said how they finish as they wound down after a stop.
    pub words: u32,
    /// Saves that came back with a workspace changed, or a delivery failed.
    pub saves_stale: u32,
    pub saves_failed: u32,
    /// Deliveries, replies and stops that found their agent gone.
    pub dropped: u32,
    /// Prepares abandoned as their runs were cancelled: each still ends as it
    /// was going to.
    pub aborts: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    Ended,
    Parked,
    Failed(RunFailure),
    Exited,
    Hung,
    Overrun,
    Rules,
    Oversized,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Running its script, `steps` steps before its fate.
    Working {
        steps: u32,
    },
    /// Yielded, with `steps` steps left once an inbound event comes.
    Waiting {
        steps: u32,
    },
    /// Hung or overrunning: the watchdog faults it at its wake.
    Stuck {
        fault: AgentFailure,
    },
    /// Stopped before its word: it goes at its wake, saying how its run
    /// finishes first if `word`.
    Stopping {
        word: bool,
    },
    /// It said its word, or was faulted: it goes at its wake.
    Exiting,
    Gone,
}

#[derive(Debug)]
struct Agent {
    owner: Token,
    fate: Fate,
    phase: Phase,
    /// Its current wake: an older one is ignored.
    wake: u64,
    /// Names its calls.
    calls: u64,
}

#[derive(Debug)]
struct Space {
    owner: Token,
    agent: Option<Token>,
    deliveries: u32,
    saving: bool,
    released: bool,
}

#[derive(Debug)]
pub struct Parent {
    script: Script,
    limits: Limits,
    rng: Rng,
    names: u64,
    /// Workspaces asked for, by the hosted run's token, until prepared.
    preparing: BTreeSet<Token>,
    /// Workspaces, by their tokens.
    spaces: BTreeMap<Token, Space>,
    /// Agents, by their tokens.
    agents: BTreeMap<Token, Agent>,
    /// Pushes in flight, by the host's token for their call: their
    /// workspaces.
    delivering: BTreeMap<Token, Token>,
    tally: Tally,
}

impl Parent {
    #[must_use]
    pub fn new(script: Script, limits: Limits, seed: u64) -> Parent {
        Parent {
            script,
            limits,
            rng: Rng::new(seed),
            names: 0,
            preparing: BTreeSet::new(),
            spaces: BTreeMap::new(),
            agents: BTreeMap::new(),
            delivering: BTreeMap::new(),
            tally: Tally::default(),
        }
    }

    #[must_use]
    pub fn tally(&self) -> Tally {
        self.tally
    }

    /// Whether everything of the hosted run `owner` has settled: its
    /// workspace, if it has one, released, and its agent, if it started one,
    /// gone, with nothing in flight.
    #[must_use]
    pub fn settled(&self, owner: Token) -> bool {
        if self.preparing.contains(&owner) {
            return false;
        }
        for space in self.spaces.values() {
            if space.owner == owner && !space.released {
                return false;
            }
        }
        for agent in self.agents.values() {
            if agent.owner == owner && agent.phase != Phase::Gone {
                return false;
            }
        }
        true
    }

    /// Checks that nothing is left: every workspace released, every agent
    /// gone, nothing in flight.
    pub fn assert_settled(&self) {
        assert!(self.preparing.is_empty(), "every prepare has ended");
        assert!(self.delivering.is_empty(), "every delivery has ended");
        for (token, space) in &self.spaces {
            assert!(space.released, "workspace {token:?} is released");
        }
        for (token, agent) in &self.agents {
            assert_eq!(agent.phase, Phase::Gone, "agent {token:?} is gone");
        }
    }

    /// Takes the host's request `request`, which is for the parent.
    #[expect(clippy::needless_pass_by_value, reason = "the world takes ownership of emitted requests")]
    pub fn take(&mut self, request: Request) -> Vec<Out> {
        match request {
            jig_worker_host::Request::Turn { .. }
            | jig_worker_host::Request::DeliverV2 { .. }
            | jig_worker_host::Request::RelayV2 { .. }
            | jig_worker_host::Request::AnswerV2 { .. }
            | jig_worker_host::Request::StartV2 { .. }
            | Request::TurnCredit { .. } => unreachable!("this script runs version one"),

            Request::Prepare { owner, workspace } => self.prepare(owner, &workspace),
            // A notice: the prepare still ends as it was going to, which the
            // world checks the host takes.
            Request::Abort { owner: _ } => {
                self.tally.aborts += 1;
                Vec::new()
            }
            Request::Start { owner, workspace, charter: _, snapshot: _, grants: _ } => {
                self.start(owner, workspace.expect("the scripted assignments have workspace items"))
            }
            Request::Deliver { agent, name: _, event: _ } => self.deliver(agent),
            Request::Reply { agent, call: _, reply: _ } => self.reply(agent),
            Request::Stop { agent } => self.stop(agent),
            Request::DeliverWorkspace { owner, workspace, message: _ } => self.delivery(owner, workspace),
            Request::Save { owner, workspace } => self.save(owner, workspace),
            Request::Release { workspace } => self.release(workspace),
            Request::Grant { .. } => Vec::new(),
            Request::Answer { .. }
            | Request::Relay { .. }
            | Request::CancelRelay { .. }
            | Request::Bounced { .. }
            | Request::Hosting { .. } => {
                unreachable!("for the engine or the top level")
            }
        }
    }

    /// The agent `agent`'s wake `wake` falls due.
    pub fn wake(&mut self, agent: Token, wake: u64) -> Vec<Out> {
        let entry = self.agents.get(&agent).expect("an agent is kept once made");
        if entry.wake != wake {
            return Vec::new();
        }
        let phase = entry.phase;
        match phase {
            Phase::Working { steps: 0 } => self.fate(agent),
            Phase::Working { steps } => self.act(agent, steps - 1),
            // Idle past its time: it parks, as a session does.
            Phase::Waiting { .. } => {
                let snapshot = self.snapshot();
                self.say(agent, Finish::Parked { snapshot })
            }
            Phase::Stuck { fault } => {
                let after = self.script.kill.draw(&mut self.rng);
                let mut out = vec![host(Duration::ZERO, Event::Faulted { owner: self.owner(agent), fault })];
                out.push(self.exit(agent, after));
                out
            }
            Phase::Stopping { word: true } => {
                self.tally.words += 1;
                let finish = match self.rng.below(3) {
                    0 => Finish::Ended { outcome: bytes(self.rng.between(1, self.limits.outcome_bytes)) },
                    1 => Finish::Parked { snapshot: None },
                    _ => Finish::Failed { failure: RunFailure::Cancelled },
                };
                let mut out = vec![host(Duration::ZERO, Event::Finished { owner: self.owner(agent), finish })];
                out.extend(self.goes(agent, b"cancelled"));
                out
            }
            Phase::Stopping { word: false } | Phase::Exiting => self.goes(agent, b"exited"),
            Phase::Gone => unreachable!("no wake is armed"),
        }
    }

    fn prepare(&mut self, owner: Token, workspace: &Workspace) -> Vec<Out> {
        self.tally.prepares += 1;
        assert!(self.preparing.insert(owner), "a run's workspace is prepared once");
        let after = self.script.prepare.draw(&mut self.rng);
        let event = if self.rng.chance(self.script.transient) {
            self.unprepared(owner, Preparation::Transient)
        } else if self.rng.chance(self.script.permanent) {
            self.unprepared(owner, Preparation::Permanent { resource: Some(workspace.items) })
        } else {
            let workspace = self.name();
            let space = Space { owner, agent: None, deliveries: 0, saving: false, released: false };
            self.spaces.insert(workspace, space);
            Event::Prepared { owner, workspace }
        };
        vec![host(after, event)]
    }

    fn unprepared(&mut self, owner: Token, failure: Preparation) -> Event {
        self.tally.unprepared += 1;
        Event::Unprepared { owner, failure, detail: Box::from(&b"fatal: could not read from workspace resource"[..]) }
    }

    fn start(&mut self, owner: Token, workspace: Token) -> Vec<Out> {
        self.tally.starts += 1;
        let agent = self.name();
        let space = self.spaces.get_mut(&workspace).expect("a start is in a prepared workspace");
        assert!(space.owner == owner && !space.released, "a start is in its run's workspace, before its release");
        assert!(space.agent.is_none(), "one agent per workspace");
        space.agent = Some(agent);
        let fate = self.draw_fate();
        let after = self.script.start.draw(&mut self.rng);
        if self.rng.chance(self.script.unstarted) {
            self.tally.unstarted += 1;
            let entry = Agent { owner, fate, phase: Phase::Gone, wake: 0, calls: 0 };
            self.agents.insert(agent, entry);
            return vec![host(after, Event::Gone { owner, detail: Box::from(&b"spawn: no such file"[..]) })];
        }
        let steps = u32::try_from(self.rng.below(u64::from(self.script.steps) + 1)).expect("fits");
        let entry = Agent { owner, fate, phase: Phase::Working { steps }, wake: 0, calls: 0 };
        self.agents.insert(agent, entry);
        let mut out = vec![host(after, Event::Started { owner, agent })];
        let step = self.script.step.draw(&mut self.rng);
        out.push(self.rearm(agent, after.saturating_add(step)));
        out
    }

    /// The run's next step, with `steps` left after it.
    fn act(&mut self, agent: Token, steps: u32) -> Vec<Out> {
        let mut out = Vec::new();
        let roll = u32::try_from(self.rng.below(1000)).expect("fits");
        let (relays, deliveries, yields) = (self.script.relays, self.script.deliveries, self.script.yields);
        if roll < relays {
            out.push(self.call(agent, Ask::Relay { body: Box::from(&b"read record"[..]) }));
        } else if roll < relays + deliveries {
            out.push(self.call(agent, Ask::Deliver { message: Box::from(&b"fix: the thing"[..]) }));
        } else if roll < relays + deliveries + yields {
            self.tally.yields += 1;
            self.set(agent, Phase::Waiting { steps });
            out.push(host(Duration::ZERO, Event::Yielded { owner: self.owner(agent) }));
            let idle = self.script.idle.draw(&mut self.rng);
            out.push(self.rearm(agent, idle));
            return out;
        }
        self.set(agent, Phase::Working { steps });
        let step = self.script.step.draw(&mut self.rng);
        out.push(self.rearm(agent, step));
        out
    }

    /// The run's fate, once its steps are done.
    fn fate(&mut self, agent: Token) -> Vec<Out> {
        let fate = self.agents.get(&agent).expect("an agent is kept once made").fate;
        match fate {
            Fate::Ended => {
                let len = self.rng.between(1, self.limits.outcome_bytes);
                self.say(agent, Finish::Ended { outcome: bytes(len) })
            }
            Fate::Parked => {
                let snapshot = self.snapshot();
                self.say(agent, Finish::Parked { snapshot })
            }
            Fate::Failed(failure) => self.say(agent, Finish::Failed { failure }),
            Fate::Oversized => {
                let finish = if self.rng.chance(500) {
                    Finish::Ended { outcome: bytes(self.limits.outcome_bytes + 1) }
                } else {
                    Finish::Parked { snapshot: Some(bytes(self.limits.snapshot_bytes + 1)) }
                };
                self.say(agent, finish)
            }
            Fate::Exited => self.goes(agent, b"panicked at 'index out of bounds'"),
            Fate::Hung => self.stuck(agent, AgentFailure::NoProgress),
            Fate::Overrun => self.stuck(agent, AgentFailure::WallTime),
            Fate::Rules => {
                let after = self.script.kill.draw(&mut self.rng);
                let fault = Event::Faulted { owner: self.owner(agent), fault: AgentFailure::Rules };
                vec![host(Duration::ZERO, fault), self.exit(agent, after)]
            }
        }
    }

    fn stuck(&mut self, agent: Token, fault: AgentFailure) -> Vec<Out> {
        self.set(agent, Phase::Stuck { fault });
        let after = self.script.watchdog.draw(&mut self.rng);
        vec![self.rearm(agent, after)]
    }

    /// The run says how it finishes, and its agent exits after a while.
    fn say(&mut self, agent: Token, finish: Finish) -> Vec<Out> {
        let after = self.script.exit.draw(&mut self.rng);
        vec![host(Duration::ZERO, Event::Finished { owner: self.owner(agent), finish }), self.exit(agent, after)]
    }

    fn exit(&mut self, agent: Token, after: Duration) -> Out {
        self.set(agent, Phase::Exiting);
        self.rearm(agent, after)
    }

    /// The agent and everything it started are gone.
    fn goes(&mut self, agent: Token, detail: &[u8]) -> Vec<Out> {
        self.set(agent, Phase::Gone);
        vec![host(Duration::ZERO, Event::Gone { owner: self.owner(agent), detail: Box::from(detail) })]
    }

    fn call(&mut self, agent: Token, ask: Ask) -> Out {
        match ask {
            jig_worker_host::Ask::DeliverV2 { .. } => unreachable!("this script runs version one"),

            Ask::Relay { .. } => self.tally.relays += 1,
            Ask::Deliver { .. } => self.tally.deliveries += 1,
        }
        let entry = self.agents.get_mut(&agent).expect("an agent is kept once made");
        entry.calls += 1;
        let call = Token::new(entry.calls);
        host(Duration::ZERO, Event::Called { owner: entry.owner, call, ask })
    }

    fn deliver(&mut self, agent: Token) -> Vec<Out> {
        let phase = self.agents.get(&agent).expect("a delivery is for an agent that started").phase;
        match phase {
            Phase::Waiting { steps } => {
                self.tally.messages += 1;
                self.set(agent, Phase::Working { steps });
                let step = self.script.step.draw(&mut self.rng);
                vec![self.rearm(agent, step)]
            }
            Phase::Working { .. } | Phase::Stuck { .. } | Phase::Stopping { .. } | Phase::Exiting => {
                self.tally.messages += 1;
                Vec::new()
            }
            Phase::Gone => {
                self.tally.dropped += 1;
                Vec::new()
            }
        }
    }

    fn reply(&mut self, agent: Token) -> Vec<Out> {
        let entry = self.agents.get(&agent).expect("a reply is for an agent that started");
        if entry.phase == Phase::Gone {
            self.tally.dropped += 1;
        }
        Vec::new()
    }

    fn stop(&mut self, agent: Token) -> Vec<Out> {
        let phase = self.agents.get(&agent).expect("a stop is for an agent that started").phase;
        match phase {
            Phase::Working { .. } | Phase::Waiting { .. } | Phase::Stuck { .. } => {
                self.tally.stops += 1;
                let mut out = Vec::new();
                if self.rng.chance(self.script.late) {
                    self.tally.late_calls += 1;
                    out.push(self.call(agent, Ask::Relay { body: Box::from(&b"one more"[..]) }));
                }
                let word = self.rng.chance(self.script.words);
                self.set(agent, Phase::Stopping { word });
                let after = self.script.wind.draw(&mut self.rng);
                out.push(self.rearm(agent, after));
                out
            }
            Phase::Stopping { .. } | Phase::Exiting => Vec::new(),
            Phase::Gone => {
                self.tally.dropped += 1;
                Vec::new()
            }
        }
    }

    fn delivery(&mut self, call: Token, workspace: Token) -> Vec<Out> {
        let space = self.spaces.get_mut(&workspace).expect("a delivery is in a prepared workspace");
        assert!(!space.released, "a delivery is before release");
        space.deliveries += 1;
        assert!(self.delivering.insert(call, workspace).is_none(), "a call delivers once");
        let changed = self.rng.chance(self.script.changes);
        let outcome = if !changed {
            DeliveryOutcome::Nothing
        } else if self.rng.chance(self.script.moved) {
            DeliveryOutcome::Stale
        } else if self.rng.chance(self.script.failed) {
            if self.rng.chance(500) { DeliveryOutcome::Failed } else { DeliveryOutcome::Refused }
        } else {
            DeliveryOutcome::Delivered
        };
        let delivery = Delivery { outcome, left: workspace, changed: changed && outcome == DeliveryOutcome::Delivered };
        let after = self.script.delivery.draw(&mut self.rng);
        vec![host(after, Event::Delivered { owner: call, delivery })]
    }

    fn save(&mut self, owner: Token, workspace: Token) -> Vec<Out> {
        self.tally.saves += 1;
        self.check_quiet(workspace, "saved");
        let space = self.spaces.get_mut(&workspace).expect("checked above");
        assert!(space.owner == owner && !space.saving, "a run saves once, in its workspace");
        space.saving = true;
        let at = if self.rng.chance(self.script.moved) {
            self.tally.saves_stale += 1;
            None
        } else if self.rng.chance(self.script.failed) {
            self.tally.saves_failed += 1;
            None
        } else {
            Some(workspace)
        };
        let after = self.script.delivery.draw(&mut self.rng);
        vec![host(after, Event::Saved { owner, at })]
    }

    fn release(&mut self, workspace: Token) -> Vec<Out> {
        self.tally.releases += 1;
        self.check_quiet(workspace, "released");
        let space = self.spaces.get_mut(&workspace).expect("checked above");
        space.released = true;
        Vec::new()
    }

    /// Checks that nothing runs in `workspace`, and nothing is in flight for
    /// it, before it is `what`.
    fn check_quiet(&self, workspace: Token, what: &str) {
        let space = self.spaces.get(&workspace).expect("a prepared workspace");
        assert!(!space.released, "nothing is {what} after its release");
        assert_eq!(space.deliveries, 0, "nothing is {what} while a delivery is in flight");
        assert!(!space.saving, "nothing is {what} while a save is in flight");
        if let Some(agent) = space.agent {
            let phase = self.agents.get(&agent).expect("an agent is kept once made").phase;
            assert_eq!(phase, Phase::Gone, "nothing is {what} while the run's agent may still be running");
        }
    }

    /// The prepare of `owner` has ended, as the host takes its terminal.
    pub fn prepared(&mut self, owner: Token) {
        assert!(self.preparing.remove(&owner), "a prepare ends once");
    }

    /// The delivery `call` has ended, as the world delivers its `Delivered`.
    pub fn delivered(&mut self, call: Token) {
        let workspace = self.delivering.remove(&call).expect("a delivery ends once");
        let space = self.spaces.get_mut(&workspace).expect("a delivered workspace");
        space.deliveries -= 1;
    }

    /// The save of `owner` has ended, as the world delivers its `Saved`.
    pub fn saved(&mut self, owner: Token) {
        for space in self.spaces.values_mut() {
            if space.owner == owner && space.saving {
                space.saving = false;
                return;
            }
        }
        unreachable!("a save ends once");
    }

    fn snapshot(&mut self) -> Option<Box<[u8]>> {
        if self.rng.chance(500) { Some(bytes(self.rng.between(1, self.limits.snapshot_bytes))) } else { None }
    }

    fn draw_fate(&mut self) -> Fate {
        let f = self.script.fates;
        let weights = [f.ended, f.parked, f.failed, f.exited, f.hung, f.overrun, f.rules, f.oversized];
        let total: u32 = weights.iter().sum();
        assert!(total > 0, "some fate has weight");
        let mut roll = u32::try_from(self.rng.below(u64::from(total))).expect("fits");
        let mut chosen = 0;
        for (index, weight) in weights.iter().enumerate() {
            if roll < *weight {
                chosen = index;
                break;
            }
            roll -= weight;
        }
        match chosen {
            0 => Fate::Ended,
            1 => Fate::Parked,
            2 => {
                let kinds = [
                    RunFailure::Model,
                    RunFailure::Budget,
                    RunFailure::Policy,
                    RunFailure::Cancelled,
                    RunFailure::Stale,
                    RunFailure::Exhausted,
                ];
                Fate::Failed(kinds[usize::try_from(self.rng.below(6)).expect("fits")])
            }
            3 => Fate::Exited,
            4 => Fate::Hung,
            5 => Fate::Overrun,
            6 => Fate::Rules,
            _ => Fate::Oversized,
        }
    }

    fn owner(&self, agent: Token) -> Token {
        self.agents.get(&agent).expect("an agent is kept once made").owner
    }

    fn set(&mut self, agent: Token, phase: Phase) {
        self.agents.get_mut(&agent).expect("an agent is kept once made").phase = phase;
    }

    /// Arms the agent's next wake, `after` from now, replacing the one before.
    fn rearm(&mut self, agent: Token, after: Duration) -> Out {
        let entry = self.agents.get_mut(&agent).expect("an agent is kept once made");
        entry.wake += 1;
        Out::Wake { after, agent, wake: entry.wake }
    }

    fn name(&mut self) -> Token {
        self.names += 1;
        Token::new(self.names)
    }
}

fn host(after: Duration, event: Event) -> Out {
    Out::Host { after, event }
}

fn bytes(len: u64) -> Box<[u8]> {
    vec![b'x'; usize::try_from(len).expect("a test length fits")].into_boxed_slice()
}
