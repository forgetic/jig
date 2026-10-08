//! A scripted caller and two independent systems exercise the root boundary.
use jig_core as core;
use jig_core_accounts as accounts;
use jig_core_authority as authority;
use jig_core_fleet as fleet;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_fake_store::Store;
use jig_test_connector as connector;
use jig_test_domain as root;
use jig_test_system::{Fault, System};
use skein_lib::{Duration, Env, List, Queue, ReplyTo, Time, Token, Wall};
use std::collections::VecDeque;

/// Root, store and separate systems, with an answer channel that may disappear.
#[derive(Debug)]
pub struct World {
    pub domain: root::Domain,
    pub store: Store,
    pub systems: [System; 2],
    pub typed_calls: Vec<(Token, u64, u64, fleet::TypedCall)>,
    pub typed_answers: Vec<(Box<[u8]>, core::SettledCall)>,
    pub inbound: Vec<tasks::Word>,
    pub answers: Vec<(Token, core::CallKey, core::CallPart)>,
    pub assigned: Option<root::Assignment>,
    pub sign_in: Option<u64>,
    pub people_answers: Vec<people::Reply>,
    pub procedure: Option<u64>,
    pub pending_reads: Vec<(u16, connector::SystemRequest)>,
    pub lost_answers: u32,
    pub hold_writes: bool,
    pub pending_writes: Vec<(u16, connector::SystemRequest)>,
    pub lose_answer: bool,
    pub trace: Vec<String>,
    events: VecDeque<root::Event>,
    limits: root::Limits,
    wall: u64,
}

fn grant() -> authority::Grant {
    authority::Grant {
        connector: 1,
        kind: 5,
        pattern: authority::Pattern { segments: Box::new([]), last: authority::Last::Open(Box::new([])) },
    }
}

fn fixture(seed: u64, requirement: bool) -> (root::Config, root::Limits) {
    let mut limits = crate::world::limits();
    limits.core.authority.segments = 2;
    limits.core.people.requests = 16;
    let mut configuration = crate::world::config(seed);
    let mut rules = configuration.core.authority.rules().clone();
    rules.ceiling.grants = Box::new([grant()]);
    let mut policy = configuration.core.authority.policy(1).expect("configured policy").clone();
    policy.ceiling.grants = Box::new([grant()]);
    policy.roles[0].authority.grants = Box::new([grant()]);
    if requirement {
        policy.requirements = Box::new([authority::Requirement {
            connector: 1,
            kind: 5,
            pattern: grant().pattern,
            judge: authority::Judge { connector: 2, requirement: 9, parameters: 0 },
            guard: authority::Guard::Observed { freshness: Duration::from_secs(60) },
            must_be_guarded: false,
        }]);
    }
    let mut domain = authority::Domain::new(rules, limits.core.authority).expect("bounded effect rules");
    let mut out = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(&mut domain, authority::Event::Policy { project: 1, policy }, &mut out);
    assert_eq!(out.pop(), Some(authority::PolicyFact::Added { project: 1 }));
    configuration.core.settings.chat_authority.grants = Box::new([grant()]);
    configuration.core.settings.chat_authority.budget.spend = 300;
    configuration.core.authority = domain;
    configuration.first.procedures = Box::new([connector::ProcedureSpec {
        number: 1,
        actions: Box::new([
            connector::ProcedureAction::Effect { kind: 5, purpose: 19, target: 13 },
            connector::ProcedureAction::Finish { result: connector::ProcedureResult::Succeeded },
        ]),
        max_steps: 3,
        stall: Duration::from_secs(1),
    }]);
    // Both connectors name the same resource; their facts come from distinct
    // systems. Connector two is the observer, connector one is the writer.
    configuration.second.prefix = configuration.first.prefix.clone();
    configuration.second.resources.clone_from(&configuration.first.resources);
    configuration.second.pools.clone_from(&configuration.first.pools);
    configuration.second.requirements =
        Box::new([connector::RequirementSpec { number: 9, guarded: false, freshness: Duration::from_secs(60) }]);
    (configuration, limits)
}

impl World {
    #[must_use]
    pub fn new(seed: u64, requirement: bool) -> World {
        Self::with_entries(seed, requirement, crate::world::limits().connector.entries)
    }

