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
use skein_lib::{Decision, Env, Journal, JournalLimits, JournalRoom, Map, Queue, ReplyTo, Token, Wall};

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
    /// One committed inbox word for its current host.
    Message { channel: Token, task: u64, attempt: u64, word: tasks::Word },
    /// Perform this step of the core-owned restart script.
    Restart(core::RestartStep),
    /// Core-held party, host, task or view output.
    Core(core::Held),
    /// A typed settled answer with its original opaque call name.
    CallAnswer { channel: Token, task: u64, attempt: u64, name: Box<[u8]>, call: core::SettledCall },
    /// A prepared worker assignment, after the claim is durable.
    Assigned { channel: Token, assignment: Assignment },
    /// A post-commit continuation inside the core's fleet.
    Fleet(fleet::Event),
    /// Connector output to its fake system.
    System { connector: u16, call: connector::SystemRequest },
    /// A fresh read owed after this run's loss committed.
    LostRead { connector: u16, task: u64, attempt: u64, resource: tasks::Name, call: connector::SystemRequest },
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
    pub answered: Box<[core::SettledCall]>,
    /// The selected account grant.
    pub grant: jig_core_accounts::Grant,
}

/// What the root asks its surrounding world to do.
#[derive(Debug)]
pub enum Request {
    /// First cold load, before the deployment counter is known.
    Restart(core::RestartStep),
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
    /// A correlated reply to the connector's fresh read after a lost writer.
    WriterRead {
        connector: u16,
        resource: tasks::Name,
        event: connector::SystemEvent,
    },
    /// A numbered connector's independent retry deadline elapsed.
    ConnectorTimer {
        number: u16,
    },
    RestartBegin,
    RestartDone(core::RestartStep),
    /// Drive one due child timer through the same decision barrier.
    Timer(core::Timer),
    /// A party, host, account or other event in jig's vocabulary.
    Core(core::Event),
    /// One numbered connector's input from its system or a scripted peer.
    Connector {
        number: u16,
        event: connector::Event,
    },
    /// A decoded named effect call, whose payload stays with its connector.
    EffectCall {
        to: ReplyTo,
        key: core::CallKey,
        number: u16,
        effect: connector::Effect,
        deadline: Wall,
        proposal: Option<Box<[u8]>>,
    },
    /// A scripted worker's retained turn, including its opaque body.
    Turn {
        channel: Token,
        task: u64,
        attempt: u64,
        turn: u32,
        cumulative: u64,
        read: Option<u64>,
        transcript: Box<[u8]>,
    },
    /// A scripted worker's retained terminal answer.
    Answer {
        channel: Token,
        task: u64,
        attempt: u64,
        cumulative: u64,
        end: tasks::End,
    },
    /// A bounded store page of prior opaque turns; pages arrive oldest first.
    TranscriptLoaded {
        task: u64,
        rows: Box<[core::TurnRecord]>,
        done: bool,
    },
    /// Store acknowledgement of this exact commit number.
    Committed {
        number: u64,
    },
    /// A store failure of this exact commit number.
    Failed {
        number: u64,
    },
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
    /// Host kinds the test application's charter permits.
    pub hosting: fleet::Kinds,
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
    hosting: fleet::Kinds,
    core: core::Core,
    first: connector::Domain,
    second: connector::Domain,
    journal: Journal<Write, Delivery>,
    now: Queue<core::Now>,
    assignments: Map<u64, Assignment>,
    workspace_writes: Map<u64, Box<[tasks::Name]>>,
    procedure_steps: Map<u64, (u64, u16)>,
    effects: Map<Token, connector::Effect>,
    effect_procedures: Map<Token, u64>,
    outbox_procedures: Map<u64, u16>,
    judges: Map<Token, (Token, jig_core_authority::Judge, [u8; 32])>,
    outbox_tasks: Map<u64, u64>,
    payloads: Map<Token, Payload>,
    next_payload: u64,
    decision_wrote: bool,
    stopped_reported: bool,
    restart_active: bool,
    restart_load: Option<core::RestartStep>,
}

