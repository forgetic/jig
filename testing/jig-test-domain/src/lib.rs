//! The testing application's root. It owns two numbered test connectors and
//! one commit door; the core owns every cross-child decision. The root only
//! translates connector values and wraps records (`domain/root.md`, 3–4).
//!
//! | State | Input | Next state | Output |
//! | --- | --- | --- | --- |
//! | open | admitted core or connector event | decision held | at most one commit |
//! | decision held | committed number | durable | held outputs may leave |
//! | open | failed number | stopped | stop, no later delivery |

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use jig_core as core;
use jig_core_fleet as fleet;
use jig_core_tasks as tasks;
use jig_test_connector as connector;
use skein_lib::{Decision, Env, Journal, JournalLimits, JournalRoom, Map, Queue, Token};

/// A store address owned by the core or one numbered connector.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Key {
    /// A core or child address.
    Core(core::Key),
    /// A connector-owned address.
    Connector { number: u16, key: connector::RecordKey },
}

/// A durable row owned by the core or one numbered connector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Record {
    /// A core or child row.
    Core(core::Record),
    /// A connector-owned row.
    Connector { number: u16, record: connector::Record },
}

impl Record {
    /// The row's stable typed store address.
    #[must_use]
    pub fn key(&self) -> Key {
        match self {
            Self::Core(row) => Key::Core(core_key(row)),
            Self::Connector { number, record } => Key::Connector { number: *number, key: record.key() },
        }
    }
}

fn core_key(row: &core::Record) -> core::Key {
    match row {
        core::Record::Core(row) => core::Key::Core(match row {
            core::CoreRecord::Deployment(_) => core::CoreKey::Deployment,
            core::CoreRecord::Call(row) => core::CoreKey::Call(row.key),
            core::CoreRecord::RunProof(row) => core::CoreKey::RunProof(row.task),
            core::CoreRecord::Turn(row) => core::CoreKey::Turn { task: row.task, attempt: row.attempt, turn: row.turn },
            core::CoreRecord::Terminal(row) => core::CoreKey::Terminal { task: row.task, attempt: row.attempt },
            core::CoreRecord::ProposalDecision(row) => core::CoreKey::ProposalDecision(row.proposal),
            core::CoreRecord::EscalationDecision(row) => {
                core::CoreKey::EscalationDecision { task: row.task, revision: row.revision }
            }
        }),
        core::Record::Tasks(row) => core::Key::Tasks(row.key()),
        core::Record::People(row) => core::Key::People(row.key()),
        core::Record::Notes(row) => core::Key::Notes(match row {
            jig_core_notes::Record::Entry(entry) => jig_core_notes::Key::Entry { name: entry.name },
            jig_core_notes::Record::Line(line) => {
                jig_core_notes::Key::Line { scope: line.scope.clone(), name: line.name }
            }
        }),
    }
}

/// One mutation in an atomic decision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Write {
    /// Replace the row under its key.
    Save(Record),
    /// Remove the row under its key.
    Erase(Key),
}

/// What the journal releases after the associated commit.
#[derive(Debug)]
pub enum Delivery {
    /// Core-held party, host, task or view output.
    Core(core::Held),
    /// A prepared worker assignment, after the claim is durable.
    Assigned { channel: Token, assignment: Assignment },
    /// A post-commit continuation inside the core's fleet.
    Fleet(fleet::Event),
    /// Connector output to its fake system.
    System { connector: u16, call: connector::SystemRequest },
    /// A procedure is due at its owning connector.
    Procedure { task: u64, step: u64, connector: u16, code: u16 },
}

/// The root's assembled, typed worker assignment.
#[derive(Debug)]
pub struct Assignment {
    /// Claimed task and activation fence.
    pub task: u64,
    pub attempt: u64,
    /// Charter and task-specific policy and contract.
    pub charter: u32,
    pub run: Box<core::RunCharter>,
    /// Whole words and prior committed conversation offered to the worker.
    pub inbox: Box<[tasks::Word]>,
    pub transcript: Box<[Box<[u8]>]>,
    /// The selected account grant.
    pub grant: jig_core_accounts::Grant,
}

/// What the root asks its surrounding world to do.
#[derive(Debug)]
pub enum Request {
    /// Apply one numbered transaction as a whole.
    Commit { number: u64, writes: Queue<Write> },
    /// Release one output only after its prerequisite commit.
    Deliver(Delivery),
    /// An immediate observation with no durability dependency.
    Now(core::Now),
    /// A failed store commit stopped this root.
    Stop,
}

