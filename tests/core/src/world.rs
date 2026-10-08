//! Seeded fixtures and scripts for the testing application.

use crate::Store;
use jig_core as core;
use jig_core_accounts as accounts;
use jig_core_authority as authority;
use jig_core_fleet as fleet;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_test_domain as root;
use skein_lib::{Duration, Env, JournalLimits, Queue, ReplyTo, Time, Token, Wall};
use std::collections::VecDeque;

#[must_use]
#[expect(clippy::too_many_lines, reason = "the walking fixture states every child bound")]
pub fn limits() -> root::Limits {
    let retry = tasks::Retry { retries: 1, base: Duration::from_millis(10), max: Duration::from_secs(1) };
    let tasks = tasks::Limits {
        tasks: 2,
        funders: 4,
        project_tasks: 2,
        tree_tasks: 2,
        depth: 1,
        delegates: 1,
        references: 2,
        subscriptions: 2,
        batch: 1,
        dependencies: 1,
        holdings: 2,
        hold_kinds: 2,
        pools: 2,
        hold_segments: 4,
        hold_bytes: 128,
        hold_waiters: 2,
        hold_wait: Duration::from_secs(10),
        inputs: 1,
        spec_bytes: 64,
        parameters: 1,
        result_bytes: 128,
        inbox_messages: 4,
        inbox_bytes: 128,
        message_bytes: 64,
        proposal_stall: Duration::from_millis(10),
        escalation_stall: Duration::from_secs(3600),
        saved_resources: 2,
        contract_choices: 1,
        charters: 1,
        authority_grants: 1,
        authority_segments: 1,
        authority_bytes: 32,
        executor_kinds: 2,
        retries: tasks::Retries {
            transient: retry,
            permanent: retry,
            run: retry,
            agent: retry,
            lost: retry,
            invalid: retry,
        },
        facts: 2,
    };
    let people = people::Limits {
        people: 2,
        inbox_entries: 2,
        sign_ins: 2,
        projects: 1,
        holdings: 2,
        goals: 4,
        initial_owners: 1,
        requests: 4,
        pending: 2,
        waiters: 2,
        identity_bytes: 32,
        words: 64,
        amendment_bytes: 512,
        sign_in_lifetime: Duration::from_secs(60),
        request_retention: Duration::from_secs(120),
        facts: 2,
    };
    let fleet = fleet::Limits {
        workers: 1,
        engine_slots: 0,
        slots: 1,
        workstreams: 1,
        attempts: 2,
        calls: 1,
        call_name_bytes: 64,
        turns: 2,
        grace: Duration::from_secs(5),
        facts: 2,
    };
    let writes = tasks::max_out(&tasks) * 8
        + people::max_out(&people) * 4
        + people.pending * 2
        + fleet::max_out(&fleet) * 4
        + jig_test_connector::MAX_OUT * 2
        + tasks.tasks
        + 6;
    root::Limits {
        journal: JournalLimits { commits: 3, held: 2000, writes: writes.max(2000), now: 32, release: 32 },
        routes: 200,
        connector: jig_test_connector_world::LIMITS,
        core: core::Limits {
            connectors: 2,
            resume_bytes: 256,
            run_bytes: 256,
            policy_bytes: 8192,
            escalation_reason_bytes: 128,
            load_slots: 2,
            call_records: 2,
            call_answer_bytes: 256,
            tasks,
            authority: authority_limits(),
            people,
            fleet,
            brief: jig_core_brief::Limits { briefs: 2, sections: 3, read_bytes: 256, brief_bytes: 384 },
            brief_parts: 2,
            brief_core_budgets: core::CoreBriefBudgets {
                task: 128,
                dependencies: 128,
                attempts: 128,
                plan: 128,
                notes: 64,
            },
            accounts: accounts::Limits {
                accounts: 1,
                refresh_margin: Duration::from_secs(1),
                backoff_base: Duration::from_millis(10),
                backoff_max: Duration::from_secs(1),
                rejected_interval: Duration::from_millis(10),
                spent_attention: Duration::from_secs(1),
                facts: 2,
            },
            views: jig_core_views::Limits {
                runs: 2,
                watchers: 4,
                backlog: 2,
                report_bytes: 64,
                snapshot_bytes: 128,
                facts: 8,
            },
            notes: jig_core_notes::Limits {
                scopes: 4,
                entries_per_scope: 1,
                pattern_bytes: 64,
                description_bytes: 64,
                body_bytes: 256,
                references: 1,
                load_rows: 1,
                lines: 1,
                recalled: 1,
            },
        },
    }
}

