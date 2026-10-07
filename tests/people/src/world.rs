use crate::referee::{People, RouteKind, Seen, Stimulus};
use jig_core_people::{
    self as people, Ask, Domain, Entry, Event, Holding, Identity, IdentityKey, InitialOwner, Key, Limits, Outcome,
    ProposalChoice, ProposalDecision, Refusal, Reply, Request, RequestKey, Role, Seed, Stored,
};
use skein_lib::{Duration, Env, Queue, ReplyTo, Rng, Time, Token, Wall};
use skein_world::domain::{Referee, Trace};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const LIMITS: Limits = Limits {
    people: 8,
    inbox_entries: 4,
    sign_ins: 8,
    projects: 3,
    holdings: 8,
    goals: 8,
    initial_owners: 3,
    requests: 32,
    pending: 8,
    waiters: 3,
    identity_bytes: 32,
    words: 64,
    amendment_bytes: 512,
    sign_in_lifetime: Duration::from_secs(60),
    request_retention: Duration::from_secs(120),
    facts: 16,
};

pub const ENDINGS: &[&str] = &[
    "started",
    "authority",
    "observer",
    "duplicate",
    "conflict",
    "expired",
    "signed out",
    "restart before durable",
    "restart after durable",
    "busy",
    "waiter busy",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub seed: u64,
    pub limits: Limits,
    pub authority_refuses: bool,
    pub facts: bool,
}

impl Settings {
    #[must_use]
    pub const fn calm(seed: u64) -> Settings {
        Settings { seed, limits: LIMITS, authority_refuses: false, facts: true }
    }

    #[must_use]
    pub fn random(seed: u64) -> Settings {
        let mut rng = Rng::new(seed);
        let authority_refuses = rng.below(2) == 0;
        let limits = Limits {
            requests: u32::try_from(rng.between(4, 8)).expect("tiny capacity fits"),
            pending: u32::try_from(rng.between(1, 3)).expect("tiny capacity fits"),
            waiters: u32::try_from(rng.between(1, 3)).expect("tiny capacity fits"),
            people: 2,
            sign_ins: 2,
            projects: 1,
            holdings: 2,
            ..LIMITS
        };
        Settings { limits, authority_refuses, ..Settings::calm(seed) }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    pub routes: u32,
    pub tasks: u32,
    pub replies: u32,
    pub restarts: u32,
    pub endings: BTreeSet<&'static str>,
}

#[derive(Debug)]
struct Commit {
    number: u64,
    changes: Vec<Change>,
    tasks: Vec<(RequestKey, u64)>,
    proposals: Vec<((u64, u64), (u64, ProposalChoice))>,
    cleared_waiting: Vec<u64>,
}

#[derive(Debug)]
enum Change {
    Save(Stored),
    Erase(Key),
}

#[derive(Debug)]
struct Held {
    commit: u64,
    call: u64,
    reply: Reply,
}

#[derive(Debug)]
pub struct World {
    settings: Settings,
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
    owners: Box<[InitialOwner]>,
    records: BTreeMap<Key, Stored>,
    durable_tasks: BTreeMap<RequestKey, u64>,
    proposals: BTreeMap<(u64, u64), (u64, ProposalChoice)>,
    commits: VecDeque<Commit>,
    held: VecDeque<Held>,
    calls: BTreeSet<u64>,
    replies: BTreeMap<u64, Reply>,
    deferred: Vec<(Token, RequestKey)>,
    next: u64,
    issued: u64,
    durable: u64,
    stats: Stats,
    trace: Trace,
    referee: Referee<People>,
    defer_route: bool,
    context: Option<RequestKey>,
    hold_replies: bool,
}

#[must_use]
pub fn identity(user: u64) -> Identity {
    Identity {
        key: IdentityKey { provider: 0, subject: (user).to_be_bytes().into() },
        login: b"login".to_vec().into_boxed_slice(),
        name: b"name".to_vec().into_boxed_slice(),
    }
}

impl World {
    #[must_use]
    pub fn new(settings: Settings) -> World {
        Self::with_owners(settings, Box::new([]))
    }

