//! Translations between the test root and independent scripted peers. The
//! workers and parties retain their own observations; this world reads only
//! released root outputs and durable store rows (`domain/testing.md`, 4, 7).
use crate::Store;
use jig_core as core;
use jig_core_fleet as fleet;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_fake_parties as parties;
use jig_fake_workers as workers;
use jig_test_domain as root;
use skein_lib::{Duration, ReplyTo, Time, Token};
use std::collections::BTreeMap;

type OpaqueName = (u64, u64, Box<[u8]>);

/// Scripted peers and the ordinary world's correlation ledger.
#[derive(Debug)]
pub struct Peers {
    pub workers: Vec<workers::Worker>,
    pub parties: Vec<parties::Party>,
    pub upstream: Vec<workers::Up>,
    pub requests: Vec<parties::Up>,
    party_flights: BTreeMap<u64, usize>,
    call_hosts: BTreeMap<core::CallKey, (u64, u32)>,
    opaque: BTreeMap<OpaqueName, (u64, u32)>,
}

impl Peers {
    #[must_use]
    pub fn new(workers: Vec<workers::Worker>, parties: Vec<parties::Party>) -> Self {
        Self {
            workers,
            parties,
            upstream: Vec::new(),
            requests: Vec::new(),
            party_flights: BTreeMap::new(),
            call_hosts: BTreeMap::new(),
            opaque: BTreeMap::new(),
        }
    }

    /// Translate one host report without reading a domain's state.
    pub fn worker_event(&mut self, up: workers::Up) -> root::Event {
        self.upstream.push(up.clone());
        match up {
            workers::Up::Hello { channel, slots, stop_bound, hosting } => {
                root::Event::Core(core::Event::Fleet(fleet::Event::Hello {
                    channel: Token::new(channel),
                    hello: fleet::Hello {
                        slots,
                        stop_bound,
                        workstreams: Box::new([]),
                        hosting: hosting
                            .into_iter()
                            .map(|(task, attempt, answered)| fleet::Hosted {
                                run: Token::new(task),
                                attempt: Token::new(attempt),
                                phase: if answered { fleet::Phase::Answered } else { fleet::Phase::Active },
                            })
                            .collect(),
                    },
                }))
            }
            workers::Up::Lost { channel } => {
                root::Event::Core(core::Event::Fleet(fleet::Event::Lost { channel: Token::new(channel) }))
            }
            workers::Up::Turn { channel, task, attempt, turn, spent, read, body } => root::Event::Turn {
                channel: Token::new(channel),
                task,
                attempt,
                turn,
                cumulative: spent,
                read,
                transcript: body,
            },
            workers::Up::Answer { channel, task, attempt, spent, end } => root::Event::Answer {
                channel: Token::new(channel),
                task,
                attempt,
                cumulative: spent,
                end: end_value(end),
            },
            workers::Up::Call { channel, task, attempt, number, call } => {
                let key = core::CallKey { task, attempt, completion: number, position: 0 };
                self.call_hosts.insert(key, (channel, number));
                let to = ReplyTo::new(Token::new(channel * 100_000 + task * 100 + u64::from(number)));
                match call {
                    workers::Call::Opaque { name, tool, input, writes } => {
                        self.opaque.insert((task, attempt, name.clone()), (channel, number));
                        root::Event::Core(core::Event::Fleet(fleet::Event::RelayTyped {
                            channel: Token::new(channel),
                            run: Token::new(task),
                            attempt: Token::new(attempt),
                            call: fleet::TypedCall { name, tool, input, writes, deadline: Duration::from_secs(1) },
                        }))
                    }
                    workers::Call::ProposeGoal { words, budget, priority } => {
                        root::Event::Core(core::Event::NamedAction {
                            to,
                            key,
                            action: core::NamedAction::ProposeGoal {
                                goal: Box::new(delegate(words, tasks::Executor::Agent { charter: 1 }, budget)),
                                priority,
                                reason: b"this goal needs a holder".as_slice().into(),
                            },
                        })
                    }
                    workers::Call::Choose { role, first, second } => {
                        let mut words = first.into_vec();
                        words.extend_from_slice(b" or ");
                        words.extend_from_slice(&second);
                        let mut member = delegate(
                            words.into_boxed_slice(),
                            tasks::Executor::Person(tasks::PersonAddress::Role(role)),
                            0,
                        );
                        member.authority = authority(0, false);
                        member.contract = tasks::Contract::Verdict {
                            choices: Box::new([
                                tasks::Verdict { code: 1, words: 64, followups: 0 },
                                tasks::Verdict { code: 2, words: 64, followups: 0 },
                            ]),
                        };
                        root::Event::Core(core::Event::DelegateValidated {
                            to,
                            key,
                            batch: Box::new([member]),
                            stubs: Box::new([]),
                        })
                    }
                }
            }
        }
    }