    /// A finite connector outbox that can be filled by a pending write.
    #[must_use]
    pub fn with_entries(seed: u64, requirement: bool, entries: u32) -> World {
        let (configuration, mut limits) = fixture(seed, requirement);
        limits.connector.entries = entries;
        let mut world = World {
            domain: root::Domain::new(configuration, &limits),
            store: Store::new(),
            systems: [System::new(), System::new()],
            typed_calls: Vec::new(),
            typed_answers: Vec::new(),
            inbound: Vec::new(),
            answers: Vec::new(),
            assigned: None,
            sign_in: None,
            people_answers: Vec::new(),
            procedure: None,
            pending_reads: Vec::new(),
            lost_answers: 0,
            hold_writes: false,
            pending_writes: Vec::new(),
            lose_answer: false,
            trace: Vec::new(),
            events: VecDeque::new(),
            limits,
            wall: 0,
        };
        world.events.extend([
            root::Event::Core(core::Event::People(people::Event::Roles { project: 1, holdings: Box::new([]) })),
            root::Event::Core(core::Event::People(people::Event::Restored)),
            root::Event::Core(core::Event::Tasks(tasks::Event::Restored)),
            root::Event::Core(core::Event::Fleet(fleet::Event::Loaded)),
            root::Event::Core(core::Event::Account(accounts::Event::Add {
                account: 1,
                generation: 1,
                valid: Some(Duration::from_secs(60)),
            })),
            root::Event::Core(core::Event::Fleet(fleet::Event::Hello {
                channel: Token::new(7),
                hello: fleet::Hello {
                    stop_bound: Duration::from_millis(100),
                    slots: 1,
                    workstreams: Box::new([]),
                    hosting: Box::new([]),
                },
            })),
            root::Event::Core(core::Event::SignIn {
                reply_to: ReplyTo::new(Token::new(101)),
                identity: people::Identity {
                    key: people::IdentityKey { provider: 0, subject: 7_u64.to_be_bytes().into() },
                    login: b"person".as_slice().into(),
                    name: b"Person".as_slice().into(),
                },
            }),
        ]);
        world.drain();
        assert!(world.assigned.is_some(), "the real task hub and fleet assigned the caller: {:?}", world.trace);
        world
    }

    fn env(&self) -> Env<root::Limits> {
        Env { now: Time::from_nanos(self.wall), wall: Wall::from_nanos(self.wall), limits: self.limits }
    }

    pub fn send(&mut self, event: root::Event) {
        self.events.push_back(event);
        self.drain();
    }

    pub fn drain(&mut self) {
        for _ in 0..300 {
            self.wall += 1_000_000;
            let env = self.env();
            if let Some(event) = self.events.pop_front() {
                self.trace.push(format!("input {event:?}"));
                root::step(&mut self.domain, &env, event);
            }
            let mut out = Queue::with_capacity(128);
            root::release(&mut self.domain, &env, &mut out);
            let empty = out.is_empty();
            for _ in 0..out.len() {
                let request = out.pop().expect("root output count");
                self.trace.push(format!("output {request:?}"));
                match request {
                    root::Request::Commit { number, mut writes } => {
                        let mut rows = Vec::new();
                        while let Some(write) = writes.pop() {
                            rows.push(write);
                        }
                        self.store.submit(number, rows.into_boxed_slice(), 0);
                        let number = self.store.tick(false).expect("store stays available").expect("ready commit");
                        self.events.push_front(root::Event::Committed { number });
                    }
                    root::Request::Deliver(delivery) => self.delivered(delivery),
                    root::Request::Now(core::Now::StartPreparation { task, .. }) => {
                        let rows: Vec<_> = self
                            .store
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
                        for row in rows {
                            self.events.push_back(root::Event::TranscriptLoaded {
                                task,
                                rows: Box::new([row]),
                                done: false,
                            });
                        }
                        self.events.push_back(root::Event::TranscriptLoaded { task, rows: Box::new([]), done: true });
                    }
                    root::Request::Now(core::Now::CallTyped { to, run, attempt, call }) => {
                        self.typed_calls.push((to.into_token(), run.raw(), attempt.raw(), call));
                    }
                    root::Request::Now(core::Now::EffectAnswer { to, key, part }) => {
                        self.answers.push((to.into_token(), key, part));
                    }
                    root::Request::Now(core::Now::Account(accounts::Request::Refresh { account, generation })) => {
                        self.events.push_back(root::Event::Core(core::Event::Account(accounts::Event::Refreshed {
                            account,
                            generation,
                            valid: Duration::from_secs(60),
                        })));
                    }
                    root::Request::Now(_) => {}
                    root::Request::Stop => panic!("root stopped: {:?}", self.trace),
                }
            }
            self.domain.reclaim();
            if empty && self.events.is_empty() && self.domain.quiescent() {
                return;
            }
        }
        panic!("effect world did not settle: {:?}", self.trace);
    }