/// A test input at the root boundary.
#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "the testing root moves admitted core events by value")]
pub enum Event {
    /// A party, host, account or other event in jig's vocabulary.
    Core(core::Event),
    /// One numbered connector's input from its system or a scripted peer.
    Connector { number: u16, event: connector::Event },
    /// A scripted worker's retained turn, including its opaque body.
    Turn { channel: Token, task: u64, attempt: u64, turn: u32, cumulative: u64, transcript: Box<[u8]> },
    /// A scripted worker's retained terminal answer.
    Answer { channel: Token, task: u64, attempt: u64, cumulative: u64, end: tasks::End },
    /// Store acknowledgement of this exact commit number.
    Committed { number: u64 },
    /// A store failure of this exact commit number.
    Failed { number: u64 },
}

/// Finite room reserved before either child changes state.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The core's bounded state and route buffers.
    pub core: core::Limits,
    /// Each test connector's bounded state and output.
    pub connector: connector::Limits,
    /// The application's journal capacities.
    pub journal: JournalLimits,
    /// At most this many synchronous child continuations per decision.
    pub routes: u32,
}

/// The testing application's two numbered connectors.
#[derive(Debug)]
pub struct Config {
    /// The core's configuration.
    pub core: core::Config,
    /// Connector one.
    pub first: connector::Config,
    /// Connector two.
    pub second: connector::Config,
}

/// The root's live components and one commit barrier.
#[derive(Debug)]
pub struct Domain {
    core: core::Core,
    first: connector::Domain,
    second: connector::Domain,
    journal: Journal<Write, Delivery>,
    now: Queue<core::Now>,
    assignments: Map<u64, Assignment>,
    procedure_steps: Map<u64, (u64, u16)>,
    payloads: Map<Token, Payload>,
    next_payload: u64,
    stopped_reported: bool,
}

impl Domain {
    /// Build the testing application without issuing a store request.
    #[must_use]
    pub fn new(config: Config, limits: &Limits) -> Domain {
        assert!(limits.routes > 0 && limits.journal.writes > 0 && limits.journal.held > 0, "finite route room");
        Domain {
            core: core::Core::new(config.core, &limits.core),
            first: connector::Domain::new(config.first, &limits.connector),
            second: connector::Domain::new(config.second, &limits.connector),
            journal: Journal::new(&limits.journal),
            now: Queue::with_capacity(limits.journal.now),
            assignments: Map::with_capacity(limits.core.tasks.tasks),
            procedure_steps: Map::with_capacity(limits.core.tasks.tasks),
            payloads: Map::with_capacity(
                limits.core.fleet.turns.checked_add(limits.core.fleet.attempts).expect("payload room"),
            ),
            next_payload: 1,
            stopped_reported: false,
        }
    }

    /// Whether every accepted decision and output has settled.
    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.journal.idle() && self.now.is_empty() && self.core.due.is_empty() && self.assignments.is_empty()
    }
}

#[expect(clippy::large_enum_variant, reason = "the bounded route queue owns complete core events")]
enum Work {
    Core(core::Event),
    ResumeFleet,
    Connector { number: u16, event: connector::Event },
    Turn { channel: Token, task: u64, attempt: u64, turn: u32, cumulative: u64, transcript: Box<[u8]> },
    Answer { channel: Token, task: u64, attempt: u64, cumulative: u64, end: tasks::End },
}

#[derive(Debug)]
enum Payload {
    Turn { task: u64, attempt: u64, turn: u32, cumulative: u64, transcript: Box<[u8]> },
    Answer { task: u64, attempt: u64, cumulative: u64, end: tasks::End },
}

/// Admit one input and route all synchronous continuations inside one decision.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event) {
    match event {
        Event::Committed { number } => domain.journal.committed(number),
        Event::Failed { number } => domain.journal.failed(number),
        Event::Core(event) => decide(domain, env, Work::Core(event)),
        Event::Connector { number, event } => decide(domain, env, Work::Connector { number, event }),
        Event::Turn { channel, task, attempt, turn, cumulative, transcript } => {
            decide(domain, env, Work::Turn { channel, task, attempt, turn, cumulative, transcript });
        }
        Event::Answer { channel, task, attempt, cumulative, end } => {
            decide(domain, env, Work::Answer { channel, task, attempt, cumulative, end });
        }
    }
}

