//! Ops's two proof-of-concept stories on jig's independent conformance loop.
mod config;
mod observations;
mod scripts;
mod systems;

use jig_conformance::referee as r;
use jig_conformance::{Application, Clock, Input, Output};
use jig_core as core;
use jig_core_accounts as accounts;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_ops_domain as root;
use jig_ops_domain_infrastructure as infra;
use jig_ops_domain_observability as obs;
use jig_ops_fake_production as production;
use observations::Store;
use skein_lib::{Duration, Env, List, Queue, ReplyTo, Time, Token, Wall};
pub use systems::Systems;

/// Only these two stories belong to this proof of concept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Story {
    Alert,
    Night,
}

/// The same seed and story reconstruct every cold domain.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub seed: u64,
    pub story: Story,
    limits: root::Limits,
}

impl Config {
    /// Fix the finite limits once; cold starts still rebuild the same root.
    #[must_use]
    pub fn new(seed: u64, story: Story) -> Self {
        Self { seed, story, limits: config::root(seed).1 }
    }
}

/// Outside party state and evidence survive domain crashes.
pub struct Peers {
    policy: r::Policy,
    applied: u64,
    observed: Vec<r::Observed>,
    pub sign_in: Option<u64>,
    account_ready: bool,
    asked_sign_in: bool,
    pub results: Vec<u64>,
    delivered_results: std::collections::BTreeSet<u64>,
    pending_results: Vec<(u64, u64, u64)>,
    pub assignments: Vec<(u64, u64)>,
    pub entries: Vec<(u64, u16)>,
    pub accepted: Vec<u64>,
    pub replies: u64,
    llm: scripts::Llm,
    asked_accept: std::collections::BTreeSet<(u64, u64)>,
}

/// Released root outputs and receipts observed in a whole committed transaction.
#[derive(Debug)]
pub enum Delivery {
    Root(Box<root::Output>),
    Results { commit: u64, rows: Vec<(u64, u64)> },
}

/// The real ops root and its independent production; no surrogate engine.
pub struct Ops;

fn env(config: &Config, clock: Clock) -> Env<root::Limits> {
    Env { now: Time::from_nanos(clock.now), wall: Wall::from_nanos(clock.now), limits: config.limits }
}

impl Application for Ops {
    type Domain = root::Domain;
    type Config = Config;
    type Systems = Systems;
    type Peers = Peers;
    type Key = root::Key;
    type Record = root::Record;
    type Event = root::Event;
    type Delivery = Delivery;