fn authority_limits() -> authority::Limits {
    authority::Limits {
        projects: 1,
        roles: 1,
        requirements: 1,
        facts: 2,
        grants: 1,
        executors: 2,
        segments: 1,
        segment_bytes: 32,
        implications: 0,
        batch: 1,
        accounts: 1,
        writes: 1,
    }
}

fn authority_value(spend: u64, kinds: Box<[authority::Executor]>) -> authority::Authority {
    authority::Authority {
        tools: authority::Tools(1023),
        grants: Box::new([]),
        delegation: authority::Delegation { kinds, tasks: 3, depth: 2 },
        budget: authority::Budget { spend, deadline: None },
        notes: authority::Scopes(0),
        note_resources: Box::new([]),
    }
}

/// Real authority policy and an authenticated owner, rather than allow flags.
#[must_use]
#[expect(clippy::too_many_lines, reason = "the walking fixture states the whole deployment")]
pub fn config(seed: u64) -> root::Config {
    let ceiling = authority_value(1000, Box::new([authority::Executor::Charter(1), authority::Executor::Procedure(1)]));
    let rules = authority::Rules {
        ceiling: ceiling.clone(),
        period_spend: 1000,
        minimum_run_spend: 1,
        maximum_run_spend: 100,
        implies: authority::Implies::new(Box::new([]), 0).expect("empty implication configuration"),
        requirements: Box::new([]),
    };
    let mut domain = authority::Domain::new(rules, authority_limits()).expect("valid authority configuration");
    let policy = authority::Policy {
        escalation_role: Some(0),
        ceiling,
        period_spend: 1000,
        roles: Box::new([authority::Role {
            number: 0,
            authority: authority_value(
                500,
                Box::new([authority::Executor::Charter(1), authority::Executor::Procedure(1)]),
            ),
            period_spend: 500,
            requests: authority::Requests::ALL,
            decides: authority::Proposals::ALL,
        }]),
        requirements: Box::new([]),
    };
    let mut out = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(&mut domain, authority::Event::Policy { project: 1, policy }, &mut out);
    assert_eq!(out.pop(), Some(authority::PolicyFact::Added { project: 1 }), "real policy admitted");
    let mut chat_authority = authority_value(100, Box::new([authority::Executor::Procedure(1)]));
    chat_authority.delegation.tasks = 1;
    chat_authority.delegation.depth = 1;
    let mut first = jig_test_connector_world::config(1);
    first.deployment = [31; 16];
    let mut second = jig_test_connector_world::config(2);
    second.deployment = [31; 16];
    first.procedures = Box::new([jig_test_connector::ProcedureSpec {
        number: 1,
        actions: Box::new([jig_test_connector::ProcedureAction::Finish {
            result: jig_test_connector::ProcedureResult::Succeeded,
        }]),
        max_steps: 1,
        stall: Duration::from_secs(1),
    }]);
    root::Config {
        first,
        second,
        core: core::Config {
            deployment: [31; 16],
            seed,
            owners: Box::new([people::InitialOwner {
                project: 1,
                identity: people::IdentityKey { provider: 0, subject: 7_u64.to_be_bytes().into() },
            }]),
            authority: domain,
            projects: {
                let mut projects = skein_lib::List::with_capacity(1);
                projects.push(1).expect("one project");
                projects
            },
            permission_roles: {
                let mut projects: skein_lib::Map<u32, Box<[people::PermissionRole]>> = skein_lib::Map::with_capacity(1);
                projects
                    .insert(
                        1,
                        Box::new([
                            people::PermissionRole { connector: 1, permission: 3, role: 1 },
                            people::PermissionRole { connector: 1, permission: 2, role: 2 },
                            people::PermissionRole { connector: 1, permission: 1, role: 3 },
                        ]),
                    )
                    .expect("one configured permission policy");
                projects
            },
            connectors: Box::new([1, 2]),
            settings: core::Settings {
                deployment_provider: 3,
                recurring_connector: 1,
                charter: 1,
                run: core::RunPolicy {
                    instructions: b"Complete the task within its authority.".as_slice().into(),
                    waiting: Duration::from_secs(1),
                    resume: true,
                    turns: 8,
                    time: Duration::from_secs(60),
                    model: core::Model {
                        dialect: 1,
                        account: 1,
                        endpoint: 1,
                        name: b"scripted".as_slice().into(),
                        max_tokens: 1024,
                        input_price: 1,
                        cached_price: 1,
                        output_price: 1,
                        price_unit: 1000,
                    },
                    alternatives: Box::new([]),
                    inspect: true,
                    modify: true,
                    shell: true,
                    agents: true,
                    call_timeout: Duration::from_secs(10),
                },
                resume_bytes: 256,
                period: 1,
                period_budget: 1000,
                person_budget: 500,
                chat_authority,
                tools: core::ToolFamilies::standard(),
                account: 1,
                account_generation: 1,
                account_valid: Some(Duration::from_secs(60)),
            },
        },
    }
}