    /// Scripts progress only from independently received outputs and store rows.
    pub fn tick(&mut self, store: &Store, now: Time) -> Vec<root::Event> {
        let reports: Vec<_> = self.workers.iter_mut().flat_map(|worker| worker.tick(now)).collect();
        let mut events: Vec<_> = reports.into_iter().map(|up| self.worker_event(up)).collect();
        for index in 0..self.parties.len() {
            let entry = waiting(store, &self.parties[index]);
            self.parties[index].waiting(entry);
            if let Some(up) = self.parties[index].tick(now) {
                events.push(self.party_event(index, up));
            }
        }
        events
    }

    fn party_event(&mut self, index: usize, up: parties::Up) -> root::Event {
        self.requests.push(up.clone());
        match up {
            parties::Up::SignIn { to, provider, subject } => {
                assert!(self.party_flights.insert(to, index).is_none());
                root::Event::Core(core::Event::SignIn {
                    reply_to: ReplyTo::new(Token::new(to)),
                    identity: people::Identity {
                        key: people::IdentityKey { provider, subject },
                        login: b"person".as_slice().into(),
                        name: b"Person".as_slice().into(),
                    },
                })
            }
            parties::Up::Request { to, sign_in, project, key, ask } => {
                assert!(self.party_flights.insert(to, index).is_none());
                let ask = match ask {
                    parties::Ask::Chat { words } => people::Ask::StartChat { project, words },
                    parties::Ask::Goal { words, budget, priority } => {
                        people::Ask::SetGoal { project, spec: words, charter: 1, budget, priority }
                    }
                    parties::Ask::Say { task, words } => people::Ask::Say { project, task: task_number(task), words },
                    parties::Ask::Stop { task } => people::Ask::Stop { project, task: task_number(task) },
                    parties::Ask::Release { task } => people::Ask::Release { project, task: task_number(task) },
                    parties::Ask::Take { task } => people::Ask::TakePerson { project, task: task_number(task) },
                    parties::Ask::Choose { task, code, words } => people::Ask::AnswerPerson {
                        project,
                        task: task_number(task),
                        result: people::PersonResult::Verdict { code, words },
                    },
                    parties::Ask::Decide { .. } => panic!("party resolves its visible proposal"),
                    parties::Ask::Decision { proposer, proposal, accept } => people::Ask::DecideProposal {
                        project,
                        proposer,
                        proposal,
                        decision: if accept {
                            people::ProposalDecision::Accept
                        } else {
                            people::ProposalDecision::Reject { reason: b"not now".as_slice().into() }
                        },
                    },
                };
                root::Event::Core(core::Event::People(people::Event::Ask {
                    reply_to: ReplyTo::new(Token::new(to)),
                    sign_in,
                    key,
                    ask,
                }))
            }
        }
    }

    /// Deliver post-commit observations to the peer that owns the flight.
    pub fn delivered(&mut self, delivery: &root::Delivery) {
        match delivery {
            root::Delivery::Assigned { channel, assignment } => {
                self.worker(channel.raw()).assign(workers::Assignment {
                    task: assignment.task,
                    attempt: assignment.attempt,
                    budget: assignment.run.budget,
                    transcript: assignment.transcript.clone(),
                });
            }
            root::Delivery::Core(core::Held::AcknowledgeTurn { channel, run, attempt, turn }) => {
                self.worker(channel.raw()).acknowledge_turn(run.raw(), attempt.raw(), *turn);
            }
            root::Delivery::Core(core::Held::Acknowledge { channel, run, attempt }) => {
                self.worker(channel.raw()).acknowledge_answer(run.raw(), attempt.raw());
            }
            root::Delivery::Core(core::Held::Cancel { channel, run, attempt }) => {
                self.worker(channel.raw()).cancel(run.raw(), attempt.raw());
            }
            root::Delivery::Core(core::Held::Inbound { channel, run, attempt, word }) => {
                self.worker(channel.raw()).message(run.raw(), attempt.raw(), word.number, word.words.clone());
            }
            root::Delivery::Core(core::Held::CallAnswer { key, part, .. }) => {
                if let Some((channel, number)) = self.call_hosts.remove(key) {
                    let bytes = format!("{part:?}").into_bytes().into_boxed_slice();
                    self.worker(channel).call_answer(key.task, key.attempt, number, bytes);
                }
            }
            root::Delivery::TypedAnswer { task, attempt, name, call, .. } => {
                if let Some((channel, number)) = self.opaque.remove(&(*task, *attempt, name.clone())) {
                    self.worker(channel).call_answer(
                        *task,
                        *attempt,
                        number,
                        match &call.answer {
                            core::SettledAnswer::Host { body, .. } => body.clone(),
                            core::SettledAnswer::Delivery { evidence, .. } => evidence.clone(),
                        },
                    );
                }
            }
            root::Delivery::Restart(_)
            | root::Delivery::Core(_)
            | root::Delivery::Fleet(_)
            | root::Delivery::System { .. }
            | root::Delivery::Procedure { .. } => {}
        }
    }