    fn build(config: &Config) -> root::Domain {
        let (configuration, limits) = config::root(config.seed);
        root::Domain::new(configuration, &limits)
    }
    fn systems(_: &Config, _: u64) -> Systems {
        let mut production = production::Production::new(0, production::Backend::Fake);
        production.add_service(production::ServiceName::new("production", "checkout"), "v2", 3);
        production.add_service(production::ServiceName::new("staging", "checkout"), "v2", 3);
        Systems { production, now: 0 }
    }
    fn peers(config: &Config, _: u64) -> Peers {
        Peers {
            policy: observations::policy(config.seed),
            applied: 0,
            observed: Vec::new(),
            sign_in: None,
            account_ready: false,
            asked_sign_in: false,
            results: Vec::new(),
            delivered_results: std::collections::BTreeSet::new(),
            pending_results: Vec::new(),
            assignments: Vec::new(),
            entries: Vec::new(),
            accepted: Vec::new(),
            replies: 0,
            llm: scripts::Llm::new(config.seed),
            asked_accept: std::collections::BTreeSet::new(),
        }
    }
    fn policy(peers: &Peers) -> &r::Policy {
        &peers.policy
    }
    fn recovery(_: &Config, connector: u16, kind: u16) -> r::Recovery {
        match (connector, kind) {
            (2, 1) => r::Recovery::Keyed,
            (2, 2) => r::Recovery::Conditional,
            _ => panic!("declared story kinds"),
        }
    }
    fn start(_: &Config, store: &mut Store) -> Vec<root::Event> {
        if store.rows.is_empty() {
            let record =
                root::Record::Core(core::Record::People(people::Stored::Roles { project: 1, holdings: Box::new([]) }));
            store.rows.insert(record.key(), record);
        }
        vec![root::Event::Restart]
    }
    fn cold(peers: &mut Peers, store: &Store, _: Clock) {
        peers.observed.push(r::Observed::ColdStore { receipts: observations::snapshot(store).receipts });
        peers.account_ready = false;
        peers.asked_accept.clear();
    }
    fn timers(_: &Config) -> Vec<root::Event> {
        vec![
            root::Event::Timer(core::Timer::Tasks),
            root::Event::Timer(core::Timer::Fleet),
            root::Event::Timer(core::Timer::Account),
            root::Event::Timer(core::Timer::Brief),
            root::Event::ObservabilityTimer,
            root::Event::InfrastructureTimer,
            root::Event::HostTimer,
        ]
    }
    fn step(domain: &mut root::Domain, config: &Config, clock: Clock, input: Input<root::Event, root::Record>) {
        let event = match input {
            Input::Event(event) => event,
            Input::Committed(number) => root::Event::Store(root::Store::Committed { number }),
            Input::Failed(number) => root::Event::Store(root::Store::Failed { number }),
            Input::Restore(row) => root::Event::Store(root::Store::Restore(row)),
        };
        root::step(domain, &env(config, clock), event);
    }
    fn release(
        domain: &mut root::Domain,
        config: &Config,
        clock: Clock,
    ) -> Vec<Output<root::Key, root::Record, Delivery>> {
        let mut queue = Queue::with_capacity(256);
        root::release(domain, &env(config, clock), &mut queue);
        let mut outputs = Vec::new();
        while let Some(output) = queue.pop() {
            match output {
                root::Output::Commit { number, mut writes } => {
                    let mut rows = Vec::new();
                    let mut results = Vec::new();
                    while let Some(write) = writes.pop() {
                        rows.push(match write {
                            root::Write::Save(row) => {
                                if let root::Record::Core(core::Record::Tasks(tasks::Stored::Live(parent))) = &row {
                                    for word in &parent.inbox {
                                        if let (tasks::MessageKind::Result(_), tasks::Party::Task(child)) =
                                            (&word.kind, word.from)
                                        {
                                            results.push((child, parent.number));
                                        }
                                    }
                                }
                                jig_fake_store::Write::Save { key: row.key(), row }
                            }
                            root::Write::Erase(key) => jig_fake_store::Write::Erase(key),
                        });
                    }
                    outputs.push(Output::Commit { number, writes: rows.into_boxed_slice() });
                    if !results.is_empty() {
                        outputs.push(Output::Deliver(Delivery::Results { commit: number, rows: results }));
                    }
                }
                root::Output::Stop => outputs.push(Output::Stop),
                other @ (root::Output::Load { .. }
                | root::Output::CoreLoad(_)
                | root::Output::Core(_)
                | root::Output::Now(_)
                | root::Output::Observability(_)
                | root::Output::Infrastructure(_)
                | root::Output::Read { .. }
                | root::Output::Llm { .. }
                | root::Output::ToChild(_)
                | root::Output::Busy) => outputs.push(Output::Deliver(Delivery::Root(Box::new(other)))),
            }
        }
        outputs
    }
    fn inbound(_: &mut Peers, _: &Store, _: Clock, _: &mut root::Event) {}