impl Domain {
    /// Install the application's fixed hold vocabulary before its first input.
    pub fn configure_holds(&mut self, env: &Env<Limits>, connector: u16, kinds: Box<[tasks::Kind]>) {
        let mut out = Queue::with_capacity(tasks::max_out(&env.limits.core.tasks));
        tasks::step(
            &mut self.core.tasks,
            &Env { now: env.now, wall: env.wall, limits: env.limits.core.tasks },
            tasks::Event::Kinds { connector, kinds },
            &mut out,
        );
        assert!(out.is_empty(), "fixed hold vocabulary has no effects");
    }
    /// Build the testing application without issuing a store request.
    #[must_use]
    pub fn new(config: Config, limits: &Limits) -> Domain {
        assert!(limits.routes > 0 && limits.journal.writes > 0 && limits.journal.held > 0, "finite route room");
        let core_room = core::room_max(&limits.core).expect("valid core room");
        let connector_room = connector::MAX_OUT.checked_mul(2).expect("two connector outputs");
        let writes = core_room.writes.checked_add(connector_room).expect("combined route writes fit");
        let held = core_room.held.checked_add(connector_room).expect("combined route deliveries fit");
        assert!(
            writes <= limits.journal.writes && held <= limits.journal.held,
            "journal admits the whole core and connector route before stepping"
        );
        Domain {
            hosting: config.hosting,
            core: core::Core::new(config.core, &limits.core),
            first: connector::Domain::new(config.first, &limits.connector),
            second: connector::Domain::new(config.second, &limits.connector),
            journal: Journal::new(&limits.journal),
            now: Queue::with_capacity(limits.journal.now),
            assignments: Map::with_capacity(limits.core.tasks.tasks),
            workspace_writes: Map::with_capacity(limits.core.tasks.tasks),
            procedure_steps: Map::with_capacity(limits.core.tasks.tasks),
            effects: Map::with_capacity(
                limits.core.call_records.checked_add(limits.core.tasks.tasks).expect("effect room"),
            ),
            effect_procedures: Map::with_capacity(limits.core.tasks.tasks),
            outbox_procedures: Map::with_capacity(limits.connector.entries.checked_mul(2).expect("outbox procedures")),
            judges: Map::with_capacity(limits.core.authority.facts),
            outbox_tasks: Map::with_capacity(limits.connector.entries.checked_mul(2).expect("outboxes")),
            payloads: Map::with_capacity(
                limits.core.fleet.turns.checked_add(limits.core.fleet.attempts).expect("payload room"),
            ),
            next_payload: 1,
            decision_wrote: false,
            stopped_reported: false,
            restart_active: false,
            restart_load: None,
        }
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        !self.restart_active || self.core.restart_ready()
    }

    /// The client's current held-task context, admitted by the core's visibility rule.
    #[must_use]
    pub fn escalation(&self, person: u64, task: u64) -> Option<Box<tasks::EscalationContext>> {
        let context = self.core.tasks.escalation(task)?;
        let role = self.core.people.role(person, context.project);
        if self.core.escalation_visible(&context, person, role) { Some(context) } else { None }
    }

    /// Release retired child slots after every complete application iteration.
    pub fn reclaim(&mut self) {
        self.core.reclaim();
    }

    /// Whether no work can advance without another input. An assignment may
    /// still wait for a host whose next hello supplies its capacity.
    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.journal.idle()
            && self.now.is_empty()
            && (!self.ready() || self.core.due.is_empty() || !self.core.accounts.usable(self.core.settings.account))
    }
}

#[expect(clippy::large_enum_variant, reason = "the bounded route queue owns complete core events")]
enum Work {
    ConnectorFire {
        number: u16,
    },
    ConnectorResume {
        number: u16,
    },
    Restart(core::RestartRequest),
    AdoptRestored,
    AdoptDone,
    WriterRead {
        connector: u16,
        resource: tasks::Name,
        event: connector::SystemEvent,
    },
    Timer(core::Timer),
    Core(core::Event),
    ResumeFleet,
    Connector {
        number: u16,
        event: connector::Event,
    },
    EffectCall {
        to: ReplyTo,
        key: core::CallKey,
        number: u16,
        effect: connector::Effect,
        deadline: Wall,
        proposal: Option<Box<[u8]>>,
    },
    Turn {
        channel: Token,
        task: u64,
        attempt: u64,
        turn: u32,
        cumulative: u64,
        read: Option<u64>,
        transcript: Box<[u8]>,
    },
    Answer {
        channel: Token,
        task: u64,
        attempt: u64,
        cumulative: u64,
        end: tasks::End,
    },
}

#[derive(Debug)]
enum Payload {
    Turn { task: u64, attempt: u64, turn: u32, cumulative: u64, read: Option<u64>, transcript: Box<[u8]> },
    Answer { task: u64, attempt: u64, cumulative: u64, end: tasks::End },
    SettledCall(core::SettledCall),
    InboxWord(tasks::Word),
}