    pub fn reply(&mut self, to: ReplyTo, sign_in: Option<u64>, reply: people::Reply) {
        let number = to.into_token().raw();
        if let Some(index) = self.party_flights.remove(&number) {
            self.parties[index].reply(number, party_reply(reply, sign_in));
        }
    }

    fn worker(&mut self, channel: u64) -> &mut workers::Worker {
        self.workers.iter_mut().find(|worker| worker.channel == channel).expect("configured host")
    }

    /// The test root's decoder answers the opaque echo tool in a durable decision.
    pub fn opaque_answer(
        &mut self,
        to: ReplyTo,
        run: Token,
        attempt: Token,
        call: &fleet::TypedCall,
    ) -> Vec<root::Event> {
        let number = self.opaque.get(&(run.raw(), attempt.raw(), call.name.clone())).expect("retained opaque call").1;
        let key = core::CallKey { task: run.raw(), attempt: attempt.raw(), completion: number, position: 0 };
        self.call_hosts.remove(&key);
        let token = to.into_token();
        vec![
            root::Event::Core(core::Event::NamedAnswer {
                to: ReplyTo::new(token),
                key,
                part: core::CallPart::Unavailable,
            }),
            root::Event::Core(core::Event::SettledCall {
                to: ReplyTo::new(token),
                key,
                call: core::SettledCall {
                    serial: 0,
                    name: call.name.clone(),
                    tool: call.tool.clone(),
                    answer: core::SettledAnswer::Host { error: false, body: call.input.clone() },
                },
            }),
        ]
    }

    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.workers.iter().all(workers::Worker::quiescent) && self.parties.iter().all(parties::Party::quiescent)
    }
}

fn task_number(task: parties::Task) -> u64 {
    match task {
        parties::Task::Number(number) => number,
        parties::Task::Last | parties::Task::Waiting => panic!("party resolved its task reference"),
    }
}

fn party_reply(reply: people::Reply, sign_in: Option<u64>) -> parties::Reply {
    match reply {
        people::Reply::SignedIn { person, .. } => {
            parties::Reply::SignedIn { person, sign_in: sign_in.expect("new sign-in number") }
        }
        people::Reply::Outcome(people::Outcome::Started { task } | people::Outcome::GoalStarted { task }) => {
            parties::Reply::Started { task }
        }
        people::Reply::Outcome(people::Outcome::GoalProposed { proposal }) => parties::Reply::Proposed { proposal },
        people::Reply::Outcome(people::Outcome::Stopped { task } | people::Outcome::Released { task }) => {
            parties::Reply::Changed { task }
        }
        people::Reply::Refused(_) | people::Reply::Outcome(people::Outcome::Refused(_)) => parties::Reply::Refused,
        people::Reply::SignedOut | people::Reply::Outcome(_) => parties::Reply::Done,
    }
}

fn waiting(store: &Store, party: &parties::Party) -> Option<parties::Waiting> {
    let person = party.person?;
    for record in store.rows.values() {
        let root::Record::Core(core::Record::Tasks(tasks::Stored::Live(task))) = record else { continue };
        if let Some(proposal) = &task.proposal {
            let visible = match proposal.state {
                tasks::ProposalState::Pending { holder: tasks::ProposalHolder::Person(holder), .. } => holder == person,
                tasks::ProposalState::Pending { holder: tasks::ProposalHolder::Policy { .. }, .. } => {
                    matches!(party.role, parties::Role::Owner | parties::Role::Maintainer)
                }
                tasks::ProposalState::Pending { .. }
                | tasks::ProposalState::Accepted { .. }
                | tasks::ProposalState::Rejected { .. }
                | tasks::ProposalState::Withdrawn => false,
            };
            if visible {
                return Some(parties::Waiting::Proposal { proposer: task.number, proposal: proposal.number });
            }
        }
        let visible = match task.executor {
            tasks::Executor::Person(tasks::PersonAddress::Person(holder)) => holder == person,
            tasks::Executor::Person(tasks::PersonAddress::Role(role)) => role == role_number(party.role),
            tasks::Executor::Agent { .. } | tasks::Executor::Procedure { .. } => false,
        };
        if visible
            && task.taken_by.is_none_or(|holder| holder == person)
            && matches!(task.phase, tasks::Phase::Active(_))
        {
            return Some(parties::Waiting::Choice { task: task.number });
        }
    }
    None
}

fn role_number(role: parties::Role) -> u32 {
    match role {
        parties::Role::Owner => 0,
        parties::Role::Maintainer => 1,
        parties::Role::Member => 2,
        parties::Role::Observer => 3,
    }
}

