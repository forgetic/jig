//! Durable parent and scripted executors at the retained tasks boundary
//! (domain/tasks.md, 11; domain/engine.md, 7).
use crate::referee::{Seen, Stimulus, Tasks};
use jig_core_tasks::{
    self as tasks, Accepted, Authority, Budget, Cause, Contract, Delegation, Domain, End, Ending, Event, Executor,
    Fact, Funder, Key, Limits, MessageKind, New, Numbers, Party, Problem, Request, ResultKind, Retries, Retry,
    RunContext, Scopes, Spec, Stored, TaskResult, Tools, Word,
};
use skein_lib::{Duration, Env, Queue, ReplyTo, Rng, Time, Token, Wall};
use skein_world::domain::{Referee, Trace};
use std::collections::{BTreeMap, BTreeSet};

pub const RETRY: Retry = Retry { retries: 2, base: Duration::from_millis(10), max: Duration::from_secs(1) };

pub const LIMITS: Limits = Limits {
    tasks: 16,
    funders: 32,
    project_tasks: 16,
    tree_tasks: 16,
    depth: 4,
    delegates: 8,
    references: 8,
    subscriptions: 8,
    batch: 8,
    dependencies: 8,
    inputs: 4,
    spec_bytes: 64,
    parameters: 4,
    result_bytes: 32,
    inbox_messages: 8,
    inbox_bytes: 128,
    message_bytes: 32,
    proposal_stall: Duration::from_millis(10),
    escalation_stall: Duration::from_secs(3600),
    saved_resources: 2,
    contract_choices: 4,
    charters: 2,
    authority_grants: 4,
    authority_segments: 4,
    authority_bytes: 64,
    executor_kinds: 3,
    retries: Retries { transient: RETRY, permanent: RETRY, run: RETRY, agent: RETRY, lost: RETRY, invalid: RETRY },
    facts: 16,
};

#[must_use]
pub fn authority() -> Authority {
    Authority {
        tools: Tools(0),
        grants: Box::new([]),
        delegation: Delegation { kinds: Box::new([]), tasks: 16, depth: 4 },
        budget: Budget { spend: 100, deadline: None },
        notes: Scopes(0),
        note_resources: Box::new([]),
    }
}