    #[expect(clippy::too_many_lines, reason = "each outside output has an exhaustive adapter")]
    fn deliver(
        peers: &mut Peers,
        systems: &mut Systems,
        store: &mut Store,
        clock: Clock,
        delivery: Delivery,
    ) -> Vec<Input<root::Event, root::Record>> {
        let delivery = match delivery {
            Delivery::Root(root) => *root,
            Delivery::Results { commit, rows } => {
                for (task, parent) in rows {
                    peers.pending_results.push((commit, task, parent));
                }
                return Vec::new();
            }
        };
        match delivery {
            root::Output::ToChild(child) => {
                if let root::Released::Host(jig_host::Event::Assign { assignment, .. }) = &child {
                    let task = assignment.assignment.run.raw();
                    let attempt = assignment.assignment.attempt.raw();
                    let budget = observations::snapshot(store).tasks[&task].run_reserved;
                    peers.assignments.push((task, attempt));
                    peers.observed.push(r::Observed::Assigned { task, attempt, budget });
                }
                vec![Input::Event(root::Event::Released(child))]
            }
            root::Output::Load { step, range } => {
                let position = match step {
                    core::RestartStep::LoadCore => 0,
                    core::RestartStep::RestoreConnector { connector } => u32::from(connector),
                    core::RestartStep::AdoptRuns
                    | core::RestartStep::ReadAfresh { .. }
                    | core::RestartStep::SettleOutbox { .. }
                    | core::RestartStep::Open => panic!("only row loads go to the store"),
                };
                peers.observed.push(r::Observed::RestartStage { position });
                let mut rows: Vec<_> = store
                    .rows
                    .values()
                    .filter(|row| match (range, row) {
                        (
                            root::Range::Core,
                            root::Record::Core(core::Record::Tasks(
                                tasks::Stored::Ended(_) | tasks::Stored::History(_) | tasks::Stored::Milestone(_),
                            )),
                        ) => false,
                        (root::Range::Core, root::Record::Core(_))
                        | (root::Range::Observability, root::Record::Observability(_))
                        | (root::Range::Infrastructure, root::Record::Infrastructure(_)) => true,
                        (root::Range::Core | root::Range::Observability | root::Range::Infrastructure, _) => false,
                    })
                    .cloned()
                    .collect();
                rows.sort_by_key(|row| match row {
                    root::Record::Core(core::Record::Core(core::CoreRecord::Deployment(_))) => 0,
                    root::Record::Core(core::Record::People(people::Stored::Person { .. })) => 1,
                    root::Record::Core(core::Record::People(_)) => 2,
                    root::Record::Core(core::Record::Tasks(_)) => 3,
                    root::Record::Core(_) => 4,
                    root::Record::Infrastructure(_) | root::Record::Observability(_) => 5,
                });
                let mut events: Vec<_> = rows.into_iter().map(Input::Restore).collect();
                events.push(Input::Event(root::Event::Store(root::Store::Restored(step))));
                events
            }
            root::Output::Infrastructure(call) => {
                if let infra::SystemRequest::Apply { key, attempt, effect } = &call {
                    let attempt = *attempt;
                    let stable = observations::key(*key);
                    let (kind, service, condition, target) = observations::parts(effect);
                    peers
                        .observed
                        .push(r::Observed::Released { requires: vec![r::Receipt::Outbox(2, stable, attempt)] });
                    let name = production::ServiceName::new(
                        std::str::from_utf8(&service.environment).expect("fixture bytes"),
                        std::str::from_utf8(&service.name).expect("fixture bytes"),
                    );
                    let before = match effect {
                        infra::Effect::Restart { operation, .. } => {
                            systems.production.find_restart(*operation).map(|_| *operation)
                        }
                        infra::Effect::Scale { .. } => {
                            Some(u64::from(systems.production.read_service(&name).expect("fixture service").replicas))
                        }
                        infra::Effect::Rollback { .. }
                        | infra::Effect::CreateEnvironment { .. }
                        | infra::Effect::TearDown { .. } => panic!("two story effects"),
                    };
                    let previous = systems.production.observed().len();
                    let event = systems.infrastructure(call);
                    let applied = match &systems.production.observed()[previous] {
                        production::ObservedEffect::Restart { applied, .. }
                        | production::ObservedEffect::Scale { applied, .. } => *applied,
                        production::ObservedEffect::Silence { .. }
                        | production::ObservedEffect::Rollback { .. }
                        | production::ObservedEffect::Create { .. }
                        | production::ObservedEffect::TearDown { .. } => panic!("two story effects"),
                    };
                    peers.observed.push(r::Observed::Effect {
                        connector: 2,
                        effect: jig_test_system::Observed {
                            kind,
                            key: stable,
                            resources: vec![jig_test_system::Name(observations::resource(&service, 2).path)],
                            condition,
                            target,
                            before: vec![jig_test_system::ValueObserved { state: before, owner: None }],
                            applied,
                            copy: if attempt == 1 {
                                jig_test_system::Copy::First
                            } else {
                                jig_test_system::Copy::Retried
                            },
                        },
                    });
                    return vec![Input::Event(root::Event::Infrastructure(infra::Event::System(event)))];
                }
                vec![Input::Event(root::Event::Infrastructure(infra::Event::System(systems.infrastructure(call))))]
            }
            root::Output::Observability(call) => {
                let event = systems.observability(call);
                if let obs::SystemEvent::Fact { service, fact } = &event {
                    let target = if service.environment.as_ref() == b"production" { 77 } else { 1 };
                    let met = if target == 77 {
                        fact.healthy_replicas >= 2
                    } else {
                        fact.load_percent < 20
                            && fact.load.first().is_some_and(|point| point.at <= fact.observed.saturating_sub(1800))
                            && fact.observed >= 1800
                            && fact.load.iter().all(|point| point.percent < 20)
                    };
                    peers.observed.push(r::Observed::Read {
                        resource: r::Name {
                            connector: 1,
                            path: vec![
                                b"env".to_vec(),
                                service.environment.to_vec(),
                                b"service".to_vec(),
                                service.name.to_vec(),
                            ],
                        },
                        state: met.then_some(target),
                        at: fact.observed * 1_000_000_000,
                    });
                }
                vec![Input::Event(root::Event::Observability(obs::Event::System(event)))]
            }
            root::Output::Now(core::Now::Account(accounts::Request::Refresh { account, generation })) => {
                vec![Input::Event(root::Event::Core(core::Event::Account(accounts::Event::Refreshed {
                    account,
                    generation,
                    valid: Duration::from_secs(7200),
                })))]
            }
            root::Output::Core(core::Held::PeopleReply { sign_in, reply, .. }) => {
                peers.replies += 1;
                match reply {
                    people::Reply::SignedIn { .. } => {
                        let number = sign_in.expect("saved session");
                        peers.sign_in = Some(number);
                        peers.observed.push(r::Observed::Released { requires: vec![r::Receipt::SignIn(number)] });
                    }
                    people::Reply::Outcome(_) => {}
                    other @ (people::Reply::SignedOut | people::Reply::Refused(_)) => {
                        panic!("unexpected party reply: {other:?}, {sign_in:?}")
                    }
                }
                Vec::new()
            }
            root::Output::Core(core::Held::Result { task, person, .. }) => {
                peers.results.push(task);
                peers.observed.push(r::Observed::Result { task, to: r::Source::Person(person) });
                peers.observed.push(r::Observed::Released { requires: vec![r::Receipt::Ended(task)] });
                Vec::new()
            }
            root::Output::Core(core::Held::NotesLoad { owner, .. }) => {
                vec![Input::Event(root::Event::Core(core::Event::Notes(jig_core_notes::Event::Loaded {
                    owner,
                    rows: jig_core_notes::Rows { records: List::with_capacity(0) },
                    more: false,
                })))]
            }
            root::Output::CoreLoad(core::Now::StartPreparation { task, .. }) => {
                let mut rows: Vec<_> = store
                    .rows
                    .values()
                    .filter_map(|record| match record {
                        root::Record::Core(core::Record::Core(core::CoreRecord::Turn(row))) if row.task == task => {
                            Some(row.clone())
                        }
                        root::Record::Core(_) | root::Record::Infrastructure(_) | root::Record::Observability(_) => {
                            None
                        }
                    })
                    .collect();
                rows.sort_by_key(|row| (row.attempt, row.turn));
                vec![Input::Event(root::Event::Store(root::Store::Transcript {
                    task,
                    rows: rows.into_boxed_slice(),
                    done: true,
                }))]
            }
            root::Output::Read { .. }
            | root::Output::Core(_)
            | root::Output::Now(_)
            | root::Output::CoreLoad(_)
            | root::Output::Busy => Vec::new(),
            root::Output::Llm { client, request } => peers.llm.answer(clock, client, request),
            root::Output::Commit { .. } | root::Output::Stop => unreachable!("harness consumed commit/stop"),
        }
    }
    fn poll(
        peers: &mut Peers,
        systems: &mut Systems,
        store: &mut Store,
        clock: Clock,
        ready: bool,
    ) -> Vec<Input<root::Event, root::Record>> {
        let now = clock.now / 1_000_000_000;
        if now > systems.now {
            systems.production.advance(now - systems.now);
            systems.now = now;
        }
        let mut events = Vec::new();
        if ready && !peers.account_ready {
            peers.account_ready = true;
            events.push(Input::Event(root::Event::Core(core::Event::Account(accounts::Event::Add {
                account: 1,
                generation: 1,
                valid: Some(Duration::from_secs(7200)),
            }))));
        }
        if ready && !peers.asked_sign_in {
            peers.asked_sign_in = true;
            events.push(Input::Event(root::Event::Core(core::Event::SignIn {
                reply_to: ReplyTo::new(Token::new(100)),
                identity: people::Identity {
                    key: people::IdentityKey { provider: 0, subject: 7_u64.to_be_bytes().into() },
                    login: b"oncall".as_slice().into(),
                    name: Box::new([]),
                },
            })));
        }
        if ready && let Some(sign_in) = peers.sign_in {
            for record in store.rows.values() {
                if let root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) = record
                    && let Some(proposal) = &row.proposal
                    && matches!(
                        proposal.state,
                        tasks::ProposalState::Pending { holder: tasks::ProposalHolder::Person(1), .. }
                    )
                    && peers.asked_accept.insert((row.number, proposal.number))
                {
                    events.push(Input::Event(root::Event::Core(core::Event::People(people::Event::Ask {
                        reply_to: ReplyTo::new(Token::new(200)),
                        sign_in,
                        key: {
                            let mut key = [0; 16];
                            key[..8].copy_from_slice(&row.number.to_be_bytes());
                            key[8..].copy_from_slice(&proposal.number.to_be_bytes());
                            key
                        },
                        ask: people::Ask::DecideProposal {
                            project: 1,
                            proposer: row.number,
                            proposal: proposal.number,
                            decision: people::ProposalDecision::Accept,
                        },
                    }))));
                }
            }
        }
        events
    }
    fn observed(peers: &mut Peers, _: &Systems, store: &Store, _: Clock) -> Vec<r::Observed> {
        let mut observed = Vec::new();
        if peers.applied != store.applied {
            let snapshot = observations::snapshot(store);
            for row in store.rows.values() {
                if let root::Record::Core(core::Record::Core(core::CoreRecord::ProposalDecision(row))) = row
                    && row.choice == people::ProposalChoice::Accepted
                    && !peers.accepted.contains(&store.applied)
                    && peers.accepted.is_empty()
                {
                    peers.accepted.push(store.applied);
                }
            }
            for decision in &snapshot.decisions {
                if !peers.entries.iter().any(|(commit, kind)| *commit == store.applied && *kind == decision.kind) {
                    peers.entries.push((store.applied, decision.kind));
                }
            }
            observed.push(r::Observed::Durable { number: store.applied, snapshot });
            for row in store.rows.values() {
                if let root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) = row {
                    for word in &row.inbox {
                        if let (tasks::MessageKind::Result(_), tasks::Party::Task(child)) = (&word.kind, word.from)
                            && peers.delivered_results.insert(child)
                        {
                            observed.push(r::Observed::Result { task: child, to: r::Source::Task(row.number) });
                        }
                    }
                }
            }
            peers.applied = store.applied;
        }
        peers.pending_results.retain(|&(commit, task, parent)| {
            if commit > store.applied {
                return true;
            }
            if peers.delivered_results.insert(task) {
                observed.push(r::Observed::Result { task, to: r::Source::Task(parent) });
            }
            false
        });
        observed.append(&mut peers.observed);
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
        root::worst_case(&config::root(config.seed).1).expect("finite scenario root")
    }
    fn configuration_heap(_: &Config) -> u64 {
        0
    }
}

