//! The testing application adapter (`domain/testing.md`, 5). Its peers report
//! outside observations; the conformance harness owns the engine and store.
#![forbid(unsafe_code)]
use jig_conformance::scenarios::Scenario;
use jig_conformance::{Application, Clock, Input, Output};
use jig_core as core;
use jig_core_accounts as accounts;
use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_world::{Store, observations::Observer, peers::Peers};
use jig_fake_parties::{Act, Ask, Party, Role};
use jig_fake_workers::{Script, Worker};
use jig_test_connector as connector;
use jig_test_domain as root;
use jig_test_system::System;
use skein_lib::{Duration, Env, List, Queue, Time, Wall};
use std::collections::BTreeMap;

/// Tiny limits and seeded vocabulary, reconstructed on every cold start.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub seed: u64,
    pub engine: bool,
    pub workers: u32,
    pub scenario: Option<Scenario>,
    limits: root::Limits,
    fault: negative::Fault,
}

impl Config {
    /// Construct seeded vocabulary and retain its limits for each iteration.
    #[must_use]
    pub fn new(seed: u64, engine: bool, workers: u32, scenario: Option<Scenario>) -> Self {
        let mut config = Self {
            seed,
            engine,
            workers,
            scenario,
            limits: jig_core_world::peers::fixture(seed, u32::from(engine)).1,
            fault: negative::Fault::None,
        };
        config.limits = config.root().1;
        config
    }

    /// Choose one deliberate boundary corruption for a referee sensitivity story.
    #[must_use]
    pub fn broken(mut self, fault: negative::Fault) -> Self {
        self.fault = fault;
        self
    }

    fn root(self) -> (root::Config, root::Limits) {
        let (mut config, mut limits) = jig_core_world::peers::fixture(self.seed, u32::from(self.engine));
        if self.scenario == Some(Scenario::ShrinkingPool) {
            limits.core.fleet.slots = 3;
            limits.core.tasks.inbox_bytes = 512;
            limits.core.tasks.inbox_messages = 8;
            config.core.settings.chat_authority.budget.spend = 80;
        }
        if self.scenario == Some(Scenario::StandingTask) {
            limits.core.tasks.tasks = 8;
            limits.core.tasks.project_tasks = 8;
        }
        if self.scenario == Some(Scenario::OtherHand) {
            for kind in &mut config.first.kinds {
                if kind.kind == 5 {
                    kind.recovery = connector::Recovery::Conditional;
                }
            }
        }
        if matches!(self.scenario, Some(Scenario::ChangedJudge | Scenario::NarrowingPolicy)) {
            let rules = config.core.authority.rules().clone();
            let mut policy = config.core.authority.policy(1).expect("scenario policy").clone();
            policy.requirements = Box::new([authority::Requirement {
                connector: 1,
                kind: 5,
                pattern: authority::Pattern { segments: Box::new([]), last: authority::Last::Open(Box::new([])) },
                judge: authority::Judge { connector: 2, requirement: 9, parameters: 0 },
                guard: authority::Guard::Observed { freshness: Duration::from_secs(60) },
                must_be_guarded: false,
            }]);
            let mut checked = authority::Domain::new(rules, limits.core.authority).expect("scenario authority fits");
            let mut out = Queue::with_capacity(authority::POLICY_MAX_OUT);
            authority::step(&mut checked, authority::Event::Policy { project: 1, policy }, &mut out);
            assert_eq!(out.pop(), Some(authority::PolicyFact::Added { project: 1 }));
            config.core.authority = checked;
        }
        (config, limits)
    }
}

/// The testing engine; the trait is implemented only in ordinary world code.
#[derive(Debug)]
pub struct Testing;

/// A released root output, including immediate observations and cold loads.
#[derive(Debug)]
pub enum Delivery {
    /// A journal-held output, including a restart script instruction.
    Held(root::Delivery),
    /// A root observation that decides nothing.
    Now(core::Now),
}