    #[must_use]
    pub fn with_owners(settings: Settings, owners: Box<[InitialOwner]>) -> World {
        let domain = Domain::new(&settings.limits, owners.clone(), 1);
        let mut world = World {
            settings,
            domain,
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits: settings.limits },
            out: Queue::with_capacity(people::max_out(&settings.limits)),
            owners,
            records: BTreeMap::new(),
            durable_tasks: BTreeMap::new(),
            proposals: BTreeMap::new(),
            commits: VecDeque::new(),
            held: VecDeque::new(),
            calls: BTreeSet::new(),
            replies: BTreeMap::new(),
            deferred: Vec::new(),
            next: settings.seed.saturating_mul(1000),
            issued: 0,
            durable: 0,
            stats: Stats::default(),
            trace: Trace::default(),
            referee: Referee::new(People::default()),
            defer_route: false,
            context: None,
            hold_replies: false,
        };
        world.send(Event::Restored);
        world
    }

    #[must_use]
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    #[must_use]
    pub fn trace(&self) -> &[String] {
        self.trace.lines()
    }

    #[must_use]
    pub fn reply(&self, call: u64) -> Option<Reply> {
        self.replies.get(&call).copied()
    }

    #[must_use]
    pub fn tasks(&self) -> usize {
        self.durable_tasks.len()
    }

    #[must_use]
    pub fn records(&self) -> &BTreeMap<Key, Stored> {
        &self.records
    }

    pub fn ending(&mut self, name: &'static str) {
        self.stats.endings.insert(name);
    }