    fn delivered(&mut self, delivery: root::Delivery) {
        match delivery {
            root::Delivery::TypedAnswer { name, call, .. } => self.typed_answers.push((name, call)),
            root::Delivery::Core(core::Held::Inbound { word, .. }) => self.inbound.push(word),
            root::Delivery::Assigned { assignment, .. } => self.assigned = Some(assignment),
            root::Delivery::Procedure { task, connector: number, code, .. } => {
                self.procedure = Some(task);
                self.events.push_back(root::Event::Connector {
                    number,
                    event: connector::Event::Procedure {
                        task,
                        number: code,
                        resource: jig_test_connector_world::path(1, 1),
                        signal: connector::ProcedureSignal::Activate,
                    },
                });
            }
            root::Delivery::System {
                connector: number,
                call: connector::SystemRequest::ReadFact { resource, observed },
            } => self.pending_reads.push((number, connector::SystemRequest::ReadFact { resource, observed })),
            root::Delivery::System { connector: number, call } => {
                if let connector::SystemRequest::Apply { entry, key, .. } = &call {
                    assert!(self.store.rows.values().any(|row| matches!(row, root::Record::Connector { number: owner, record: connector::Record::Outbox(row) } if *owner == number && row.number == *entry && row.key == *key)), "system sees only a committed outbox attempt");
                    assert_eq!(key.deployment, [31; 16]);
                }
                if self.hold_writes && matches!(call, connector::SystemRequest::Apply { .. }) {
                    self.pending_writes.push((number, call));
                    return;
                }
                let event = self.systems[usize::from(number - 1)].answer(call, Fault::None);
                self.events.push_back(root::Event::Connector { number, event: connector::Event::System(event) });
            }
            root::Delivery::Core(core::Held::PeopleReply {
                sign_in: Some(sign_in),
                reply: people::Reply::SignedIn { .. },
                ..
            }) => {
                self.sign_in = Some(sign_in);
                self.events.push_back(root::Event::Core(core::Event::People(people::Event::Ask {
                    reply_to: ReplyTo::new(Token::new(102)),
                    sign_in,
                    key: [5; 16],
                    ask: people::Ask::StartChat { project: 1, words: b"make an effect".as_slice().into() },
                })));
            }
            root::Delivery::Core(core::Held::NotesLoad { owner, .. }) => {
                self.events.push_back(root::Event::Core(core::Event::Notes(jig_core_notes::Event::Loaded {
                    owner,
                    rows: jig_core_notes::Rows { records: List::with_capacity(1) },
                    more: false,
                })));
            }
            root::Delivery::Core(core::Held::CallAnswer { to, key, part }) => {
                assert!(
                    self.store.rows.contains_key(&root::Key::Core(core::Key::Core(core::CoreKey::Call(key)))),
                    "state-changing answer has its committed record"
                );
                if self.lose_answer {
                    self.lost_answers += 1;
                } else {
                    self.answers.push((to.into_token(), key, part));
                }
            }
            root::Delivery::Core(core::Held::PeopleReply { reply, .. }) => self.people_answers.push(reply),
            root::Delivery::Core(
                core::Held::SettledCall { .. }
                | core::Held::AssignTyped { .. }
                | core::Held::InboundTyped { .. }
                | core::Held::RelayedTyped { .. }
                | core::Held::ViewStart { .. }
                | core::Held::ViewFinished { .. }
                | core::Held::ViewTaskPhase { .. }
                | core::Held::ViewTurn { .. }
                | core::Held::Relay { .. }
                | core::Held::Result { .. }
                | core::Held::Acknowledge { .. }
                | core::Held::AcknowledgeTurn { .. }
                | core::Held::TaskTurnKept { .. }
                | core::Held::TaskTerminalAcknowledged { .. }
                | core::Held::NotesWritten { .. }
                | core::Held::NotesDeleted { .. }
                | core::Held::TurnBusy { .. }
                | core::Held::Relayed { .. }
                | core::Held::StopRun { .. }
                | core::Held::Cancel { .. }
                | core::Held::Refuse { .. }
                | core::Held::Assign { .. }
                | core::Held::MakeEffect { .. },
            ) => {}
            root::Delivery::Fleet(_) => panic!("root releases its fleet continuations internally"),
        }
    }