/// The outside correlation state of the application's scripted peers.
#[derive(Debug)]
pub struct Neighbours {
    pub peers: Peers,
    observer: Observer,
    restart_reads: BTreeMap<u16, core::RestartStep>,
    restoring: Option<Restore>,
    account_ready: bool,
    pub assignments: Vec<(u64, u64)>,
    pub answers: u64,
    pub restart_steps: Vec<core::RestartStep>,
    pub calls: Vec<core::CallPart>,
    pub hold_writes: bool,
    pub pending_writes: Vec<(u16, connector::SystemRequest)>,
    pub next_fault: jig_test_system::Fault,
    pub scenario: Option<Scenario>,
    pub children: Vec<u64>,
    pub initial_assignments: usize,
    pub pending_proposal: Option<u64>,
    pub saved_deadline: Option<Wall>,
    fault: negative::Fault,
    routes: Vec<jig_conformance::referee::Observed>,
}

#[derive(Debug)]
struct Restore {
    step: core::RestartStep,
    first: root::Key,
    last: root::Key,
    rows: Vec<root::Record>,
}

fn env(config: &Config, clock: Clock) -> Env<root::Limits> {
    Env { now: Time::from_nanos(clock.now), wall: Wall::from_nanos(clock.now), limits: config.limits }
}

impl Neighbours {
    fn restored(step: core::RestartStep, rows: &[root::Record]) -> Vec<Input<root::Event, root::Record>> {
        let mut inputs = Vec::new();
        for group in 0..4 {
            for row in rows {
                let rank = match row {
                    root::Record::Core(core::Record::Core(core::CoreRecord::Deployment(_))) => 0,
                    root::Record::Core(core::Record::Core(core::CoreRecord::RunProof(_))) => 2,
                    root::Record::Core(core::Record::Core(core::CoreRecord::Call(_))) => 3,
                    root::Record::Core(
                        core::Record::Core(
                            core::CoreRecord::Turn(_)
                            | core::CoreRecord::Terminal(_)
                            | core::CoreRecord::ProposalDecision(_)
                            | core::CoreRecord::EscalationDecision(_),
                        )
                        | core::Record::Tasks(tasks::Stored::History(_) | tasks::Stored::Ended(_)),
                    ) => continue,
                    root::Record::Core(core::Record::Tasks(_) | core::Record::People(_) | core::Record::Notes(_))
                    | root::Record::Connector { .. } => 1,
                };
                if rank == group {
                    inputs.push(Input::Restore(row.clone()));
                }
            }
        }
        if step == core::RestartStep::LoadCore {
            inputs.push(Input::Event(root::Event::Core(core::Event::People(people::Event::Restored))));
        }
        inputs.push(Input::Event(root::Event::RestartDone(step)));
        inputs
    }

    fn restart(&mut self, store: &mut Store, step: core::RestartStep) -> Vec<Input<root::Event, root::Record>> {
        self.restart_steps.push(step);
        match step {
            core::RestartStep::LoadCore | core::RestartStep::RestoreConnector { .. } => {
                let keys: Vec<_> = store
                    .rows
                    .keys()
                    .filter(|key| match (step, key) {
                        (core::RestartStep::LoadCore, root::Key::Core(_)) => true,
                        (core::RestartStep::RestoreConnector { connector }, root::Key::Connector { number, .. }) => {
                            connector == *number
                        }
                        _ => false,
                    })
                    .cloned()
                    .collect();
                if let (Some(first), Some(last)) = (keys.first(), keys.last()) {
                    store.load(1, first, last, None, 2);
                    self.restoring = Some(Restore { step, first: first.clone(), last: last.clone(), rows: Vec::new() });
                    Vec::new()
                } else {
                    Self::restored(step, &[])
                }
            }
            core::RestartStep::ReadAfresh { connector: number } => {
                self.restart_reads.insert(number, step);
                vec![Input::Event(root::Event::Connector {
                    number,
                    event: connector::Event::Restart(connector::RestartStep::FreshRead {
                        resource: jig_test_connector_world::path(1, 1),
                    }),
                })]
            }
            core::RestartStep::SettleOutbox { connector: number } => vec![Input::Event(root::Event::Connector {
                number,
                event: connector::Event::Restart(connector::RestartStep::SettleOutbox),
            })],
            core::RestartStep::AdoptRuns | core::RestartStep::Open => panic!("root performs synchronous restart step"),
        }
    }
}