/// Admit one input and route all synchronous continuations inside one decision.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event) {
    match event {
        Event::ConnectorTimer { number } => decide(domain, env, Work::ConnectorFire { number }),
        Event::WriterRead { connector, resource, event } => {
            decide(domain, env, Work::WriterRead { connector, resource, event });
        }
        Event::RestartBegin => {
            domain.restart_active = true;
            match domain.core.restart_begin() {
                core::RestartRequest::Step(step) => domain.restart_load = Some(step),
                core::RestartRequest::Idle | core::RestartRequest::Refused { .. } => {}
            }
        }
        Event::RestartDone(step) => {
            let request = domain.core.restart_done(step);
            decide(domain, env, Work::Restart(request));
        }
        Event::TranscriptLoaded { task, rows, done } => {
            if !domain.core.transcripts.contains_key(&task) {
                return;
            }
            if rows.len() > usize::try_from(env.limits.core.resume_bytes).expect("u32 fits usize") {
                return;
            }
            for row in rows {
                if !domain.core.append_transcript(task, row) {
                    decide(domain, env, Work::Core(core::Event::PreparationFailed { task }));
                    return;
                }
            }
            if done {
                decide(domain, env, Work::Core(core::Event::StartBrief { task }));
            }
        }
        Event::Committed { number } => domain.journal.committed(number),
        Event::Failed { number } => domain.journal.failed(number),
        Event::EffectCall { to, key, number, effect, deadline, proposal } => {
            decide(domain, env, Work::EffectCall { to, key, number, effect, deadline, proposal });
        }
        Event::Timer(timer) => decide(domain, env, Work::Timer(timer)),
        Event::Core(event) => decide(domain, env, Work::Core(event)),
        Event::Connector { number, event } => decide(domain, env, Work::Connector { number, event }),
        Event::Turn { channel, task, attempt, turn, cumulative, read, transcript } => {
            decide(domain, env, Work::Turn { channel, task, attempt, turn, cumulative, read, transcript });
        }
        Event::Answer { channel, task, attempt, cumulative, end } => {
            decide(domain, env, Work::Answer { channel, task, attempt, cumulative, end });
        }
    }
}