fn decide(domain: &mut Domain, env: &Env<Limits>, first: Work) {
    let room = JournalRoom { writes: env.limits.journal.writes, held: env.limits.journal.held };
    let Some(mut decision) = domain.journal.decision(&room) else { return };
    let mut work = Queue::with_capacity(env.limits.routes);
    work.push(first);
    for _ in 0..env.limits.routes {
        let Some(next) = work.pop() else { break };
        match next {
            Work::Core(event) => {
                let requests =
                    core::step(&mut domain.core, &Env { now: env.now, wall: env.wall, limits: env.limits.core }, event);
                route_core(domain, env, &mut decision, requests, &mut work);
            }
            Work::ResumeFleet => {
                let requests = core::resume_fleet(
                    &mut domain.core,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.core },
                );
                route_core(domain, env, &mut decision, requests, &mut work);
            }
            Work::Connector { number, event } => {
                let mut requests = Queue::with_capacity(connector::MAX_OUT);
                let connector = numbered(domain, number);
                connector::step(
                    connector,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.connector },
                    event,
                    &mut requests,
                );
                route_connector(domain, env, &mut decision, number, &mut requests, &mut work);
            }
            Work::Turn { channel, task, attempt, turn, cumulative, transcript } => {
                let body = payload(domain, Payload::Turn { task, attempt, turn, cumulative, transcript });
                work.push(Work::Core(core::Event::Fleet(fleet::Event::Turn {
                    channel,
                    run: Token::new(task),
                    attempt: Token::new(attempt),
                    turn,
                    body,
                })));
            }
            Work::Answer { channel, task, attempt, cumulative, end } => {
                let body = payload(domain, Payload::Answer { task, attempt, cumulative, end });
                work.push(Work::Core(core::Event::Fleet(fleet::Event::Answer {
                    channel,
                    run: Token::new(task),
                    attempt: Token::new(attempt),
                    answer: fleet::Answer::Ended,
                    payload: body,
                })));
            }
        }
    }
    assert!(work.is_empty(), "synchronous routes fit configured room");
    domain.journal.accept(decision);
}

fn payload(domain: &mut Domain, value: Payload) -> Token {
    let token = Token::new(domain.next_payload);
    domain.next_payload = domain.next_payload.checked_add(1).expect("bounded test flight numbering");
    assert!(domain.payloads.insert(token, value).is_ok(), "payload room reserved");
    token
}

fn numbered(domain: &mut Domain, number: u16) -> &mut connector::Domain {
    match number {
        1 => &mut domain.first,
        2 => &mut domain.second,
        _ => unreachable!("testing application has exactly two connectors"),
    }
}

/// Drain at most one commit and all currently durable outputs.
pub fn release(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    if domain.journal.stopped() {
        if !domain.stopped_reported {
            domain.stopped_reported = true;
            out.push(Request::Stop);
        }
        return;
    }
    if let Some(commit) = domain.journal.commit() {
        out.push(Request::Commit { number: commit.number, writes: commit.writes });
    }
    let mut ready = Queue::with_capacity(env.limits.journal.release);
    let _released = domain.journal.release(&mut ready);
    for _ in 0..ready.len() {
        let delivery = ready.pop().expect("release count");
        match delivery {
            Delivery::Core(core::Held::TaskTurnKept { task, attempt, turn }) => {
                step(
                    domain,
                    env,
                    Event::Core(core::Event::Fleet(jig_core_fleet::Event::TurnKept {
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                        turn,
                    })),
                );
            }
            Delivery::Core(core::Held::TaskTerminalAcknowledged { task, attempt }) => {
                step(
                    domain,
                    env,
                    Event::Core(core::Event::Fleet(jig_core_fleet::Event::Acknowledge {
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                    })),
                );
            }
            Delivery::Fleet(event) => step(domain, env, Event::Core(core::Event::Fleet(event))),
            Delivery::Core(core::Held::StopRun { task, attempt }) => {
                step(
                    domain,
                    env,
                    Event::Core(core::Event::Fleet(fleet::Event::Cancel {
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                    })),
                );
            }
            Delivery::Core(core::Held::Relay { task, attempt, previous, word }) => {
                let event = Token::new(word.number);
                assert!(
                    domain.core.relaying.replace(core::PendingRelay { previous, word }).is_none(),
                    "one committed relay in flight"
                );
                step(
                    domain,
                    env,
                    Event::Core(core::Event::Fleet(fleet::Event::Inbound {
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                        event,
                    })),
                );
            }
            other @ (Delivery::Core(_)
            | Delivery::Assigned { .. }
            | Delivery::System { .. }
            | Delivery::Procedure { .. }) => out.push(Request::Deliver(other)),
        }
    }
    for _ in 0..domain.now.len() {
        let now = domain.now.pop().expect("bounded now count");
        out.push(Request::Now(now));
    }
    if domain.journal.idle() {
        if domain.core.accounts.usable(domain.core.settings.account)
            && let Some(context) = domain.core.due.pop()
        {
            decide(domain, env, Work::Core(core::Event::Activate { context, ready: true }));
            return;
        }
        decide(domain, env, Work::ResumeFleet);
    }
}

fn write(decision: &mut Decision<Write, Delivery>, value: Write) {
    assert!(decision.write(value).is_ok(), "reserved write room");
}

fn hold(decision: &mut Decision<Write, Delivery>, value: Delivery) {
    assert!(decision.hold(value).is_ok(), "reserved delivery room");
}

// The translations below are deliberately exhaustive: a new boundary variant
// must make the testing application say how it crosses its root.
mod routes;
use routes::{route_connector, route_core};