impl Application for Testing {
    type Domain = root::Domain;
    type Config = Config;
    type Systems = [System; 2];
    type Peers = Neighbours;
    type Key = root::Key;
    type Record = root::Record;
    type Event = root::Event;
    type Delivery = Delivery;

    fn build(config: &Config) -> root::Domain {
        let (configuration, limits) = config.root();
        let mut domain = root::Domain::new(configuration, &limits);
        let env = env(config, Clock { now: 0 });
        domain.configure_holds(
            &env,
            1,
            Box::new([
                tasks::Kind { connector: 1, kind: 1, hold: tasks::HoldKind::Exclusive { taken: tasks::Taken::Waits } },
                tasks::Kind { connector: 1, kind: 2, hold: tasks::HoldKind::Pooled { taken: tasks::Taken::Waits } },
            ]),
        );
        domain
    }

    fn systems(config: &Config, _: u64) -> [System; 2] {
        let mut systems = [System::new(), System::new()];
        if matches!(config.scenario, Some(Scenario::ChangedJudge | Scenario::NarrowingPolicy)) {
            systems[1].other_hand(&jig_test_connector_world::path(1, 1), 13);
        }
        systems
    }

    fn peers(config: &Config, _: u64) -> Neighbours {
        let script = || match config.scenario {
            Some(Scenario::ShrinkingPool) => {
                Box::new([Script::Wait, Script::Finish { report: b"pool done".as_slice().into() }]) as Box<[Script]>
            }
            Some(Scenario::StandingTask) => {
                Box::new([Script::Wait, Script::Finish { report: b"period done".as_slice().into() }]) as Box<[Script]>
            }
            Some(Scenario::RunBudget) => {
                let mut script = vec![Script::Wait];
                for session in 0..8 {
                    script.push(Script::Turn {
                        body: format!("session {session}").into_bytes().into_boxed_slice(),
                        cost: 2,
                        read: None,
                    });
                }
                for child in 0..4 {
                    script.push(Script::Turn {
                        body: format!("sub-agent {child}").into_bytes().into_boxed_slice(),
                        cost: 1,
                        read: None,
                    });
                }
                script.push(Script::Park);
                script.into_boxed_slice()
            }
            None
            | Some(
                Scenario::LateCopy
                | Scenario::OtherHand
                | Scenario::UncertainRestarts
                | Scenario::ChangedJudge
                | Scenario::NarrowingPolicy,
            ) => Box::new([
                Script::Wait,
                Script::Turn { body: b"whole turn".as_slice().into(), cost: 2, read: None },
                Script::Park,
            ]),
        };
        let mut workers: Vec<_> = (0..config.workers)
            .map(|index| Worker::new(7 + u64::from(index), 1, (0..8).map(|_| script()).collect()))
            .collect();
        if config.engine {
            workers.insert(0, Worker::new(0, 1, (0..8).map(|_| script()).collect()));
        }
        let mut observer = Observer::recording(&config.root().0);
        if config.scenario == Some(Scenario::OtherHand) {
            observer
                .referee
                .policy
                .participating
                .insert(jig_conformance::referee::Name { connector: 1, path: vec![vec![1], vec![1]] });
        }
        Neighbours {
            peers: Peers::new(
                workers,
                vec![Party::new(
                    1,
                    Role::Owner,
                    10_000,
                    Box::new([
                        Act::SignIn { provider: 0, subject: 7_u64.to_be_bytes().into() },
                        Act::Request { key: [1; 16], ask: Ask::Chat { words: b"work".as_slice().into() } },
                    ]),
                )],
            ),
            observer,
            restart_reads: BTreeMap::new(),
            restoring: None,
            account_ready: false,
            assignments: Vec::new(),
            answers: 0,
            restart_steps: Vec::new(),
            calls: Vec::new(),
            hold_writes: false,
            pending_writes: Vec::new(),
            next_fault: jig_test_system::Fault::None,
            scenario: config.scenario,
            children: Vec::new(),
            initial_assignments: 0,
            pending_proposal: None,
            saved_deadline: None,
            fault: config.fault,
            routes: Vec::new(),
        }
    }