#[expect(clippy::too_many_lines, reason = "one bounded decision routes the complete root work vocabulary")]
fn decide(domain: &mut Domain, env: &Env<Limits>, first: Work) {
    let core_room = match &first {
        Work::Core(event) => core::room(&env.limits.core, event),
        Work::Restart(_)
        | Work::AdoptRestored
        | Work::AdoptDone
        | Work::Timer(_)
        | Work::WriterRead { .. }
        | Work::ResumeFleet
        | Work::Connector { .. }
        | Work::ConnectorResume { .. }
        | Work::ConnectorFire { .. }
        | Work::EffectCall { .. }
        | Work::Turn { .. }
        | Work::Answer { .. } => core::room_max(&env.limits.core),
    }
    .expect("validated core room");
    let connector_room = connector::MAX_OUT.checked_mul(2).expect("two connector outputs");
    let room = JournalRoom {
        writes: core_room.writes.checked_add(connector_room).expect("combined route writes"),
        held: core_room.held.checked_add(connector_room).expect("combined route deliveries"),
    };
    let Some(mut decision) = domain.journal.decision(&room) else { return };
    domain.decision_wrote = false;
    let mut work = Queue::with_capacity(env.limits.routes);
    work.push(first);
    for _ in 0..env.limits.routes {
        let Some(next) = work.pop() else { break };
        match next {
            Work::ConnectorFire { number } => {
                let mut out = Queue::with_capacity(connector::MAX_OUT);
                connector::fire(
                    numbered(domain, number),
                    &Env { now: env.now, wall: env.wall, limits: env.limits.connector },
                    &mut out,
                );
                route_connector(domain, env, &mut decision, number, &mut out, &mut work);
            }
            Work::Restart(request) => match request {
                core::RestartRequest::Idle => {}
                core::RestartRequest::Refused { .. } => domain.stopped_reported = false,
                core::RestartRequest::Step(step) => match step {
                    core::RestartStep::AdoptRuns => {
                        work.push(Work::Core(core::Event::Tasks(tasks::Event::Restored)));
                        work.push(Work::AdoptRestored);
                    }
                    core::RestartStep::Open => {
                        let _request = domain.core.restart_done(step);
                    }
                    core::RestartStep::SettleOutbox { connector: number } => {
                        work.push(Work::Connector {
                            number,
                            event: connector::Event::Restart(connector::RestartStep::SettleOutbox),
                        });
                    }
                    core::RestartStep::LoadCore
                    | core::RestartStep::RestoreConnector { .. }
                    | core::RestartStep::ReadAfresh { .. } => hold(&mut decision, Delivery::Restart(step)),
                },
            },
            Work::AdoptRestored => {
                if domain.core.restart_admit_claims() {
                    for _ in 0..domain.core.adopted.len() {
                        work.push(Work::Core(core::Event::Fleet(
                            domain.core.adopted.pop().expect("restored claim count"),
                        )));
                    }
                    work.push(Work::Core(core::Event::Fleet(fleet::Event::Loaded)));
                    work.push(Work::AdoptDone);
                }
            }
            Work::AdoptDone => {
                let request = domain.core.restart_done(core::RestartStep::AdoptRuns);
                work.push(Work::Restart(request));
            }

            Work::EffectCall { to, key, number, effect, deadline, proposal } => {
                let owner = effect_payload(domain, effect);
                let origin = match proposal {
                    Some(reason) => core::EffectOrigin::Propose { to, key, reason, as_holder: false },
                    None => core::EffectOrigin::Call { to, key, deadline },
                };
                work.push(Work::Core(core::Event::EffectStart { owner, connector: number, origin }));
            }
            Work::Timer(timer) => {
                let requests =
                    core::fire(&mut domain.core, &Env { now: env.now, wall: env.wall, limits: env.limits.core }, timer);
                route_core(domain, env, &mut decision, requests, &mut work);
            }
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
            Work::WriterRead { connector, resource, event } => {
                if let connector::SystemEvent::Fact { resource: observed, fact, .. } = &event
                    && !fact.pending
                    && observed.segments() == resource.path.as_ref()
                {
                    work.push(Work::Connector { number: connector, event: connector::Event::System(event) });
                    work.push(Work::Core(core::Event::Tasks(tasks::Event::ReadAfresh { resource })));
                }
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
            Work::ConnectorResume { number } => {
                let mut requests = Queue::with_capacity(connector::MAX_OUT);
                connector::resume(numbered(domain, number), &mut requests);
                route_connector(domain, env, &mut decision, number, &mut requests, &mut work);
            }
            Work::Turn { channel, task, attempt, turn, cumulative, read, transcript } => {
                let body = payload(domain, Payload::Turn { task, attempt, turn, cumulative, read, transcript });
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
    if domain.decision_wrote || domain.core.counters.dirty() {
        let header = domain.core.counters.next_commit();
        write(
            domain,
            &mut decision,
            Write::Save(Record::Core(core::Record::Core(core::CoreRecord::Deployment(header)))),
        );
    }
    if domain.core.restart_failure().is_none() {
        domain.journal.accept(decision);
    }
}

fn effect_payload(domain: &mut Domain, effect: connector::Effect) -> Token {
    let token = Token::new(domain.next_payload);
    domain.next_payload = domain.next_payload.checked_add(1).expect("bounded owner numbers");
    assert!(domain.effects.insert(token, effect).is_ok(), "admitted effect room");
    token
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
    if let Some(step) = domain.restart_load.take() {
        out.push(Request::Restart(step));
        return;
    }
    if domain.journal.stopped() || domain.core.restart_failure().is_some() {
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
            Delivery::Core(core::Held::MakeEffect { connector, entry }) => {
                step(domain, env, Event::Connector { number: connector, event: connector::Event::Make { entry } });
            }
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
                let event = relay_message(domain, task, attempt, previous, word);
                step(domain, env, event);
            }
            other @ (Delivery::Message { .. }
            | Delivery::Restart(_)
            | Delivery::CallAnswer { .. }
            | Delivery::Core(_)
            | Delivery::Assigned { .. }
            | Delivery::System { .. }
            | Delivery::LostRead { .. }
            | Delivery::Procedure { .. }) => out.push(Request::Deliver(other)),
        }
    }
    for _ in 0..domain.now.len() {
        let now = domain.now.pop().expect("bounded now count");
        out.push(Request::Now(now));
    }
    if resume_restart_outbox(domain, env) {
        return;
    }
    if domain.journal.idle() && (!domain.restart_active || domain.core.restart_ready()) {
        if let Some(number) = closing_connector(domain) {
            decide(domain, env, Work::ConnectorResume { number });
            return;
        }
        if domain.core.accounts.usable(domain.core.settings.account)
            && let Some(context) = domain.core.due.pop()
        {
            decide(domain, env, Work::Core(core::Event::Activate { context, ready: true }));
            return;
        }
        decide(domain, env, Work::ResumeFleet);
    }
}

fn closing_connector(domain: &Domain) -> Option<u16> {
    if connector::closing_ready(&domain.first) {
        Some(1)
    } else if connector::closing_ready(&domain.second) {
        Some(2)
    } else {
        None
    }
}

fn resume_restart_outbox(domain: &mut Domain, env: &Env<Limits>) -> bool {
    if !domain.journal.idle() {
        return false;
    }
    let number = match domain.core.restart_step() {
        Some(core::RestartStep::SettleOutbox { connector }) => connector,
        Some(
            core::RestartStep::LoadCore
            | core::RestartStep::RestoreConnector { .. }
            | core::RestartStep::AdoptRuns
            | core::RestartStep::ReadAfresh { .. }
            | core::RestartStep::Open,
        )
        | None => return false,
    };
    decide(
        domain,
        env,
        Work::Connector { number, event: connector::Event::Restart(connector::RestartStep::SettleOutbox) },
    );
    true
}

fn write(domain: &mut Domain, decision: &mut Decision<Write, Delivery>, value: Write) {
    domain.decision_wrote = true;
    assert!(decision.write(value).is_ok(), "reserved write room");
}

fn hold(decision: &mut Decision<Write, Delivery>, value: Delivery) {
    assert!(decision.hold(value).is_ok(), "reserved delivery room");
}

// The translations below are deliberately exhaustive: a new boundary variant
// must make the testing application say how it crosses its root.
mod routes;
use routes::{route_connector, route_core};

/// Translate one durable row during the requested restart load. The root
/// supplies header, children, proofs, then calls; the core validates their links.
#[must_use]
pub fn restore_record(domain: &mut Domain, env: &Env<Limits>, record: Record) -> bool {
    match record {
        Record::Core(core::Record::Core(row)) => match domain.core.restore_core(row, &env.limits.core) {
            core::Restored::Deployment { commits } => {
                domain.journal = Journal::from_durable(&env.limits.journal, commits);
                true
            }
            core::Restored::Live | core::Restored::Archive => true,
            core::Restored::Rejected => false,
        },
        Record::Core(core::Record::Tasks(row)) => {
            if !domain.core.restore_task_row(&row) {
                return false;
            }
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.core.tasks));
            tasks::step(
                &mut domain.core.tasks,
                &Env { now: env.now, wall: env.wall, limits: env.limits.core.tasks },
                tasks::Event::Restore { record: row },
                &mut out,
            );
            out.is_empty()
        }
        Record::Core(core::Record::People(row)) => {
            let mut out = Queue::with_capacity(jig_core_people::max_out(&env.limits.core.people));
            jig_core_people::step(
                &mut domain.core.people,
                &Env { now: env.now, wall: env.wall, limits: env.limits.core.people },
                jig_core_people::Event::Restore { record: row },
                &mut out,
            );
            out.is_empty()
        }
        Record::Core(core::Record::Notes(_)) => true,
        Record::Connector { number, record } => {
            match &record {
                connector::Record::Outbox(row) => {
                    if domain.outbox_tasks.insert(row.number, row.task).is_err() {
                        return false;
                    }
                    // The application encodes procedure purposes with attempt zero.
                    if row.key.attempt == 0 && domain.outbox_procedures.insert(row.number, number).is_err() {
                        return false;
                    }
                }
                connector::Record::Proposal { .. }
                | connector::Record::Procedure(_)
                | connector::Record::Result { .. }
                | connector::Record::Task { .. }
                | connector::Record::Adoption { .. }
                | connector::Record::Subscription { .. }
                | connector::Record::Pool { .. }
                | connector::Record::Made { .. } => {}
            }
            let mut out = Queue::with_capacity(connector::MAX_OUT);
            connector::step(
                numbered(domain, number),
                &Env { now: env.now, wall: env.wall, limits: env.limits.connector },
                connector::Event::Restore { record },
                &mut out,
            );
            true
        }
    }
}

fn relay_message(domain: &mut Domain, task: u64, attempt: u64, previous: Option<u64>, word: tasks::Word) -> Event {
    let name = Token::new(word.number);
    let body = payload(domain, Payload::InboxWord(word));
    assert!(
        domain.core.relaying.replace(core::PendingRelay { previous, message: name.raw() }).is_none(),
        "one committed relay in flight"
    );
    Event::Core(core::Event::Fleet(fleet::Event::Inbound {
        run: Token::new(task),
        attempt: Token::new(attempt),
        message: fleet::Message { name, sender: body, words: body },
    }))
}