/// A scripted party and worker watching only the root's outputs and durable
/// store rows. The core and connectors remain inside the testing root.
#[derive(Debug)]
pub struct World {
    /// The application under test.
    pub domain: root::Domain,
    /// The ordered fake store.
    pub store: Store,
    /// External events not yet delivered.
    pub events: VecDeque<root::Event>,
    /// Every output observed from outside the root.
    pub trace: Vec<String>,
    /// Sign-in number issued to the scripted party.
    pub sign_in: Option<u64>,
    /// Task issued for the party's chat.
    pub chat: Option<u64>,
    /// Current worker assignment.
    pub assignment: Option<root::Assignment>,
    /// Count of accepted turns the worker may forget.
    pub turn_acks: u32,
    /// Count of terminal answers the worker may forget.
    pub answer_acks: u32,
    /// Count of results delivered to the party.
    pub results: u32,
    /// Goal's delegated procedure, if this script requested one.
    pub procedure: Option<u64>,
    /// Refused named batches delivered to the calling worker.
    pub batch_refusals: u32,
    scenario: Scenario,
    delegated: bool,
    goal_answer_sent: bool,
    limits: root::Limits,
    iteration: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Chat,
    Goal,
    Batch,
}

impl World {
    /// Start a seeded application, including its two connector kinds and one worker.
    #[must_use]
    pub fn new(seed: u64) -> World {
        World::with_scenario(seed, Scenario::Chat)
    }

    /// One person asks for a tracked goal, whose run delegates a procedure.
    #[must_use]
    pub fn goal(seed: u64) -> World {
        World::with_scenario(seed, Scenario::Goal)
    }

    /// One worker tries a batch larger than its delegated authority.
    #[must_use]
    pub fn batch(seed: u64) -> World {
        World::with_scenario(seed, Scenario::Batch)
    }

    fn with_scenario(seed: u64, scenario: Scenario) -> World {
        let limits = limits();
        let mut events = VecDeque::new();
        for connector in [1, 2] {
            events.push_back(root::Event::Core(core::Event::Tasks(tasks::Event::Kinds {
                connector,
                kinds: Box::new([tasks::Kind {
                    connector,
                    kind: 1,
                    hold: tasks::HoldKind::Exclusive { taken: tasks::Taken::Waits },
                }]),
            })));
        }
        events.push_back(root::Event::Core(core::Event::People(people::Event::Roles {
            project: 1,
            holdings: Box::new([]),
        })));
        events.push_back(root::Event::Core(core::Event::People(people::Event::Restored)));
        events.push_back(root::Event::Core(core::Event::Tasks(tasks::Event::Restored)));
        events.push_back(root::Event::Core(core::Event::Fleet(fleet::Event::Loaded)));
        events.push_back(root::Event::Core(core::Event::Account(accounts::Event::Add {
            account: 1,
            generation: 1,
            valid: Some(Duration::from_secs(60)),
        })));
        events.push_back(root::Event::Core(core::Event::Fleet(fleet::Event::Hello {
            channel: Token::new(7),
            hello: fleet::Hello {
                stop_bound: Duration::from_millis(100),
                slots: 1,
                workstreams: Box::new([]),
                hosting: Box::new([]),
            },
        })));
        events.push_back(root::Event::Core(core::Event::SignIn {
            reply_to: ReplyTo::new(Token::new(101)),
            identity: people::Identity {
                key: people::IdentityKey { provider: 0, subject: 7_u64.to_be_bytes().into() },
                login: b"person".as_slice().into(),
                name: b"Person".as_slice().into(),
            },
        }));
        World {
            domain: root::Domain::new(config(seed), &limits),
            store: Store::new(),
            events,
            trace: Vec::new(),
            sign_in: None,
            chat: None,
            assignment: None,
            turn_acks: 0,
            answer_acks: 0,
            results: 0,
            procedure: None,
            batch_refusals: 0,
            scenario,
            delegated: false,
            goal_answer_sent: false,
            limits,
            iteration: 0,
        }
    }