    fn policy(peers: &Neighbours) -> jig_conformance::referee::Policy {
        peers.observer.referee.policy.clone()
    }

    fn recovery(config: &Config, number: u16, kind: u16) -> jig_conformance::referee::Recovery {
        let configuration = config.root().0;
        let connector = if number == 1 { configuration.first } else { configuration.second };
        match connector.kinds.iter().find(|entry| entry.kind == kind).expect("configured kind").recovery {
            connector::Recovery::Keyed => jig_conformance::referee::Recovery::Keyed,
            connector::Recovery::Conditional => jig_conformance::referee::Recovery::Conditional,
            connector::Recovery::Idempotent => jig_conformance::referee::Recovery::Idempotent,
            connector::Recovery::Unrecoverable => jig_conformance::referee::Recovery::Unrecoverable,
        }
    }

    fn start(_: &Config, store: &mut Store) -> Vec<root::Event> {
        if store.rows.is_empty() {
            let row =
                root::Record::Core(core::Record::People(people::Stored::Roles { project: 1, holdings: Box::new([]) }));
            store.rows.insert(row.key(), row);
        }
        vec![root::Event::RestartBegin]
    }

    fn cold(peers: &mut Neighbours, store: &Store, clock: Clock) {
        peers.observer.cold(clock.now, store);
        peers.peers.restart();
        peers.restoring = None;
        peers.restart_reads.clear();
        peers.account_ready = false;
        peers.pending_writes.clear();
    }

    fn timers(_: &Config) -> Vec<root::Event> {
        vec![
            root::Event::Timer(core::Timer::Fleet),
            root::Event::Timer(core::Timer::Tasks),
            root::Event::ConnectorTimer { number: 1 },
            root::Event::ConnectorTimer { number: 2 },
        ]
    }

    fn step(domain: &mut root::Domain, config: &Config, clock: Clock, input: Input<root::Event, root::Record>) {
        let env = env(config, clock);
        let input = negative::input(domain, config, clock, input);
        match input {
            Input::Event(event) => root::step(domain, &env, event),
            Input::Committed(number) => root::step(domain, &env, root::Event::Committed { number }),
            Input::Failed(number) => root::step(domain, &env, root::Event::Failed { number }),
            Input::Restore(row) => assert!(root::restore_record(domain, &env, row), "row accepted by cold script"),
        }
    }

    fn release(
        domain: &mut root::Domain,
        config: &Config,
        clock: Clock,
    ) -> Vec<Output<root::Key, root::Record, Delivery>> {
        let mut queue = Queue::with_capacity(128);
        root::release(domain, &env(config, clock), &mut queue);
        let mut outputs = Vec::new();
        while let Some(request) = queue.pop() {
            outputs.push(match request {
                root::Request::Commit { number, mut writes } => {
                    let mut rows = Vec::new();
                    while let Some(write) = writes.pop() {
                        rows.push(write);
                    }
                    Output::Commit { number, writes: jig_core_world::store_writes(rows) }
                }
                root::Request::Restart(step) => Output::Deliver(Delivery::Held(root::Delivery::Restart(step))),
                root::Request::Deliver(delivery) => Output::Deliver(Delivery::Held(delivery)),
                root::Request::Now(now) => Output::Deliver(Delivery::Now(now)),
                root::Request::Stop => Output::Stop,
            });
        }
        negative::released(domain, config, clock, outputs)
    }