    fn name(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn call(&mut self) -> (u64, ReplyTo) {
        let number = self.name();
        assert!(self.calls.insert(number));
        self.observe(Seen::Called { call: number });
        (number, ReplyTo::new(Token::new(number)))
    }

    pub fn signin(&mut self, sign_in: u64, user: u64) -> u64 {
        let (call, reply_to) = self.call();
        let person = self.name();
        self.send(Event::SignedIn { reply_to, person, sign_in, identity: identity(user), kind: people::Kind::Person });
        call
    }

    pub fn signin_service(&mut self, sign_in: u64, person: u64) -> u64 {
        let (call, reply_to) = self.call();
        let candidate = self.name();
        self.send(Event::SignedIn {
            reply_to,
            person: candidate,
            sign_in,
            identity: Identity {
                key: IdentityKey { provider: 1, subject: person.to_be_bytes().into() },
                login: Box::new([]),
                name: Box::new([]),
            },
            kind: people::Kind::Service,
        });
        call
    }

    pub fn signout(&mut self, sign_in: u64) -> u64 {
        let (call, reply_to) = self.call();
        self.send(Event::SignOut { reply_to, sign_in });
        call
    }

    pub fn roles(&mut self, project: u32, holdings: Box<[Holding]>) {
        self.send(Event::Roles { project, holdings });
    }

    pub fn seed(&mut self, project: u32, collaborators: Box<[Seed]>) {
        assert!(self.domain.can_seed(&self.env.limits, project, &collaborators));
        self.send(Event::Seed { project, collaborators });
    }

    #[must_use]
    pub fn role_for_identity(&self, identity: IdentityKey, project: u32) -> Option<(u64, Role)> {
        self.domain.role_for_identity(identity, project)
    }

    pub fn waiting(&mut self, task: u64, entries: Box<[Entry]>) {
        self.send(Event::Waiting { task, entries });
    }

    #[must_use]
    pub fn inbox(&self, person: u64) -> Box<[Entry]> {
        self.domain.cached_waiting(&self.env.limits, person).expect("known person")
    }

    pub fn check_role_waiting(&mut self, task: u64, holders: [u64; 2], open: bool) {
        let present = holders.map(|person| self.inbox(person).iter().any(|entry| entry.task == task));
        if open {
            self.observe(Seen::RoleWaitingOpened { task, both_present: present == [true, true] });
        } else {
            self.observe(Seen::RoleWaitingResolved { task, both_absent: present == [false, false] });
        }
    }

    pub fn ask(&mut self, sign_in: u64, key: [u8; 16], words: &[u8]) -> u64 {
        self.request(sign_in, key, Ask::StartChat { project: 1, words: words.to_vec().into_boxed_slice() })
    }

    pub fn request(&mut self, sign_in: u64, key: [u8; 16], ask: Ask) -> u64 {
        let (call, reply_to) = self.call();
        let person = self
            .records
            .values()
            .chain(self.commits.iter().flat_map(|c| {
                c.changes.iter().filter_map(|change| match change {
                    Change::Save(row) => Some(row),
                    Change::Erase(_) => None,
                })
            }))
            .find_map(|row| match row {
                Stored::SignIn { number, person, .. } if *number == sign_in => Some(*person),
                Stored::SignIn { .. }
                | Stored::Person { .. }
                | Stored::ReadPosition { .. }
                | Stored::Roles { .. }
                | Stored::Answer { .. }
                | Stored::Policy { .. } => None,
            })
            .unwrap_or(0);
        self.context = Some(RequestKey { person, key });
        self.send(Event::Ask { reply_to, sign_in, key, ask });
        self.context = None;
        call
    }

    pub fn defer_routes(&mut self, defer: bool) {
        self.defer_route = defer;
    }

    pub fn decide_deferred(&mut self) {
        for (request, key) in std::mem::take(&mut self.deferred) {
            self.context = Some(key);
            let task = self.name();
            self.send_decision(request, key, task);
            self.context = None;
        }
    }

    fn send_decision(&mut self, request: Token, key: RequestKey, task: u64) {
        let outcome = if self.settings.authority_refuses {
            Outcome::Refused(Refusal::Authority)
        } else {
            Outcome::Started { task }
        };
        self.send_inner(Event::Decided { request, outcome }, Some((key, task)));
    }

    fn send(&mut self, event: Event) {
        self.send_inner(event, None);
    }

    #[expect(clippy::too_many_lines, reason = "the scripted parent drains one complete people step and its follow-up")]
    fn send_inner(&mut self, event: Event, task: Option<(RequestKey, u64)>) {
        let mut changes = Vec::new();
        let mut tasks = Vec::new();
        let mut proposals = Vec::new();
        let mut cleared_waiting = Vec::new();
        if !self.settings.authority_refuses {
            tasks.extend(task);
        }
        let mut replies = Vec::new();
        people::step(&mut self.domain, &self.env, event, &mut self.out);
        // Same-step root routing: Route and Decided share the decision's commit.
        while let Some(request) = self.out.pop() {
            self.trace.log(self.env.now, format_args!("{request:?}"));
            match request {
                Request::Save { record } => changes.push(Change::Save(record)),
                Request::Erase { key } => changes.push(Change::Erase(key)),
                Request::Reply { to, reply } => replies.push((to.into_token().raw(), reply)),
                Request::Route { request, person, project, role, ask } => {
                    assert_eq!(project, 1);
                    let key = self.context.expect("a routed call carries the client's key");
                    assert_eq!(person, key.person);
                    self.stats.routes += 1;
                    let kind = match ask.as_ref() {
                        Ask::StartChat { .. } => RouteKind::Chat,
                        Ask::SetGoal { .. } => RouteKind::Goal,
                        Ask::MakeService { .. } => RouteKind::Service,
                        Ask::DecideProposal { .. } => RouteKind::Proposal,
                        Ask::TakePerson { .. } => RouteKind::TakePerson,
                        Ask::Watch { .. }
                        | Ask::EditNote { .. }
                        | Ask::Adopt { .. }
                        | Ask::Stop { .. }
                        | Ask::Cancel { .. }
                        | Ask::Release { .. }
                        | Ask::HandBackPerson { .. }
                        | Ask::AnswerPerson { .. }
                        | Ask::Move { .. }
                        | Ask::Say { .. }
                        | Ask::AnswerQuestion { .. }
                        | Ask::Prioritise { .. }
                        | Ask::Amend { .. }
                        | Ask::SetRoles { .. }
                        | Ask::ChangePolicy { .. }
                        | Ask::SetPool { .. }
                        | Ask::DecideEscalation { .. } => {
                            panic!("the scripted parent handles only the named people stories")
                        }
                    };
                    let service = match self.records.get(&Key::Person(person)) {
                        Some(Stored::Person { identity, .. }) => identity.key.provider == 1,
                        Some(_) | None => false,
                    };
                    self.observe(Seen::Routed {
                        role: role.expect("scripted request requires project membership"),
                        kind,
                        service,
                    });
                    if self.defer_route {
                        assert_eq!(kind, RouteKind::Chat, "only chats use the deferred script");
                        self.deferred.push((request, key));
                    } else {
                        match *ask {
                            Ask::MakeService { .. } => {
                                let candidate = self.name();
                                people::step(
                                    &mut self.domain,
                                    &self.env,
                                    Event::MakeService { request, person: candidate },
                                    &mut self.out,
                                );
                            }
                            Ask::StartChat { .. } | Ask::SetGoal { .. } => {
                                let goal = kind == RouteKind::Goal;
                                let outcome = if self.settings.authority_refuses {
                                    Outcome::Refused(Refusal::Authority)
                                } else {
                                    let number = self.name();
                                    tasks.push((key, number));
                                    if goal {
                                        Outcome::GoalStarted { task: number }
                                    } else {
                                        Outcome::Started { task: number }
                                    }
                                };
                                people::step(
                                    &mut self.domain,
                                    &self.env,
                                    Event::Decided { request, outcome },
                                    &mut self.out,
                                );
                            }
                            Ask::DecideProposal { proposer, proposal, decision, .. } => {
                                let name = (proposer, proposal);
                                let (by, choice) = if let Some(&winner) = self.proposals.get(&name) {
                                    winner
                                } else {
                                    let choice = match decision {
                                        ProposalDecision::Accept => ProposalChoice::Accepted,
                                        ProposalDecision::Reject { .. } => ProposalChoice::Rejected,
                                        ProposalDecision::Pass => ProposalChoice::Passed,
                                    };
                                    proposals.push((name, (person, choice)));
                                    (person, choice)
                                };
                                let outcome = Outcome::ProposalDecided { proposer, proposal, by, choice };
                                people::step(
                                    &mut self.domain,
                                    &self.env,
                                    Event::Decided { request, outcome },
                                    &mut self.out,
                                );
                            }
                            Ask::TakePerson { task, .. } => {
                                let outcome = Outcome::PersonTaken { task };
                                cleared_waiting.push(task);
                                people::step(
                                    &mut self.domain,
                                    &self.env,
                                    Event::Decided { request, outcome },
                                    &mut self.out,
                                );
                            }
                            Ask::Watch { .. }
                            | Ask::EditNote { .. }
                            | Ask::Adopt { .. }
                            | Ask::Stop { .. }
                            | Ask::Cancel { .. }
                            | Ask::Release { .. }
                            | Ask::HandBackPerson { .. }
                            | Ask::AnswerPerson { .. }
                            | Ask::Move { .. }
                            | Ask::Say { .. }
                            | Ask::AnswerQuestion { .. }
                            | Ask::Prioritise { .. }
                            | Ask::Amend { .. }
                            | Ask::SetRoles { .. }
                            | Ask::ChangePolicy { .. }
                            | Ask::SetPool { .. }
                            | Ask::DecideEscalation { .. } => unreachable!("scripted route kind was checked"),
                        }
                    }
                }
                Request::RolesApplied { .. } | Request::RolesRefused { .. } => {
                    self.ending("busy");
                }
                Request::ServiceMade { request, outcome } => {
                    people::step(&mut self.domain, &self.env, Event::Decided { request, outcome }, &mut self.out);
                }
                Request::RestoreRefused { .. } => panic!("valid world records must restore"),
            }
        }
        if !changes.is_empty() || !tasks.is_empty() || !proposals.is_empty() || !cleared_waiting.is_empty() {
            self.issued += 1;
            self.commits.push_back(Commit { number: self.issued, changes, tasks, proposals, cleared_waiting });
        }
        for (call, reply) in replies {
            self.held.push_back(Held { commit: self.issued, call, reply });
        }
        self.release();
        self.domain.reclaim();
        if self.settings.facts {
            while self.domain.pop_fact().is_some() {}
        }
    }

    pub fn commit_all(&mut self) {
        while let Some(commit) = self.commits.pop_front() {
            for change in commit.changes {
                match change {
                    Change::Save(row) => {
                        self.records.insert(row.key(), row);
                    }
                    Change::Erase(key) => {
                        self.records.remove(&key);
                    }
                }
            }
            self.durable = commit.number;
            self.observe(Seen::Durable { commit: self.durable });
            for (key, task) in commit.tasks {
                assert!(self.durable_tasks.insert(key, task).is_none(), "same key cannot make another task");
                self.stats.tasks += 1;
                self.observe(Seen::Created { key });
            }
            for (name, winner) in commit.proposals {
                assert!(self.proposals.insert(name, winner).is_none(), "first proposal decision wins");
            }
            for task in commit.cleared_waiting {
                self.waiting(task, Box::new([]));
            }
            self.release();
        }
    }

    pub fn hold_replies(&mut self, hold: bool) {
        self.hold_replies = hold;
    }

    fn release(&mut self) {
        if self.hold_replies {
            return;
        }
        while self.held.front().is_some_and(|reply| reply.commit <= self.durable) {
            let held = self.held.pop_front().expect("front present");
            assert!(self.calls.remove(&held.call), "one reply per live call");
            assert!(self.replies.insert(held.call, held.reply).is_none());
            self.stats.replies += 1;
            self.observe(Seen::Replied { call: held.call, after: held.commit });
        }
    }

    /// Cold restart drops volatile outputs and flights, retaining only durable
    /// people records and scripted tasks. Client calls lost with the process
    /// are abandoned, then retried with a fresh `ReplyTo` and the same key.
    pub fn restart(&mut self) {
        self.referee.inject(self.env.now, Stimulus::Restart);
        self.fire_referee();
    }

    fn restart_cold(&mut self) {
        for call in std::mem::take(&mut self.calls) {
            self.observe(Seen::Abandoned { call });
        }
        self.commits.clear();
        self.held.clear();
        self.deferred.clear();
        self.issued = self.durable;
        self.domain = Domain::new(&self.env.limits, self.owners.clone(), 1);
        for record in self.records.values().cloned().collect::<Vec<_>>() {
            self.send(Event::Restore { record });
        }
        self.send(Event::Restored);
        self.stats.restarts += 1;
    }

    pub fn advance(&mut self, duration: Duration) {
        self.env.now = self.env.now.saturating_add(duration);
        self.env.wall = Wall::from_nanos(self.env.wall.as_nanos().saturating_add(duration.as_nanos()));
        while self.domain.is_due(self.env.now) {
            people::fire(&mut self.domain, &self.env, &mut self.out);
            // Fire's erases form their own decision, handled by a harmless
            // Restored event after its already queued outputs.
            self.send(Event::Restored);
        }
        self.commit_all();
        self.fire_referee();
    }

    fn observe(&mut self, seen: Seen) {
        let mut stimuli = Vec::new();
        self.referee.observe(self.env.now, seen, &mut stimuli);
        for stimulus in stimuli {
            match stimulus {
                Stimulus::Restart => self.restart_cold(),
            }
        }
        self.referee.assert_holding(self.settings.seed);
    }

    fn fire_referee(&mut self) {
        let mut stimuli = Vec::new();
        self.referee.fire(self.env.now, &mut stimuli);
        for stimulus in stimuli {
            match stimulus {
                Stimulus::Restart => self.restart_cold(),
            }
        }
        self.referee.assert_holding(self.settings.seed);
    }

    pub fn assert_settled(&self) {
        assert!(self.calls.is_empty() && self.commits.is_empty() && self.held.is_empty() && self.deferred.is_empty());
        self.referee.assert_passed(self.settings.seed);
    }

    /// Compact scenario matrix: each seed draws authority and crash cuts while
    /// every configured outcome is named and checked across the sweep.
    pub fn run(&mut self) {
        let signin = self.signin(10, 1);
        self.commit_all();
        let Reply::SignedIn { person, .. } = self.reply(signin).expect("sign-in replied") else { panic!("signed in") };
        self.roles(1, Box::new([Holding { person, role: Role::Member }]));
        self.commit_all();
        let mut rng = Rng::new(self.settings.seed);
        let first = self.ask(10, [1; 16], b"hello");
        assert!(self.reply(first).is_none(), "answer waits for durability");
        let before = rng.below(2) == 0;
        if before {
            self.restart();
            self.ending("restart before durable");
        } else {
            self.hold_replies(true);
            self.commit_all();
            assert!(self.reply(first).is_none());
            self.restart();
            self.hold_replies(false);
            self.ending("restart after durable");
        }
        let replay = self.ask(10, [1; 16], b"hello");
        self.commit_all();
        assert!(matches!(self.reply(replay), Some(Reply::Outcome(_))));
        self.ending(if self.settings.authority_refuses { "authority" } else { "started" });
        let routes = self.stats.routes;
        let duplicate = self.ask(10, [1; 16], b"hello");
        self.commit_all();
        assert_eq!(self.reply(duplicate), self.reply(replay));
        assert_eq!(self.stats.routes, routes);
        self.ending("duplicate");
        let conflict = self.ask(10, [1; 16], b"different");
        self.commit_all();
        assert_eq!(self.reply(conflict), Some(Reply::Refused(Refusal::KeyConflict)));
        self.ending("conflict");
        self.defer_routes(true);
        let mut waiting = Vec::new();
        for _ in 0..self.settings.limits.waiters {
            waiting.push(self.ask(10, [7; 16], b"pending"));
        }
        let overflow = self.ask(10, [7; 16], b"pending");
        self.commit_all();
        assert_eq!(self.reply(overflow), Some(Reply::Refused(Refusal::Busy)));
        self.ending("waiter busy");
        self.decide_deferred();
        self.commit_all();
        self.defer_routes(false);
        for call in waiting {
            assert!(matches!(self.reply(call), Some(Reply::Outcome(_))));
        }
        self.roles(1, Box::new([Holding { person, role: Role::Observer }]));
        self.commit_all();
        let denied = self.ask(10, [2; 16], b"hello");
        self.commit_all();
        assert_eq!(self.reply(denied), Some(Reply::Outcome(Outcome::Refused(Refusal::Role))));
        self.ending("observer");
        for byte in
            20..u8::try_from(self.settings.limits.requests).expect("scenario request limit fits u8").saturating_add(20)
        {
            let call = self.ask(10, [byte; 16], b"fill");
            self.commit_all();
            if self.reply(call) == Some(Reply::Refused(Refusal::Busy)) {
                self.ending("busy");
            }
        }
        assert!(self.stats.endings.contains("busy"));
        self.advance(Duration::from_secs(61));
        let expired = self.ask(10, [3; 16], b"hello");
        self.commit_all();
        assert_eq!(self.reply(expired), Some(Reply::Refused(Refusal::SignIn)));
        self.ending("expired");
        let signin = self.signin(11, 1);
        self.commit_all();
        assert!(matches!(self.reply(signin), Some(Reply::SignedIn { .. })));
        let signout = self.signout(11);
        self.commit_all();
        assert_eq!(self.reply(signout), Some(Reply::SignedOut));
        self.ending("signed out");
        self.assert_settled();
    }
}