fn authority(budget: u64, agent: bool) -> tasks::Authority {
    tasks::Authority {
        tools: tasks::Tools(if agent { 1023 } else { 0 }),
        grants: Box::new([]),
        delegation: tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 },
        budget: tasks::Budget { spend: budget, deadline: None },
        notes: tasks::Scopes(0),
        note_resources: Box::new([]),
    }
}

fn delegate(words: Box<[u8]>, executor: tasks::Executor, budget: u64) -> core::Delegate {
    core::Delegate {
        executor,
        spec: tasks::Spec { words, parameters: Box::new([]), inputs: Box::new([]) },
        contract: tasks::Contract::Report { words: 128 },
        authority: authority(budget, true),
        symbolic_grants: Box::new([]),
        dependencies: Box::new([]),
        wake: tasks::WakePolicy::DEFAULT,
    }
}

fn end_value(end: workers::End) -> tasks::End {
    match end {
        workers::End::Finished(words) => {
            tasks::End::Finished { result: tasks::TaskResult::Report { words }, cancel_delegates: false }
        }
        workers::End::Parked => tasks::End::Parked,
        workers::End::Failed(why) => tasks::End::Failed(match why {
            workers::Failure::Transient => tasks::Class::Transient,
            workers::Failure::Permanent => tasks::Class::Permanent,
            workers::Failure::Run => tasks::Class::Run,
            workers::Failure::Agent => tasks::Class::Agent,
            workers::Failure::Lost => tasks::Class::Lost,
            workers::Failure::Invalid => tasks::Class::Invalid,
        }),
    }
}

/// Tiny limits with enough roles, hosts and person verdicts for the peer stories.
#[must_use]
pub fn fixture(seed: u64, engine_slots: u32) -> (root::Config, root::Limits) {
    use jig_core_authority as policy;
    let (mut configuration, mut limits) = crate::effects::fixture(seed, false);
    limits.core.tasks.tasks = 5;
    limits.core.tasks.project_tasks = 5;
    limits.core.tasks.tree_tasks = 5;
    limits.core.tasks.depth = 2;
    limits.core.tasks.delegates = 3;
    limits.core.tasks.funders = 16;
    limits.core.tasks.executor_kinds = 3;
    limits.core.tasks.contract_choices = 2;
    limits.core.people.people = 4;
    limits.core.people.sign_ins = 4;
    limits.core.people.holdings = 4;
    limits.core.people.inbox_entries = 4;
    limits.core.people.requests = 32;
    limits.core.authority.roles = 4;
    limits.core.authority.executors = 3;
    limits.core.fleet.workers = 3;
    limits.core.fleet.slots = 1;
    limits.core.fleet.engine_slots = engine_slots;
    limits.core.fleet.attempts = 5;
    limits.core.fleet.calls = 5;
    limits.core.brief.briefs = 5;
    limits.core.views.runs = 5;
    limits.core.call_records = 16;
    limits.journal.writes = 10_000;
    limits.journal.held = 10_000;
    configuration.hosting = fleet::Kinds::Both;
    let kinds = Box::new([policy::Executor::Charter(1), policy::Executor::Procedure(1), policy::Executor::Role(1)]);
    configuration.core.settings.chat_authority.delegation =
        policy::Delegation { kinds: kinds.clone(), tasks: 3, depth: 2 };
    configuration.core.settings.chat_authority.budget.spend = 40;
    let mut rules = configuration.core.authority.rules().clone();
    rules.ceiling.delegation = policy::Delegation { kinds, tasks: 6, depth: 3 };
    rules.maximum_run_spend = 20;
    let mut project = configuration.core.authority.policy(1).expect("peer policy").clone();
    project.ceiling = rules.ceiling.clone();
    let mut owner = project.roles[0].clone();
    owner.authority.delegation = rules.ceiling.delegation.clone();
    let mut maintainer = owner.clone();
    maintainer.number = 1;
    let mut member = owner.clone();
    member.number = 2;
    member.authority.budget.spend = 50;
    member.decides = policy::Proposals(0);
    let mut observer = owner.clone();
    observer.number = 3;
    observer.requests = policy::Requests(128);
    observer.decides = policy::Proposals(0);
    project.roles = Box::new([owner, maintainer, member, observer]);
    let mut checked = policy::Domain::new(rules, limits.core.authority).expect("peer policy limits");
    let mut out = skein_lib::Queue::with_capacity(policy::POLICY_MAX_OUT);
    policy::step(&mut checked, policy::Event::Policy { project: 1, policy: project }, &mut out);
    assert_eq!(out.pop(), Some(policy::PolicyFact::Added { project: 1 }));
    configuration.core.authority = checked;
    (configuration, limits)
}