    #[must_use]
    pub fn call_key(&self, completion: u32) -> core::CallKey {
        let assignment = self.assigned.as_ref().expect("assigned caller");
        core::CallKey { task: assignment.task, attempt: assignment.attempt, completion, position: 0 }
    }

    #[must_use]
    pub fn effect() -> connector::Effect {
        connector::Effect {
            kind: 5,
            resources: Box::new([jig_test_connector_world::path(1, 1)]),
            purpose: 19,
            condition: None,
            target: 13,
            state: 13,
        }
    }

    pub fn call(&mut self, completion: u32, right: u64) {
        self.send(root::Event::EffectCall {
            to: ReplyTo::new(Token::new(right)),
            key: self.call_key(completion),
            number: 1,
            effect: Self::effect(),
            deadline: Wall::from_nanos(self.wall + 1_000_000_000),
            proposal: None,
        });
    }

    /// The caller's independently stored expense, including made effects.
    #[must_use]
    pub fn spent(&self) -> u64 {
        let task = self.call_key(1).task;
        match self.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))) {
            Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) => row.numbers.spent,
            row => panic!("live caller's stored balance: {row:?}"),
        }
    }

    /// Expense debited to the accepting person's finite pool.
    #[must_use]
    pub fn holder_spent(&self) -> u64 {
        match self.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Ledger(tasks::Funder::Pool {
            project: 1,
            person: 1,
            period: 1,
        })))) {
            Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Ledger(row)))) => row.numbers.spent,
            row => panic!("holder's stored balance: {row:?}"),
        }
    }

    /// Explicitly propose the connector payload instead of making it.
    pub fn propose(&mut self, completion: u32) -> u64 {
        self.send(root::Event::EffectCall {
            to: ReplyTo::new(Token::new(220)),
            key: self.call_key(completion),
            number: 1,
            effect: Self::effect(),
            deadline: Wall::from_nanos(self.wall + 1_000_000_000),
            proposal: Some(b"ask the holder".as_slice().into()),
        });
        match self.answers.last() {
            Some((_, _, core::CallPart::Proposed { proposal })) => *proposal,
            answer => panic!("effect proposed: {answer:?}; {:?}", self.trace),
        }
    }

    /// The signed-in person decides the caller's proposal through people.
    pub fn decide_proposal(&mut self, proposal: u64, accept: bool, request: u8) {
        let decision = if accept {
            people::ProposalDecision::Accept
        } else {
            people::ProposalDecision::Reject { reason: b"not now".as_slice().into() }
        };
        self.send(root::Event::Core(core::Event::People(people::Event::Ask {
            reply_to: ReplyTo::new(Token::new(u64::from(request) + 230)),
            sign_in: self.sign_in.expect("signed-in person"),
            key: [request; 16],
            ask: people::Ask::DecideProposal { project: 1, proposer: self.call_key(1).task, proposal, decision },
        })));
    }

    /// Cross the call's absolute deadline while its committed write is pending.
    pub fn deadline(&mut self) {
        self.wall += 1_000_000_000;
        self.send(root::Event::Core(core::Event::EffectDeadline));
    }

    /// Deliver the system answers held past the caller's deadline.
    pub fn release_writes(&mut self) {
        self.hold_writes = false;
        for (number, call) in std::mem::take(&mut self.pending_writes) {
            let event = self.systems[usize::from(number - 1)].answer(call, Fault::None);
            self.events.push_back(root::Event::Connector { number, event: connector::Event::System(event) });
        }
        self.drain();
    }

    pub fn delegate(&mut self) {
        let assignment = self.assigned.as_ref().expect("assigned caller");
        let mut authority = assignment.run.authority.clone();
        authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
        authority.budget.spend = 20;
        authority.tools = tasks::Tools(0);
        self.send(root::Event::Core(core::Event::DelegateValidated {
            to: ReplyTo::new(Token::new(201)),
            key: self.call_key(1),
            batch: Box::new([core::Delegate {
                executor: tasks::Executor::Procedure { connector: 1, code: 1 },
                spec: tasks::Spec {
                    words: b"make a checked effect".as_slice().into(),
                    parameters: Box::new([]),
                    inputs: Box::new([]),
                },
                contract: tasks::Contract::Report { words: 128 },
                authority,
                symbolic_grants: Box::new([]),
                dependencies: Box::new([]),
                wake: tasks::WakePolicy::DEFAULT,
            }]),
            stubs: Box::new([]),
        }));
        assert!(self.procedure.is_some(), "delegated procedure admitted: {:?}", self.trace);
    }

    /// Read the observer's system, then give the idle procedure another step.
    pub fn observed(&mut self, state: u64) {
        self.systems[1].other_hand(&jig_test_connector_world::path(1, 1), state);
        let reads = std::mem::take(&mut self.pending_reads);
        for (number, call) in reads {
            let event = self.systems[usize::from(number - 1)].answer(call, Fault::None);
            self.events.push_back(root::Event::Connector { number, event: connector::Event::System(event) });
        }
        self.drain();
        if let Some(task) = self.procedure {
            self.send(root::Event::Core(core::Event::Tasks(tasks::Event::WakeProcedure { task })));
        }
    }

    /// Rebuild all live components from the fake store, preserving the systems.
    pub fn restart(&mut self, seed: u64, requirement: bool) {
        self.domain = root::Domain::new(fixture(seed, requirement).0, &self.limits);
        self.events.clear();
        let env = self.env();
        for group in 0..4 {
            for row in self.store.rows.values() {
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
                        | core::Record::Tasks(tasks::Stored::History(_)),
                    ) => continue,
                    root::Record::Core(core::Record::Tasks(_) | core::Record::People(_) | core::Record::Notes(_))
                    | root::Record::Connector { .. } => 1,
                };
                if group == rank {
                    assert!(root::restore_record(&mut self.domain, &env, row.clone()), "restored {row:?}");
                }
            }
        }
        self.send(root::Event::Core(core::Event::People(people::Event::Restored)));
        self.send(root::Event::Core(core::Event::Tasks(tasks::Event::Restored)));
        self.send(root::Event::Core(core::Event::Fleet(fleet::Event::Loaded)));
        self.lose_answer = false;
    }
}