/// Play the night's recurring template without any agent assignment.
/// Returns the actual harness so the test can inspect outside evidence.
#[must_use]
pub fn night(seed: u64, cut: jig_conformance::Cut) -> jig_conformance::Harness<Ops> {
    let mut world = jig_conformance::Harness::<Ops>::new(Config::new(seed, Story::Night), seed);
    world.cut(cut);
    world.drain().expect("cold ops root");
    world.send(root::Event::Infrastructure(infra::Event::Names {
        project: 1,
        task: 1,
        resources: Box::new([infra::Resource::Service(infra::Service::new(
            b"staging".as_slice().into(),
            b"checkout".as_slice().into(),
        ))]),
    }));
    world.send(root::Event::Core(core::Event::Tasks(tasks::Event::Kinds {
        connector: 2,
        kinds: Box::new([tasks::Kind {
            connector: 2,
            kind: 1,
            hold: tasks::HoldKind::Exclusive { taken: tasks::Taken::Waits },
        }]),
    })));
    world.drain().expect("infrastructure write hold configuration");
    world.advance(1800 * 1_000_000_000);
    world.drain().expect("thirty minutes of staging observations");
    let mut child_authority = config::task_authority(10);
    child_authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    let mut standing = config::task_authority(30);
    standing.delegation =
        tasks::Delegation { kinds: Box::new([tasks::AuthorityExecutor::Procedure(1)]), tasks: 1, depth: 1 };
    world.send(root::Event::Core(core::Event::StartRecurring {
        project: 1,
        authority: standing,
        template: tasks::RecurringTemplate {
            key: 1,
            overlap: tasks::RecurringOverlap::Skip,
            batch: Box::new([tasks::New {
                number: 1,
                project: 1,
                executor: tasks::Executor::Procedure { connector: 2, code: 1 },
                spec: tasks::Spec {
                    words: b"night scale staging".as_slice().into(),
                    parameters: Box::new([
                        tasks::Parameter::Bytes { name: 1, value: b"staging".as_slice().into() },
                        tasks::Parameter::Bytes { name: 2, value: b"checkout".as_slice().into() },
                        tasks::Parameter::Number { name: 3, value: 1 },
                        tasks::Parameter::Number { name: 4, value: 7200 },
                    ]),
                    inputs: Box::new([]),
                },
                contract: tasks::Contract::Report { words: 128 },
                authority: child_authority,
                numbers: tasks::Numbers { budget: 10, spent: 0, spent_below: 0, reserved: 0 },
                funder: tasks::Funder::Period { project: 1, period: 0 },
                dependencies: Box::new([]),
                holdings: Box::new([tasks::Holding::Write {
                    resource: tasks::Name {
                        connector: 2,
                        path: infra::Resource::Service(infra::Service::new(
                            b"staging".as_slice().into(),
                            b"checkout".as_slice().into(),
                        ))
                        .segments(),
                    },
                    kind: 1,
                }]),
                wake: tasks::WakePolicy::DEFAULT,
                recurring: None,
                tracked: None,
            }]),
        },
    }));
    world.drain().unwrap_or_else(|error| panic!("night: {error:?}\n{}", world.trace.join("\n")));
    // A crash after the sent entry may leave a conditional write uncertain.
    // Advance its configured retry bound so lookup and settlement can finish.
    world.advance(config::root(seed).1.infrastructure.retry_after_seconds * 1_000_000_000);
    world.drain().unwrap_or_else(|error| panic!("night retry: {error:?}\n{}", world.trace.join("\n")));
    world
}