    /// Advance one outside event, one commit and all ready deliveries.
    pub fn iterate(&mut self) {
        self.iteration += 1;
        let now = Time::from_nanos(self.iteration * 1_000_000);
        let env = Env { now, wall: Wall::from_nanos(self.iteration * 1_000_000), limits: self.limits };
        if let Some(event) = self.events.pop_front() {
            self.trace.push(format!("input {event:?}"));
            root::step(&mut self.domain, &env, event);
        }
        let mut out = Queue::with_capacity(128);
        root::release(&mut self.domain, &env, &mut out);
        while let Some(request) = out.pop() {
            self.trace.push(format!("output {request:?}"));
            match request {
                root::Request::Commit { number, mut writes } => {
                    let mut rows = Vec::new();
                    while let Some(write) = writes.pop() {
                        rows.push(write);
                    }
                    self.store.submit(number, crate::store_writes(rows), 0);
                    let applied = self.store.tick(false).expect("store did not fail").expect("ready commit");
                    self.events.push_front(root::Event::Committed { number: applied });
                }
                root::Request::Deliver(delivery) => self.delivered(delivery),
                root::Request::Now(core::Now::Account(accounts::Request::Refresh { account, generation })) => {
                    self.events.push_back(root::Event::Core(core::Event::Account(accounts::Event::Refreshed {
                        account,
                        generation,
                        valid: Duration::from_secs(60),
                    })));
                }
                root::Request::Now(_) => {}
                root::Request::Restart(_) => panic!("walking fixture uses restored markers"),
                root::Request::Stop => panic!("testing root stopped: {:?}", self.trace),
            }
        }
        self.domain.reclaim();
        if self.scenario == Scenario::Goal
            && self.delegated
            && !self.goal_answer_sent
            && let Some(procedure) = self.procedure
            && self.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(procedure))))
        {
            let assignment = self.assignment.as_ref().expect("goal worker was assigned");
            self.goal_answer_sent = true;
            self.events.push_back(root::Event::Turn {
                channel: Token::new(7),
                task: assignment.task,
                attempt: assignment.attempt,
                turn: 1,
                read: None,
                cumulative: 3,
                transcript: b"procedure finished".as_slice().into(),
            });
        }
    }

    #[expect(clippy::too_many_lines, reason = "one script handles each externally observed delivery")]
    fn delivered(&mut self, delivery: root::Delivery) {
        match delivery {
            root::Delivery::Restart(_) => panic!("walking fixture uses the restored-marker path"),
            root::Delivery::Core(core::Held::NotesLoad { owner, range }) => {
                let mut rows = skein_lib::List::with_capacity(self.limits.core.notes.load_rows);
                let mut more = false;
                for record in self.store.rows.values() {
                    let root::Record::Core(core::Record::Notes(record)) = record else { continue };
                    let matches = match (&range, record) {
                        (jig_core_notes::Range::Entry { name }, jig_core_notes::Record::Entry(entry)) => {
                            *name == entry.name
                        }
                        (jig_core_notes::Range::Lines { scope, after }, jig_core_notes::Record::Line(line)) => {
                            *scope == line.scope && after.is_none_or(|cursor| line.name > cursor)
                        }
                        _ => false,
                    };
                    if matches && rows.push(record.clone()).is_err() {
                        more = true;
                    }
                }
                self.events.push_back(root::Event::Core(core::Event::Notes(jig_core_notes::Event::Loaded {
                    owner,
                    rows: jig_core_notes::Rows { records: rows },
                    more,
                })));
            }
            root::Delivery::Core(core::Held::PeopleReply {
                sign_in: Some(sign_in),
                reply: people::Reply::SignedIn { .. },
                ..
            }) => {
                self.sign_in = Some(sign_in);
                let ask = match self.scenario {
                    Scenario::Chat => people::Ask::StartChat { project: 1, words: b"hello".as_slice().into() },
                    Scenario::Goal | Scenario::Batch => people::Ask::SetGoal {
                        project: 1,
                        spec: b"complete procedure".as_slice().into(),
                        charter: 1,
                        budget: 100,
                        priority: 1,
                    },
                };
                self.events.push_back(root::Event::Core(core::Event::People(people::Event::Ask {
                    reply_to: ReplyTo::new(Token::new(102)),
                    sign_in,
                    key: [5; 16],
                    ask,
                })));
            }
            root::Delivery::Core(core::Held::PeopleReply {
                reply: people::Reply::Outcome(people::Outcome::Started { task }),
                ..
            }) => {
                self.chat = Some(task);
                assert!(
                    self.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))),
                    "party saw task only after commit"
                );
            }
            root::Delivery::Core(core::Held::PeopleReply {
                reply: people::Reply::Outcome(people::Outcome::GoalStarted { task }),
                ..
            }) => {
                self.chat = Some(task);
                assert!(self.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))));
            }
            root::Delivery::Assigned { channel, assignment } => {
                assert_eq!(channel, Token::new(7));
                assert!(
                    self.store
                        .rows
                        .contains_key(&root::Key::Core(core::Key::Core(core::CoreKey::RunProof(assignment.task))))
                );
                let task = assignment.task;
                let attempt = assignment.attempt;
                self.assignment = Some(assignment);
                match self.scenario {
                    Scenario::Chat => self.events.push_back(root::Event::Turn {
                        channel,
                        task,
                        attempt,
                        turn: 1,
                        read: None,
                        cumulative: 3,
                        transcript: b"answer".as_slice().into(),
                    }),
                    Scenario::Goal | Scenario::Batch => {
                        let authority = tasks::Authority {
                            tools: tasks::Tools(0),
                            grants: Box::new([]),
                            delegation: tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 },
                            budget: tasks::Budget { spend: 0, deadline: None },
                            notes: tasks::Scopes(0),
                            note_resources: Box::new([]),
                        };
                        self.delegated = true;
                        let delegate = core::Delegate {
                            executor: tasks::Executor::Procedure { connector: 1, code: 1 },
                            spec: tasks::Spec {
                                words: b"run the check".as_slice().into(),
                                parameters: Box::new([]),
                                inputs: Box::new([]),
                            },
                            contract: tasks::Contract::Report { words: 0 },
                            authority,
                            symbolic_grants: Box::new([]),
                            dependencies: Box::new([]),
                            wake: tasks::WakePolicy::DEFAULT,
                        };
                        let batch = match self.scenario {
                            Scenario::Goal => vec![delegate].into_boxed_slice(),
                            Scenario::Batch => vec![delegate.clone(), delegate].into_boxed_slice(),
                            Scenario::Chat => unreachable!("chat does not delegate"),
                        };
                        self.events.push_back(root::Event::Core(core::Event::DelegateValidated {
                            to: ReplyTo::new(Token::new(104)),
                            key: core::CallKey { task, attempt, completion: 1, position: 0 },
                            batch,
                            stubs: Box::new([]),
                        }));
                    }
                }
            }
            root::Delivery::Procedure { task, connector, code, .. } => {
                self.procedure = Some(task);
                self.events.push_back(root::Event::Connector {
                    number: connector,
                    event: jig_test_connector::Event::Procedure {
                        task,
                        number: code,
                        resource: jig_test_connector_world::path(1, 1),
                        signal: jig_test_connector::ProcedureSignal::Activate,
                    },
                });
            }
            root::Delivery::Core(core::Held::CallAnswer { part: core::CallPart::Delegated(tasks), .. }) => {
                assert_eq!(tasks.len(), 1);
                self.procedure = Some(tasks[0]);
            }
            root::Delivery::Core(core::Held::CallAnswer {
                part: core::CallPart::DelegationDenied { findings, .. },
                ..
            }) => {
                assert_eq!(self.scenario, Scenario::Batch);
                assert!(!findings.is_empty(), "worker receives a reason for the atomic refusal");
                self.batch_refusals += 1;
            }
            root::Delivery::Core(core::Held::CallAnswer {
                part: core::CallPart::DelegationRefused(problem), ..
            }) => {
                assert_eq!(self.scenario, Scenario::Batch);
                assert_eq!(problem.why, tasks::Refusal::Batch);
                self.batch_refusals += 1;
            }
            root::Delivery::Core(core::Held::Inbound { channel, run, attempt, word }) => {
                assert_eq!(word.from, tasks::Party::Task(self.procedure.expect("delegated child")));
                self.events.push_back(root::Event::Answer {
                    channel,
                    task: run.raw(),
                    attempt: attempt.raw(),
                    cumulative: 3,
                    end: tasks::End::Finished {
                        result: tasks::TaskResult::Report { words: b"done".as_slice().into() },
                        cancel_delegates: false,
                    },
                });
            }
            root::Delivery::Core(core::Held::AcknowledgeTurn { channel, run, attempt, turn }) => {
                assert_eq!(channel, Token::new(7));
                assert!(
                    self.store.rows.contains_key(&root::Key::Core(core::Key::Core(core::CoreKey::Turn {
                        task: run.raw(),
                        attempt: attempt.raw(),
                        turn,
                    }))),
                    "worker saw ACK only after turn committed"
                );
                self.turn_acks += 1;
                self.events.push_back(root::Event::Answer {
                    channel,
                    task: run.raw(),
                    attempt: attempt.raw(),
                    cumulative: 3,
                    end: tasks::End::Finished {
                        result: tasks::TaskResult::Report { words: b"done".as_slice().into() },
                        cancel_delegates: false,
                    },
                });
            }
            root::Delivery::Core(core::Held::Acknowledge { .. }) => self.answer_acks += 1,
            root::Delivery::Core(core::Held::Result { task, words, .. }) => {
                assert!(self.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(task)))));
                assert_eq!(words.as_ref(), b"done");
                self.results += 1;
            }
            root::Delivery::Core(
                core::Held::ViewStart { .. }
                | core::Held::ViewFinished { .. }
                | core::Held::ViewTaskPhase { .. }
                | core::Held::ViewTurn { .. },
            ) => {}
            other @ (root::Delivery::TypedAnswer { .. }
            | root::Delivery::Core(_)
            | root::Delivery::Fleet(_)
            | root::Delivery::System { .. }) => {
                panic!("unhandled walking delivery {other:?}: {:?}", self.trace)
            }
        }
    }

    /// Run until the worker and party have seen the terminal, within a fixed bound.
    pub fn run_chat(&mut self) {
        for _ in 0..300 {
            if self.results == 1 && self.answer_acks == 1 && self.events.is_empty() && self.domain.quiescent() {
                return;
            }
            self.iterate();
        }
        panic!("chat did not settle: {:?}", self.trace);
    }

    /// Run the goal, delegated procedure and person's final result.
    pub fn run_goal(&mut self) {
        for _ in 0..300 {
            if self.results == 1 && self.procedure.is_some() && self.events.is_empty() && self.domain.quiescent() {
                return;
            }
            self.iterate();
        }
        panic!("goal did not settle: {:?}", self.trace);
    }

    /// Run through the named batch refusal and its durable reply.
    pub fn run_batch(&mut self) {
        for _ in 0..300 {
            if self.batch_refusals == 1 && self.events.is_empty() && self.domain.quiescent() {
                return;
            }
            self.iterate();
        }
        panic!("batch did not settle: {:?}", self.trace);
    }
}