impl World {
    pub fn say(&mut self, key: u8) -> u64 {
        let task = self.assigned.as_ref().expect("assigned chat").task;
        self.send(root::Event::Core(core::Event::People(people::Event::Ask {
            reply_to: ReplyTo::new(Token::new(3000 + u64::from(key))),
            sign_in: self.sign_in.expect("signed in"),
            key: [key; 16],
            ask: people::Ask::Say { project: 1, task, words: Box::from([key]) },
        })));
        self.people_answers
            .iter()
            .rev()
            .find_map(|reply| match reply {
                people::Reply::Outcome(people::Outcome::Said { message, .. }) => Some(*message),
                people::Reply::SignedIn { .. }
                | people::Reply::SignedOut
                | people::Reply::Outcome(_)
                | people::Reply::Refused(_) => None,
            })
            .expect("durable words")
    }

    pub fn turn(&mut self, number: u32, spent: u64, read: Option<u64>, body: &[u8]) {
        let assignment = self.assigned.as_ref().expect("assigned chat");
        self.send(root::Event::Turn {
            channel: Token::new(7),
            task: assignment.task,
            attempt: assignment.attempt,
            turn: number,
            cumulative: spent,
            read,
            transcript: body.into(),
        });
    }

    pub fn end(&mut self, end: tasks::End, cumulative: u64) {
        let assignment = self.assigned.as_ref().expect("assigned chat");
        self.send(root::Event::Answer {
            channel: Token::new(7),
            task: assignment.task,
            attempt: assignment.attempt,
            cumulative,
            end,
        });
    }

    pub fn typed(&mut self, name: &[u8], tool: &[u8], input: &[u8], writes: bool) -> Token {
        let assignment = self.assigned.as_ref().expect("assigned chat");
        self.send(root::Event::Core(core::Event::Fleet(fleet::Event::RelayTyped {
            channel: Token::new(7),
            run: Token::new(assignment.task),
            attempt: Token::new(assignment.attempt),
            call: fleet::TypedCall {
                name: name.into(),
                tool: tool.into(),
                writes,
                input: input.into(),
                deadline: Duration::from_secs(2),
            },
        })));
        self.typed_calls.last().expect("fleet authenticated typed call").0
    }
}

impl World {
    pub fn retry_due(&mut self) {
        self.wall += 1_000_000_000;
        self.send(root::Event::Timer(core::Timer::Tasks));
    }
}