/// The alert storm and incident, including the person's authenticated decision.
#[must_use]
pub fn alert(seed: u64, cut: jig_conformance::Cut) -> jig_conformance::Harness<Ops> {
    let mut world = jig_conformance::Harness::<Ops>::new(Config::new(seed, Story::Alert), seed);
    world.cut(cut);
    world.drain().expect("cold ops root");
    world.send(root::Event::Infrastructure(infra::Event::Names {
        project: 1,
        task: 1,
        resources: Box::new([infra::Resource::Service(infra::Service::new(
            b"production".as_slice().into(),
            b"checkout".as_slice().into(),
        ))]),
    }));
    world.send(root::Event::Core(core::Event::Tasks(tasks::Event::Kinds {
        connector: 2,
        kinds: Box::new([tasks::Kind {
            connector: 2,
            kind: 1,
            hold: tasks::HoldKind::Exclusive { taken: tasks::Taken::Waits },
        }]),
    })));
    world.send(root::Event::Core(core::Event::People(people::Event::Ask {
        reply_to: ReplyTo::new(Token::new(101)),
        sign_in: world.peers.sign_in.expect("on-call session"),
        key: [2; 16],
        ask: people::Ask::StartChat { project: 1, words: b"@watch_setup".as_slice().into() },
    })));
    world.drain().unwrap_or_else(|error| panic!("watch: {error:?}\n{}", world.trace.join("\n")));
    for number in [7, 8, 9] {
        world.send(root::Event::Observability(obs::Event::System(obs::SystemEvent::Alert {
            own: false,
            alert: obs::Alert {
                number,
                service: obs::Service::new(b"production".as_slice().into(), b"checkout".as_slice().into()),
                severity: 1,
            },
        })));
    }
    world.drain().unwrap_or_else(|error| panic!("storm: {error:?}\n{}", world.trace.join("\n")));
    world.systems.production.inject_incident(&production::ServiceName::new("production", "checkout"));
    world.send(root::Event::Observability(obs::Event::System(obs::SystemEvent::Alert {
        own: false,
        alert: obs::Alert {
            number: 12,
            service: obs::Service::new(b"production".as_slice().into(), b"checkout".as_slice().into()),
            severity: 3,
        },
    })));
    world.advance(1_000_000_000);
    world.drain().unwrap_or_else(|error| panic!("incident: {error:?}\n{}", world.trace.join("\n")));
    world.advance(config::root(seed).1.infrastructure.retry_after_seconds * 1_000_000_000);
    world.drain().unwrap_or_else(|error| panic!("recovery: {error:?}\n{}", world.trace.join("\n")));
    world.advance(config::root(seed).1.infrastructure.retry_after_seconds * 1_000_000_000);
    world.drain().expect("settle an uncertain restart after its retry deadline");
    world.advance(10_000_000_000);
    world.drain().expect("production recovery clock");
    let refreshed = world.systems.infrastructure(infra::SystemRequest::Service {
        service: infra::Service::new(b"production".as_slice().into(), b"checkout".as_slice().into()),
    });
    world.send(root::Event::Infrastructure(infra::Event::System(refreshed)));
    world.drain().unwrap_or_else(|error| panic!("result: {error:?}\n{}", world.trace.join("\n")));
    world.advance(1_000_000_000);
    world.drain().expect("read the result");
    read_report(&mut world, cut);
    world
}