    fn inbound(peers: &mut Neighbours, store: &Store, clock: Clock, event: &mut root::Event) {
        peers.observer.inbound(store, clock.now, event);
    }

    #[expect(clippy::too_many_lines, reason = "the root delivery vocabulary has one exhaustive outside translation")]
    fn deliver(
        peers: &mut Neighbours,
        systems: &mut [System; 2],
        store: &mut Store,
        clock: Clock,
        delivery: Delivery,
    ) -> Vec<Input<root::Event, root::Record>> {
        let delivery = negative::delivery(peers.fault, delivery);
        let Delivery::Held(mut delivery) = delivery else {
            return match delivery {
                Delivery::Now(core::Now::Call { to, run, attempt, call }) => {
                    peers.peers.opaque_answer(to, run, attempt, &call).into_iter().map(Input::Event).collect()
                }
                Delivery::Now(core::Now::Account(accounts::Request::Refresh { account, generation })) => {
                    vec![Input::Event(root::Event::Core(core::Event::Account(accounts::Event::Refreshed {
                        account,
                        generation,
                        valid: Duration::from_secs(60),
                    })))]
                }
                Delivery::Now(core::Now::StartPreparation { task, .. }) => {
                    let rows = store
                        .rows
                        .values()
                        .filter_map(|record| match record {
                            root::Record::Core(core::Record::Core(core::CoreRecord::Turn(turn)))
                                if turn.task == task =>
                            {
                                Some(turn.clone())
                            }
                            root::Record::Core(_) | root::Record::Connector { .. } => None,
                        })
                        .collect();
                    vec![Input::Event(root::Event::TranscriptLoaded { task, rows, done: true })]
                }
                Delivery::Now(_) => Vec::new(),
                Delivery::Held(_) => unreachable!("held handled above"),
            };
        };
        peers.observer.delivered(clock.now, &mut delivery);
        peers.peers.delivered(&delivery);
        match delivery {
            root::Delivery::Restart(step) => {
                let position = match step {
                    core::RestartStep::LoadCore => 0,
                    core::RestartStep::RestoreConnector { connector } => u32::from(connector),
                    core::RestartStep::ReadAfresh { connector } => 2 + u32::from(connector),
                    core::RestartStep::AdoptRuns | core::RestartStep::SettleOutbox { .. } | core::RestartStep::Open => {
                        panic!("root performs internal restart phases")
                    }
                };
                peers.routes.push(jig_conformance::referee::Observed::RestartStage { position });
                peers.restart(store, step)
            }
            root::Delivery::Assigned { assignment, .. } => {
                peers.assignments.push((assignment.task, assignment.attempt));
                Vec::new()
            }
            root::Delivery::Core(core::Held::PeopleReply { to, sign_in, reply }) => {
                peers.answers += 1;
                peers.peers.reply(to, sign_in, reply);
                Vec::new()
            }
            root::Delivery::System { connector: number, call } => {
                if peers.fault == negative::Fault::RepeatedEffect {
                    negative::repeat(&mut systems[usize::from(number - 1)], &call);
                }
                let mut fault = jig_test_system::Fault::None;
                if matches!(call, connector::SystemRequest::Apply { .. }) {
                    if peers.hold_writes {
                        peers.pending_writes.push((number, call));
                        return Vec::new();
                    }
                    fault = peers.next_fault;
                    peers.next_fault = jig_test_system::Fault::None;
                }
                let event = systems[usize::from(number - 1)].answer(call, fault);
                let mut events =
                    vec![Input::Event(root::Event::Connector { number, event: connector::Event::System(event) })];
                if let Some(step) = peers.restart_reads.remove(&number) {
                    events.push(Input::Event(root::Event::RestartDone(step)));
                }
                events
            }
            root::Delivery::LostRead { connector, resource, call, .. } => vec![Input::Event(root::Event::WriterRead {
                connector,
                resource,
                event: systems[usize::from(connector - 1)].answer(call, jig_test_system::Fault::None),
            })],
            root::Delivery::Procedure { task, connector: number, code, .. } => {
                vec![Input::Event(root::Event::Connector {
                    number,
                    event: connector::Event::Procedure {
                        task,
                        number: code,
                        resource: jig_test_connector_world::path(1, 1),
                        signal: connector::ProcedureSignal::Activate,
                    },
                })]
            }
            root::Delivery::Core(core::Held::NotesLoad { owner, range }) => {
                let mut records = List::with_capacity(jig_core_world::world::limits().core.notes.load_rows);
                let mut more = false;
                for row in store.rows.values() {
                    let root::Record::Core(core::Record::Notes(record)) = row else { continue };
                    let selected = match (&range, record) {
                        (jig_core_notes::Range::Entry { name }, jig_core_notes::Record::Entry(entry)) => {
                            *name == entry.name
                        }
                        (jig_core_notes::Range::Lines { scope, after }, jig_core_notes::Record::Line(line)) => {
                            *scope == line.scope && after.is_none_or(|previous| line.name > previous)
                        }
                        _ => false,
                    };
                    if selected && records.push(record.clone()).is_err() {
                        more = true;
                    }
                }
                vec![Input::Event(root::Event::Core(core::Event::Notes(jig_core_notes::Event::Loaded {
                    owner,
                    rows: jig_core_notes::Rows { records },
                    more,
                })))]
            }
            root::Delivery::Core(core::Held::CallAnswer { part, .. }) => {
                peers.calls.push(part);
                Vec::new()
            }
            root::Delivery::Core(_) | root::Delivery::CallAnswer { .. } | root::Delivery::Message { .. } => Vec::new(),
            root::Delivery::Fleet(_) => panic!("root consumes fleet continuations"),
        }
    }