#[must_use]
pub fn task(number: u64, dependencies: &[u64]) -> New {
    New {
        number,
        project: 1,
        executor: Executor::Agent { charter: 1 },
        spec: Spec { words: Box::new([1]), parameters: Box::new([]), inputs: Box::new([]) },
        contract: Contract::Report { words: 32 },
        authority: authority(),
        numbers: Numbers { budget: 100, spent: 0, spent_below: 0, reserved: 0 },
        funder: Funder::Period { project: 1, period: 0 },
        dependencies: dependencies.into(),
        wake: tasks::WakePolicy::DEFAULT,
        recurring: None,
        tracked: None,
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply {
    Made(Vec<u64>),
    Done,
    Refused(Problem),
    Acknowledged(Accepted),
    Turn(Accepted),
}

/// Complete frozen run evidence, including internal deterministic state and all
/// parent/executor state; facts are diagnostic only (domain/tasks.md, 11).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frozen {
    /// Durable parent rows (domain/tasks.md, 5).
    pub records: BTreeMap<Key, Stored>,
    /// Committed call replies (domain/tasks.md, 5).
    pub replies: BTreeMap<u64, Reply>,
    /// Pending executor preparations (domain/engine.md, 7).
    pub activations: BTreeSet<u64>,
    /// Owned activation briefs (domain/engine.md, 9).
    pub contexts: BTreeMap<u64, RunContext>,
    /// Physical executor attempts (domain/engine.md, 7).
    pub runs: BTreeMap<u64, u64>,
    /// Requested stops awaiting terminal work (domain/tasks.md, 5.1).
    pub stops: BTreeSet<(u64, u64)>,
    /// Effects settlement gates (domain/tasks.md, 5.1).
    pub closing: BTreeSet<u64>,
    /// Committed requester outcomes (domain/tasks.md, 5.6).
    pub results: BTreeMap<u64, Ending>,
    deadlines: BTreeMap<u64, (Wall, Time)>,
    restoring: bool,
    facts: Vec<Fact>,
    consume_facts: bool,
    limits: Limits,
    seed: u64,
    priced: Option<(u64, u64)>,
    domain: String,
    referee: String,
    accounting: String,
    pending: String,
    now: Time,
    wall: Wall,
    call: u64,
    message: u64,
    commit: u64,
}

#[derive(Debug)]
pub struct World {
    pub env: Env<Limits>,
    domain: Domain,
    out: Queue<Request>,
    pending: Vec<Request>,
    pub records: BTreeMap<Key, Stored>,
    pub replies: BTreeMap<u64, Reply>,
    pub activations: BTreeSet<u64>,
    /// Owned activation brief supplied by the child (domain/engine.md, 9).
    pub contexts: BTreeMap<u64, RunContext>,
    pub runs: BTreeMap<u64, u64>,
    pub stops: BTreeSet<(u64, u64)>,
    pub closing: BTreeSet<u64>,
    pub results: BTreeMap<u64, Ending>,
    pub facts: Vec<Fact>,
    pub accounting_referee: crate::accounting_referee::Accounting,
    pub consume_facts: bool,
    pub referee: Referee<Tasks>,
    pub trace: Trace,
    seed: u64,
    call: u64,
    message: u64,
    commit: u64,
    priced: Option<(u64, u64)>,
    restoring: bool,
    deadlines: BTreeMap<u64, (Wall, Time)>,
}

impl World {
    #[must_use]
    pub fn new(seed: u64, limits: Limits) -> World {
        let mut world = World {
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            domain: Domain::new(&limits, seed, Box::new([1])),
            out: Queue::with_capacity(tasks::max_out(&limits)),
            pending: Vec::new(),
            records: BTreeMap::new(),
            replies: BTreeMap::new(),
            activations: BTreeSet::new(),
            contexts: BTreeMap::new(),
            runs: BTreeMap::new(),
            stops: BTreeSet::new(),
            closing: BTreeSet::new(),
            results: BTreeMap::new(),
            facts: Vec::new(),
            accounting_referee: crate::accounting_referee::Accounting::default(),
            consume_facts: true,
            referee: Referee::new(Tasks::default()),
            trace: Trace::default(),
            seed,
            call: 0,
            message: 0,
            commit: 0,
            priced: None,
            restoring: true,
            deadlines: BTreeMap::new(),
        };
        world.send(Event::Restored);
        world.open_period(0, 100_000);
        world
    }

    #[must_use]
    /// Capture all retained domain, parent and executor state between steps
    /// (domain/tasks.md, 11).
    pub fn frozen(&self) -> Frozen {
        Frozen {
            records: self.records.clone(),
            replies: self.replies.clone(),
            activations: self.activations.clone(),
            contexts: self.contexts.clone(),
            runs: self.runs.clone(),
            stops: self.stops.clone(),
            closing: self.closing.clone(),
            results: self.results.clone(),
            deadlines: self.deadlines.clone(),
            restoring: self.restoring,
            facts: self.facts.clone(),
            consume_facts: self.consume_facts,
            limits: self.env.limits,
            seed: self.seed,
            priced: self.priced,
            domain: format!("{:?}", self.domain),
            referee: format!("{:?}", self.referee),
            accounting: format!("{:?}", self.accounting_referee),
            pending: format!("{:?} {:?}", self.pending, self.out),
            now: self.env.now,
            wall: self.env.wall,
            call: self.call,
            message: self.message,
            commit: self.commit,
        }
    }

    pub fn open_period(&mut self, period: u64, budget: u64) {
        let reply_to = self.to();
        self.send(Event::OpenPeriod { reply_to, project: 1, period, budget });
    }

    pub fn carve_pool(&mut self, period: u64, budget: u64) {
        let reply_to = self.to();
        self.send(Event::CarvePool { reply_to, project: 1, person: 9, period, budget });
    }

    pub fn to(&mut self) -> ReplyTo {
        self.call += 1;
        ReplyTo::new(Token::new(self.call))
    }

    pub fn observe(&mut self, seen: Seen) {
        self.trace.log(self.env.now, format_args!("{seen:?}"));
        let mut stimuli = Vec::new();
        self.referee.observe(self.env.now, seen, &mut stimuli);
        self.referee.assert_holding(self.seed);
        for stimulus in stimuli {
            match stimulus {
                Stimulus::Restart => self.restart(),
            }
        }
    }

    fn route_hint(&mut self, request: &Request) {
        match request {
            Request::Notify { task, subscription, target, state, words } => {
                self.message = self.message.checked_add(1).expect("message number room");
                tasks::step(
                    &mut self.domain,
                    &self.env,
                    Event::Notice {
                        task: *task,
                        word: Word {
                            number: self.message,
                            from: Party::Task(*target),
                            kind: MessageKind::Notice { subscription: *subscription, target: *target, state: *state },
                            words: words.clone(),
                            at: self.env.wall,
                            hits: 1,
                            eligible: false,
                        },
                    },
                    &mut self.out,
                );
            }
            Request::Timer { task, subscription } => {
                self.message = self.message.checked_add(1).expect("message number room");
                tasks::step(
                    &mut self.domain,
                    &self.env,
                    Event::Notice {
                        task: *task,
                        word: Word {
                            number: self.message,
                            from: Party::Task(*task),
                            kind: MessageKind::Timer { subscription: *subscription },
                            words: Box::new([]),
                            at: self.env.wall,
                            hits: 1,
                            eligible: false,
                        },
                    },
                    &mut self.out,
                );
            }
            Request::Sent { .. }
            | Request::RecurringDue { .. }
            | Request::Relay { .. }
            | Request::EscalationsInspected { .. }
            | Request::EscalationsRechecked { .. }
            | Request::EscalationNeeded { .. }
            | Request::EscalationInspected { .. }
            | Request::EscalationDecided { .. }
            | Request::ProposalDecided { .. }
            | Request::PersonProposed { .. }
            | Request::PersonProposalDecided { .. }
            | Request::ProposalRerouteNeeded { .. }
            | Request::ProposalStalled { .. }
            | Request::EscalationStalled { .. }
            | Request::Made { .. }
            | Request::Refused { .. }
            | Request::Done { .. }
            | Request::Acknowledged { .. }
            | Request::TurnAcknowledged { .. }
            | Request::Activate { .. }
            | Request::Stop { .. }
            | Request::Adopt { .. }
            | Request::Close { .. }
            | Request::Release { .. }
            | Request::Ended { .. }
            | Request::Save { .. }
            | Request::Erase { .. }
            | Request::RestoreRefused { .. } => {}
        }
    }

    fn remember_input(&mut self, event: &Event) {
        self.priced = match event {
            Event::Turn { task, cumulative, .. }
            | Event::Activation { task, cause: Cause::Priced { cumulative }, .. } => Some((*task, *cumulative)),
            Event::OpenPeriod { .. }
            | Event::TickRecurring { .. }
            | Event::RecurringBatch { .. }
            | Event::Procedure { .. }
            | Event::WakeProcedure { .. }
            | Event::TakePerson { .. }
            | Event::HandBackPerson { .. }
            | Event::AnswerPerson { .. }
            | Event::Control { .. }
            | Event::Propose { .. }
            | Event::ProposePerson { .. }
            | Event::DecidePersonProposal { .. }
            | Event::DecideProposal { .. }
            | Event::WithdrawProposal { .. }
            | Event::StalledProposal { .. }
            | Event::Amend { .. }
            | Event::Prioritise { .. }
            | Event::Move { .. }
            | Event::CarvePool { .. }
            | Event::ResizePool { .. }
            | Event::Make { .. }
            | Event::Prepare { .. }
            | Event::Claim { .. }
            | Event::Started { .. }
            | Event::Activation { cause: Cause::Unpriced, .. }
            | Event::PreparationFailed { .. }
            | Event::Hold { .. }
            | Event::EffectsSettled { .. }
            | Event::Settled { .. }
            | Event::Restore { .. }
            | Event::Restored
            | Event::InspectEscalations { .. }
            | Event::RecheckEscalations { .. }
            | Event::InspectEscalation { .. }
            | Event::RoutedEscalation { .. }
            | Event::DecideEscalation { .. }
            | Event::Message { .. }
            | Event::Introduce { .. }
            | Event::DelegateResult { .. }
            | Event::RememberStub { .. }
            | Event::Subscribe { .. }
            | Event::SubscribeTopic { .. }
            | Event::Unsubscribe { .. }
            | Event::Notice { .. } => None,
        };
        match event {
            Event::Amend { message, .. } => {
                self.message = self.message.max(*message);
            }
            Event::Message { word, .. } | Event::DelegateResult { word, .. } | Event::Notice { word, .. } => {
                self.message = self.message.max(word.number);
            }
            Event::OpenPeriod { .. }
            | Event::TickRecurring { .. }
            | Event::RecurringBatch { .. }
            | Event::Procedure { .. }
            | Event::WakeProcedure { .. }
            | Event::TakePerson { .. }
            | Event::HandBackPerson { .. }
            | Event::AnswerPerson { .. }
            | Event::Control { .. }
            | Event::Prioritise { .. }
            | Event::Move { .. }
            | Event::Propose { .. }
            | Event::ProposePerson { .. }
            | Event::DecidePersonProposal { .. }
            | Event::DecideProposal { .. }
            | Event::WithdrawProposal { .. }
            | Event::StalledProposal { .. }
            | Event::CarvePool { .. }
            | Event::ResizePool { .. }
            | Event::Make { .. }
            | Event::Prepare { .. }
            | Event::Claim { .. }
            | Event::Started { .. }
            | Event::Activation { .. }
            | Event::PreparationFailed { .. }
            | Event::Hold { .. }
            | Event::EffectsSettled { .. }
            | Event::Settled { .. }
            | Event::Restore { .. }
            | Event::Restored
            | Event::InspectEscalations { .. }
            | Event::RecheckEscalations { .. }
            | Event::InspectEscalation { .. }
            | Event::RoutedEscalation { .. }
            | Event::DecideEscalation { .. }
            | Event::Introduce { .. }
            | Event::RememberStub { .. }
            | Event::Subscribe { .. }
            | Event::SubscribeTopic { .. }
            | Event::Unsubscribe { .. }
            | Event::Turn { .. } => {}
        }
    }

    pub fn stage(&mut self, event: Event) {
        assert!(self.pending.is_empty(), "one parent decision at a time");
        if matches!(event, Event::Restored) {
            self.restoring = false;
        }
        self.remember_input(&event);
        self.trace.log(self.env.now, format_args!("{event:?}"));
        tasks::step(&mut self.domain, &self.env, event, &mut self.out);
        while let Some(request) = self.out.pop() {
            self.route_hint(&request);
            if let Request::EscalationNeeded { context } = &request {
                tasks::step(
                    &mut self.domain,
                    &self.env,
                    Event::RoutedEscalation {
                        task: context.task,
                        revision: context.escalation.revision(),
                        holder: tasks::EscalationHolder::Person(context.requester),
                        entry: 1,
                    },
                    &mut self.out,
                );
            }
            if let Request::Ended { task, requester: Party::Task(parent), ending } = &request {
                let last = self.pending.iter().rev().find_map(|pending| {
                    if let Request::Save { record: Stored::Live(row) } = pending
                        && row.number == *parent
                    {
                        return Some(row.last_message);
                    }
                    None
                });
                let last = last.unwrap_or_else(|| self.record(*parent).last_message);
                let (kind, words) = result_notice(ending.clone());
                self.message = self.message.max(last).checked_add(1).expect("message number room");
                tasks::step(
                    &mut self.domain,
                    &self.env,
                    Event::DelegateResult {
                        task: *parent,
                        word: Word {
                            number: self.message,
                            from: Party::Task(*task),
                            kind: MessageKind::Result(kind),
                            words,
                            at: self.env.wall,
                            hits: 1,
                            eligible: false,
                        },
                    },
                    &mut self.out,
                );
            }
            self.pending.push(request);
        }
        if self.consume_facts {
            while let Some(fact) = self.domain.pop_fact() {
                self.facts.push(fact);
            }
        }
    }

    pub fn send(&mut self, event: Event) {
        self.stage(event);
        self.durable();
        self.deliver();
    }

    /// Commit state and requester results together before executor delivery
    /// (domain/tasks.md, 5; domain/engine.md, 7).
    pub fn durable(&mut self) {
        self.commit += 1;
        let mut terminals = Vec::new();
        let mut ended = Vec::new();
        for request in &self.pending {
            match request {
                Request::Save { record } => {
                    self.records.insert(record.key(), record.clone());
                }
                Request::Erase { key } => {
                    self.records.remove(key);
                }
                Request::Ended { task, ending, .. } => {
                    assert!(self.results.insert(*task, ending.clone()).is_none(), "requester result committed once");
                    ended.push((*task, status(ending)));
                }
                Request::Acknowledged { task, attempt, accepted: Accepted::New, .. } => {
                    terminals.push((*task, *attempt));
                }
                Request::Made { .. }
                | Request::RecurringDue { .. }
                | Request::Refused { .. }
                | Request::Done { .. }
                | Request::Acknowledged { accepted: Accepted::Already, .. }
                | Request::TurnAcknowledged { .. }
                | Request::Activate { .. }
                | Request::Stop { .. }
                | Request::Adopt { .. }
                | Request::Close { .. }
                | Request::Release { .. }
                | Request::RestoreRefused { .. }
                | Request::EscalationNeeded { .. }
                | Request::EscalationsInspected { .. }
                | Request::EscalationsRechecked { .. }
                | Request::EscalationInspected { .. }
                | Request::EscalationDecided { .. }
                | Request::ProposalDecided { .. }
                | Request::PersonProposed { .. }
                | Request::PersonProposalDecided { .. }
                | Request::ProposalRerouteNeeded { .. }
                | Request::ProposalStalled { .. }
                | Request::EscalationStalled { .. }
                | Request::Sent { .. }
                | Request::Relay { .. }
                | Request::Notify { .. }
                | Request::Timer { .. } => {}
            }
        }
        if self.pending.iter().any(|request| {
            matches!(
                request,
                Request::Acknowledged { accepted: Accepted::New, .. }
                    | Request::TurnAcknowledged { accepted: Accepted::New, .. }
            )
        }) && let Some((task, cumulative)) = self.priced
        {
            self.accounting_referee.charged(task, cumulative);
        }
        self.priced = None;
        self.project_deadlines();
        if !self.restoring {
            self.accounting_referee.committed(&self.records).expect("independent conservation");
        }
        self.observe(Seen::Durable { commit: self.commit });
        let made = self
            .records
            .values()
            .filter_map(|record| match record {
                Stored::Live(record) if !self.referee.expectations().has_task(record.number) => Some(Seen::Made {
                    task: record.number,
                    parent: record.requester,
                    dependencies: record.dependencies.to_vec(),
                    depth: record.depth,
                }),
                Stored::Live(_)
                | Stored::Ended(_)
                | Stored::Stub(_)
                | Stored::Ledger(_)
                | Stored::History(_)
                | Stored::PersonProposal(_) => None,
            })
            .collect::<Vec<_>>();
        for seen in made {
            self.observe(seen);
        }
        for (task, attempt) in terminals {
            self.runs.remove(&task);
            self.observe(Seen::Terminal { task, attempt });
        }
        for (task, status) in ended {
            self.observe(Seen::Ended { task, status, after: self.commit });
        }
    }

    pub fn deliver(&mut self) {
        for request in std::mem::take(&mut self.pending) {
            match request {
                Request::Save { .. }
                | Request::RecurringDue { .. }
                | Request::Erase { .. }
                | Request::Ended { .. }
                | Request::EscalationNeeded { .. }
                | Request::Relay { .. }
                | Request::Notify { .. }
                | Request::Timer { .. }
                | Request::ProposalRerouteNeeded { .. }
                | Request::ProposalStalled { .. }
                | Request::EscalationStalled { .. }
                | Request::Release { .. } => {}
                Request::PersonProposed { reply_to, .. }
                | Request::PersonProposalDecided { reply_to, .. }
                | Request::Sent { reply_to, .. }
                | Request::Done { reply_to }
                | Request::ProposalDecided { reply_to, .. } => self.reply(reply_to, Reply::Done),
                Request::Made { reply_to, tasks } => {
                    let token = reply_to.into_token();
                    if token.raw() != u64::MAX - 1 {
                        self.reply(ReplyTo::new(token), Reply::Made(tasks.into_vec()));
                    }
                }
                Request::Refused { reply_to, problem } => self.reply(reply_to, Reply::Refused(problem)),
                Request::Acknowledged { reply_to, accepted, .. } => self.reply(reply_to, Reply::Acknowledged(accepted)),
                Request::TurnAcknowledged { reply_to, accepted, .. } => self.reply(reply_to, Reply::Turn(accepted)),
                Request::Activate { context } => {
                    self.activations.insert(context.task);
                    self.contexts.insert(context.task, *context);
                }
                Request::Stop { task, attempt } => {
                    self.stops.insert((task, attempt));
                }
                Request::Adopt { task, attempt, kept } => {
                    assert_eq!(kept, self.record(task).turn, "adoption carries durable turn fence");
                    self.runs.insert(task, attempt);
                    self.observe(Seen::Assigned { task, attempt, after: self.commit, adopted: true });
                }
                Request::Close { task, .. } => {
                    self.closing.insert(task);
                    self.observe(Seen::Closing { task });
                }
                Request::RestoreRefused { problem } => panic!("world store corrupted: {problem:?}"),
                Request::EscalationsInspected { .. }
                | Request::EscalationsRechecked { .. }
                | Request::EscalationInspected { .. }
                | Request::EscalationDecided { .. } => {
                    panic!("this child world sends no escalation decisions")
                }
            }
        }
        self.observe(Seen::Limit { live: self.live(), cap: self.env.limits.tasks });
        let live = self
            .records
            .values()
            .filter_map(|row| match row {
                Stored::Live(record) => Some(*record.clone()),
                Stored::Ended(_)
                | Stored::Stub(_)
                | Stored::Ledger(_)
                | Stored::History(_)
                | Stored::PersonProposal(_) => None,
            })
            .collect();
        self.observe(Seen::Stored { live, limits: Box::new(self.env.limits) });
        self.domain.reclaim();
    }

    fn reply(&mut self, to: ReplyTo, reply: Reply) {
        let call = to.into_token().raw();
        assert!(self.replies.insert(call, reply).is_none(), "one reply per call");
        self.observe(Seen::Replied { call, after: self.commit });
    }

    #[must_use]
    pub fn live(&self) -> usize {
        self.records.keys().filter(|key| matches!(key, Key::Live(_))).count()
    }

    #[must_use]
    pub fn record(&self, number: u64) -> &tasks::TaskRecord {
        match &self.records[&Key::Live(number)] {
            Stored::Live(record) => record,
            Stored::Ended(_) | Stored::Stub(_) | Stored::Ledger(_) | Stored::History(_) | Stored::PersonProposal(_) => {
                unreachable!("live key")
            }
        }
    }

    pub fn make(&mut self, creator: Party, batch: Vec<New>) -> Reply {
        let before = self.records.keys().copied().collect::<BTreeSet<_>>();
        let members = batch.iter().map(|task| task.number).collect();
        let reply_to = self.to();
        let call = self.call;
        self.send(Event::Make { reply_to, creator, batch: batch.into_boxed_slice() });
        let reply = self.replies[&call].clone();
        let made = self
            .records
            .keys()
            .filter_map(|key| match key {
                Key::Live(number) if !before.contains(key) => Some(*number),
                Key::Live(_)
                | Key::Ended(_)
                | Key::Stub(_)
                | Key::Ledger(_)
                | Key::History { .. }
                | Key::PersonProposal(_) => None,
            })
            .collect();
        self.observe(Seen::Batch { members, accepted: matches!(reply, Reply::Made(_)), made });
        reply
    }

    pub fn claim(&mut self, task: u64, attempt: u64) {
        let numbers = self.record(task).numbers;
        let budget = numbers.budget - numbers.spent - numbers.spent_below - numbers.reserved;
        self.claim_budget(task, attempt, budget);
    }

    pub fn claim_budget(&mut self, task: u64, attempt: u64, budget: u64) {
        assert!(self.activations.remove(&task), "activated before preparation");
        let context = &self.contexts[&task];
        let record = self.record(task);
        assert_eq!(context.spec, record.spec);
        assert_eq!(context.contract, record.contract);
        assert_eq!(context.authority, record.authority);
        let reply_to = self.to();
        self.send(Event::Prepare { reply_to, task });
        let reply_to = self.to();
        let call = self.call;
        self.send(Event::Claim { reply_to, task, attempt, budget });
        assert_eq!(self.replies[&call], Reply::Done);
        assert!(self.runs.insert(task, attempt).is_none());
        self.observe(Seen::Assigned { task, attempt, after: self.commit, adopted: false });
        self.send(Event::Started { task, attempt });
    }

    pub fn terminal(&mut self, task: u64, end: End) -> Reply {
        self.terminal_cause(task, end, Cause::Unpriced)
    }

    /// Submit a real priced or recovery terminal (domain/tasks.md, 5).
    pub fn terminal_cause(&mut self, task: u64, end: End, cause: Cause) -> Reply {
        let attempt = self.runs[&task];
        let reply_to = self.to();
        let call = self.call;
        self.send(Event::Activation { reply_to, task, attempt, end, saved: None, cause });
        self.replies[&call].clone()
    }

    pub fn claim_fresh(&mut self, task: u64) {
        let attempt = self
            .records
            .values()
            .filter_map(|row| match row {
                Stored::Live(record) | Stored::Ended(record) => Some(record.attempt),
                Stored::Ledger(_) | Stored::Stub(_) | Stored::History(_) | Stored::PersonProposal(_) => None,
            })
            .max()
            .unwrap_or(0)
            + 1;
        self.claim(task, attempt);
    }

    pub fn finish(&mut self, task: u64) {
        self.terminal(
            task,
            End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        );
    }

    pub fn settle(&mut self, task: u64) {
        self.settle_effects(task);
        self.release(task);
    }

    pub fn settle_effects(&mut self, task: u64) {
        assert!(self.closing.contains(&task));
        self.send(Event::EffectsSettled { task });
    }

    pub fn release(&mut self, task: u64) {
        assert!(self.closing.remove(&task));
        self.observe(Seen::Settled { task });
        self.send(Event::Settled { task });
    }

    pub fn advance(&mut self) {
        let at = self.deadlines.values().map(|(_, at)| *at).min().expect("backoff pending");
        self.env.wall = Wall::from_nanos(
            self.env.wall.as_nanos().saturating_add(at.as_nanos().saturating_sub(self.env.now.as_nanos())),
        );
        self.env.now = at;
        tasks::fire(&mut self.domain, &self.env, &mut self.out);
        while let Some(request) = self.out.pop() {
            match &request {
                Request::Timer { task, subscription } => {
                    self.message = self.message.checked_add(1).expect("message number room");
                    tasks::step(
                        &mut self.domain,
                        &self.env,
                        Event::Notice {
                            task: *task,
                            word: Word {
                                number: self.message,
                                from: Party::Task(*task),
                                kind: MessageKind::Timer { subscription: *subscription },
                                words: Box::new([]),
                                at: self.env.wall,
                                hits: 1,
                                eligible: false,
                            },
                        },
                        &mut self.out,
                    );
                }
                Request::Notify { .. }
                | Request::RecurringDue { .. }
                | Request::Save { .. }
                | Request::Erase { .. }
                | Request::Ended { .. }
                | Request::EscalationNeeded { .. }
                | Request::Sent { .. }
                | Request::Relay { .. }
                | Request::Made { .. }
                | Request::Refused { .. }
                | Request::Done { .. }
                | Request::Acknowledged { .. }
                | Request::TurnAcknowledged { .. }
                | Request::Activate { .. }
                | Request::Stop { .. }
                | Request::Adopt { .. }
                | Request::Close { .. }
                | Request::Release { .. }
                | Request::RestoreRefused { .. }
                | Request::EscalationsInspected { .. }
                | Request::EscalationsRechecked { .. }
                | Request::EscalationInspected { .. }
                | Request::EscalationDecided { .. }
                | Request::ProposalDecided { .. }
                | Request::PersonProposed { .. }
                | Request::PersonProposalDecided { .. }
                | Request::ProposalRerouteNeeded { .. }
                | Request::ProposalStalled { .. }
                | Request::EscalationStalled { .. } => {}
            }
            self.pending.push(request);
        }
        self.durable();
        self.deliver();
    }

    fn project_deadlines(&mut self) {
        let mut projected = BTreeMap::new();
        for row in self.records.values() {
            if let Stored::Live(task) = row
                && let tasks::Phase::Active(tasks::Active::BackingOff { until }) = task.phase
            {
                let at = match self.deadlines.get(&task.number) {
                    Some((old, at)) if *old == until => *at,
                    Some(_) | None => self.env.now.saturating_add(Duration::from_nanos(
                        until.as_nanos().saturating_sub(self.env.wall.as_nanos()),
                    )),
                };
                projected.insert(task.number, (until, at));
            }
        }
        self.deadlines = projected;
    }

    pub fn restart(&mut self) {
        self.pending.clear();
        self.priced = None;
        self.restoring = true;
        self.deadlines.clear();
        self.accounting_referee.reset(&self.records);
        self.domain = Domain::new(&self.env.limits, self.seed, Box::new([1]));
        self.activations.clear();
        self.contexts.clear();
        self.stops.clear();
        self.closing.clear();
        let rows = self
            .records
            .values()
            .filter(|row| matches!(row, Stored::Live(_) | Stored::Ledger(_) | Stored::Stub(_)))
            .cloned()
            .collect::<Vec<_>>();
        for record in rows {
            self.send(Event::Restore { record });
        }
        self.send(Event::Restored);
    }

    /// Complete the stopped descendants after an accepted delegate-cancelling
    /// terminal (domain/tasks.md, 5.6).
    pub fn complete_cancel(&mut self) {
        for (task, _) in self.stops.clone() {
            if self.runs.contains_key(&task) {
                self.terminal(task, End::Parked);
            }
        }
        while let Some(task) = self.closing.iter().next().copied() {
            self.settle(task);
        }
        self.observe(Seen::Finished);
        self.referee.assert_passed(self.seed);
    }
}

fn status(ending: &Ending) -> tasks::Status {
    match ending {
        Ending::Done(_) => tasks::Status::Done,
        Ending::Failed { .. } => tasks::Status::Failed,
        Ending::Cancelled { .. } => tasks::Status::Cancelled,
    }
}

fn result_notice(ending: Ending) -> (ResultKind, Box<[u8]>) {
    match ending {
        Ending::Done(result) => match result {
            TaskResult::Report { words } => (ResultKind::Report, words),
            TaskResult::Verdict { code, words } => (ResultKind::Verdict { code }, words),
            TaskResult::Change { connector, kind, resource, words } => {
                (ResultKind::Change { connector, kind, resource }, words)
            }
            TaskResult::Failure { reason } => (ResultKind::Failed, reason),
        },
        Ending::Failed { reason } => (ResultKind::Failed, reason),
        Ending::Cancelled { reason, .. } => (ResultKind::Cancelled, reason),
    }
}

/// Seeded dependency, priced work, retry, crash and delegate-close story
/// (domain/tasks.md, 11). Replay includes complete frozen state.
#[must_use]
pub fn run_story(seed: u64) -> (Vec<String>, Frozen) {
    run_story_facts(seed, true)
}

#[must_use]
pub fn run_story_facts(seed: u64, consume_facts: bool) -> (Vec<String>, Frozen) {
    let mut rng = Rng::new(seed);
    let mut w = World::new(seed, LIMITS);
    w.consume_facts = consume_facts;
    let arrival = Duration::from_millis(rng.between(1, 20));
    w.env.now = w.env.now.saturating_add(arrival);
    w.env.wall = Wall::from_nanos(arrival.as_nanos());
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.make(Party::Task(1), vec![task(2, &[]), task(3, &[2])]);
    w.claim_fresh(2);
    let reply_to = w.to();
    w.stage(Event::Turn { reply_to, task: 1, attempt: 1, turn: 1, read: None, offered: None, cumulative: 5 });
    if rng.chance(500) {
        w.durable();
    }
    w.restart();
    match rng.below(3) {
        0 => {
            w.terminal(2, End::Refused);
            w.advance();
            w.claim_fresh(2);
        }
        1 => {
            w.terminal(2, End::Failed(tasks::Class::Lost));
            w.advance();
            w.claim_fresh(2);
        }
        2 => {}
        _ => unreachable!(),
    }
    if rng.chance(500) {
        w.restart();
    }
    if rng.chance(200) {
        w.terminal(2, End::Finished { result: TaskResult::Failure { reason: Box::new([1]) }, cancel_delegates: false });
        w.settle(2);
        assert!(matches!(w.record(3).phase, tasks::Phase::Held { .. }));
    } else {
        w.terminal_cause(
            2,
            End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
            Cause::Priced { cumulative: rng.between(1, 10) },
        );
        w.settle(2);
        w.claim_fresh(3);
        w.finish(3);
        w.settle(3);
    }
    w.make(Party::Task(1), vec![task(4, &[])]);
    w.claim_fresh(4);
    w.make(Party::Task(4), vec![task(5, &[])]);
    w.claim_fresh(5);
    if rng.chance(500) {
        w.restart();
    }
    for task in [3, 4] {
        if w.records.contains_key(&Key::Live(task)) {
            w.observe(Seen::Cancelled { task });
        }
    }
    w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true });
    w.complete_cancel();
    assert_eq!(w.results.len(), 5);
    assert_eq!(w.live(), 0);
    (w.trace.lines().to_vec(), w.frozen())
}