fn read_report(world: &mut jig_conformance::Harness<Ops>, cut: jig_conformance::Cut) {
    // The scripted client reads the committed archive under this fixture's
    // authenticated project-owner permission, without copying a live result.
    let sign_in = world.peers.sign_in.expect("on-call session");
    let row = world
        .store
        .rows
        .get(&root::Key::Core(core::Key::People(people::Key::SignIn(sign_in))))
        .expect("durable sign-in");
    let root::Record::Core(core::Record::People(people::Stored::SignIn { person, expires, .. })) = row else {
        panic!("sign-in row");
    };
    assert_eq!(*person, 1);
    assert!(expires.as_nanos() > world.clock().now);
    let role = world.store.rows.get(&root::Key::Core(core::Key::People(people::Key::Roles(1)))).expect("project roles");
    let root::Record::Core(core::Record::People(people::Stored::Roles { holdings, .. })) = role else {
        panic!("roles row");
    };
    assert!(holdings.iter().any(|holding| holding.person == *person && holding.role == people::Role::Owner));
    let result = world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(5)))).unwrap_or_else(|| {
        panic!(
            "incident report missing {cut:?}: {:?}",
            world
                .store
                .rows
                .values()
                .filter_map(|row| match row {
                    root::Record::Core(core::Record::Tasks(tasks::Stored::Live(task))) if task.number == 5 =>
                        Some((task.phase.clone(), task.inbox.clone(), task.numbers)),
                    root::Record::Core(_) | root::Record::Observability(_) | root::Record::Infrastructure(_) => None,
                })
                .collect::<Vec<_>>()
        )
    });
    let root::Record::Core(core::Record::Tasks(tasks::Stored::Ended(task))) = result else {
        panic!("ended task row");
    };
    assert_eq!(task.project, 1);
    assert!(
        matches!(&task.phase, tasks::Phase::Ended(tasks::Ending::Done(tasks::TaskResult::Report { words })) if words.as_ref() == b"checkout recovered after the accepted restart")
    );
    world.peers.results.push(task.number);
}