    fn poll(
        peers: &mut Neighbours,
        _: &mut [System; 2],
        store: &mut Store,
        clock: Clock,
        ready: bool,
    ) -> Vec<Input<root::Event, root::Record>> {
        let mut events = Vec::new();
        for (owner, page) in store.tick_pages() {
            assert_eq!(owner, 1);
            let mut restore = peers.restoring.take().expect("cold page requested");
            restore.rows.extend(page.rows);
            if let Some(after) = page.next {
                store.load(1, &restore.first, &restore.last, Some(&after), 2);
                peers.restoring = Some(restore);
            } else {
                events.extend(Neighbours::restored(restore.step, &restore.rows));
            }
        }
        if ready {
            if !peers.account_ready {
                peers.account_ready = true;
                events.push(Input::Event(root::Event::Core(core::Event::Account(accounts::Event::Add {
                    account: 1,
                    generation: 1,
                    valid: Some(Duration::from_secs(60)),
                }))));
            }
            events.extend(peers.peers.tick(store, Time::from_nanos(clock.now)).into_iter().map(Input::Event));
        }
        events
    }

    fn observed(
        peers: &mut Neighbours,
        systems: &[System; 2],
        store: &Store,
        clock: Clock,
    ) -> Vec<jig_conformance::referee::Observed> {
        peers.observer.durable(clock.now, store);
        peers.observer.systems(clock.now, systems);
        peers.observer.workers(clock.now, &peers.peers.workers);
        let mut observed = std::mem::take(&mut peers.routes);
        observed.extend(peers.observer.take());
        observed
    }

    fn ready(domain: &root::Domain) -> bool {
        domain.ready()
    }
    fn quiescent(domain: &root::Domain) -> bool {
        domain.quiescent()
    }
    fn reclaim(domain: &mut root::Domain) {
        domain.reclaim();
    }
    fn maximum(config: &Config) -> u64 {
        jig_core_world::observations::root_bound(&config.limits).expect("root bound fits")
    }
    fn configuration_heap(config: &Config) -> u64 {
        let limits = config.limits;
        core::worst_case(&limits.core).expect("core bound fits")
            + connector::worst_case(&limits.connector).expect("connector bound fits") * 2
    }
}

pub mod actions;
pub mod scenarios;

pub mod negative;
