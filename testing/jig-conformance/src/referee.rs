//! The core's promises checked from durable rows and independent neighbours.
//! This module names no domain types. A root adapter supplies snapshots of the
//! store, and fakes supply arrivals, reads and expenses. Scenario policy is
//! captured before constructing the root; it is never inferred from its live
//! state (`domain/core.md`, 11; `domain/testing.md`, 6).
use std::collections::{BTreeMap, BTreeSet};

use jig_test_system::{Observed as EffectObserved, SystemKey};

/// An externally meaningful resource address.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Name {
    pub connector: u16,
    pub path: Vec<Vec<u8>>,
}

/// Literal segments followed by an exact terminal or an open prefix.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pattern {
    pub base: Vec<Vec<u8>>,
    pub terminal: Vec<u8>,
    pub open: bool,
}

impl Pattern {
    fn covers(&self, path: &[Vec<u8>]) -> bool {
        path.starts_with(&self.base)
            && path
                .get(self.base.len())
                .is_some_and(|last| if self.open { last.starts_with(&self.terminal) } else { last == &self.terminal })
            && (self.open || path.len() == self.base.len() + 1)
    }

    fn includes(&self, other: &Self) -> bool {
        if !self.open {
            return self == other;
        }
        if !other.base.starts_with(&self.base) {
            return false;
        }
        other
            .base
            .get(self.base.len())
            .map_or_else(|| other.terminal.starts_with(&self.terminal), |segment| segment.starts_with(&self.terminal))
    }
}

/// Permission for a connector kind over one literal pattern.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Grant {
    pub connector: u16,
    pub kind: u16,
    pub pattern: Pattern,
}

/// Complete scenario authority, separate from the core's carrier.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Scope {
    pub budget: u64,
    pub deadline: Option<u64>,
    pub tools: u64,
    pub grants: Vec<Grant>,
    pub executors: BTreeSet<(u8, u32)>,
    pub descendants: u32,
    pub depth: u32,
    pub notes: u8,
    pub note_resources: Vec<(u16, Pattern)>,
}

/// Where a task's permission came from, including accepted holder work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// Ordinary child authority.
    Task(u64),
    /// A person's role, including work accepted as their own.
    Person(u64),
    /// Scenario-authorized standing work.
    Deployment,
}

/// Durable lifecycle needed by the referee, without a child's phase enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Waiting on work or resources.
    Waiting,
    /// Able to execute.
    Active,
    /// Ending but still owing cleanup.
    Closing,
    /// Held for a decision.
    Held,
    /// Done and closed, with a successful dependency result.
    Done,
    /// Closed with another result.
    Ended,
}

/// The externally visible parts of one durable task row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Task {
    /// This task has goal projection state.
    pub tracked: bool,
    pub number: u64,
    pub project: u32,
    pub source: Source,
    pub scope: Scope,
    pub executor: (u8, u32),
    pub budget: u64,
    pub spent: u64,
    pub spent_below: u64,
    pub reserved: u64,
    pub run_reserved: u64,
    pub run_spent: u64,
    pub attempt: u64,
    /// Standing work resets descendant authority for each considered period.
    pub period: Option<u64>,
    pub phase: Phase,
    pub dependencies: Vec<u64>,
    pub delegates: Vec<u64>,
    pub pools: Vec<Name>,
    pub inbox: Vec<u64>,
    pub proposal: Option<(u64, Source)>,
}

impl Task {
    fn closed(&self) -> bool {
        matches!(self.phase, Phase::Done | Phase::Ended)
    }
}

/// A receipt whose prerequisite an outside delivery depends on.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Receipt {
    /// Stable task admission.
    Task(u64),
    /// Durable claim before assignment.
    Claim(u64, u64),
    /// Immutable turn before acknowledgement.
    Turn(u64, u64, u32),
    /// Immutable terminal before acknowledgement.
    Terminal(u64, u64),
    /// Named call decision before a state-changing answer.
    Call(u64, u64, u32, u32),
    /// Saved sign-in before issuing it.
    SignIn(u64),
    /// Saved keyed party outcome.
    Party(u64, [u8; 16]),
    /// Saved effect attempt before sending it.
    Outbox(u16, SystemKey, u32),
    /// Closed result before announcing it.
    Ended(u64),
    /// Durable inbox word before relaying it.
    Word(u64, u64),
}

/// Recovery guarantee promised by a configured effect kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recovery {
    /// The system retains the key.
    Keyed,
    /// The system checks the prior state.
    Conditional,
    /// Repeated sets must name the same target.
    Idempotent,
    /// Uncertainty requires a person's decision.
    Unrecoverable,
}

/// A requirement that the scenario puts on a kind and resource pattern.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Requirement {
    /// None applies deployment-wide; Some selects one project.
    pub project: Option<u32>,
    pub connector: u16,
    pub kind: u16,
    pub pattern: Pattern,
    pub judge: u16,
    /// None means it must hold at application; Some is observed freshness.
    pub freshness: Option<u64>,
}

/// Independently configured semantics and maximum price of an effect kind.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Kind {
    pub recovery: Recovery,
    pub price: u64,
}

/// One newly durable effect decision; retained across removal and restart.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Decision {
    /// This effect projects a tracked goal under project policy.
    pub projection: bool,
    pub connector: u16,
    pub key: SystemKey,
    pub kind: u16,
    pub resources: Vec<Name>,
    pub condition: Option<u64>,
    pub target: u64,
    pub judged_state: u64,
    /// A covered holder accepted this exact proposed effect.
    pub accepted_by: Option<Source>,
}

/// Durable acceptance of an exact proposal by its covered holder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Acceptance {
    pub proposal: u64,
    pub by: Source,
}

/// One atomic committed view, supplied by the store adapter.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot {
    pub tasks: BTreeMap<u64, Task>,
    pub receipts: BTreeSet<Receipt>,
    pub writers: Vec<(Name, u64, u64)>,
    pub pools: BTreeMap<Name, u32>,
    pub decisions: Vec<Decision>,
    /// Last attempt number and absolute retry deadline for each durable key.
    pub attempts: BTreeMap<(u16, SystemKey), (u32, u64)>,
    pub acceptances: BTreeMap<(u16, SystemKey), Acceptance>,
    pub held_effects: BTreeSet<(u16, SystemKey)>,
    pub settled_effects: BTreeSet<(u16, SystemKey)>,
    pub batches: Vec<Vec<u64>>,
    /// The person's maximum effect spend is charged here on holder acceptance.
    pub person_spend: BTreeMap<u64, u64>,
    /// Budget, direct spend, settled funded spend and open reservations.
    pub balances: Vec<(u64, u64, u64, u64)>,
}

/// Policy and time limits provided by the scenario, before the root exists.
#[derive(Clone, Debug)]
pub struct Policy {
    pub deployment: Scope,
    pub projects: BTreeMap<u32, Scope>,
    /// Grants supplied by scenario policy for goal projection writes.
    pub projections: BTreeMap<u32, Vec<Grant>>,
    pub people: BTreeMap<(u32, u64), Scope>,
    pub effect_accepters: BTreeSet<(u32, u64)>,
    pub requirements: Vec<Requirement>,
    pub implications: BTreeSet<(u16, u16, u16)>,
    pub kinds: BTreeMap<(u16, u16), Kind>,
    pub owned: Vec<(u16, Pattern)>,
    pub participating: BTreeSet<Name>,
    pub mechanics: BTreeSet<(u16, u16)>,
    pub delivery_bound: u64,
    pub uncertainty_bound: u64,
    pub story_steps: u64,
}

/// A neighbour or store observation, in the referee's vocabulary.
#[derive(Clone, Debug)]
pub enum Observed {
    /// The fake store applied one whole commit.
    Durable { number: u64, snapshot: Snapshot },
    /// A delivery reached a host, party, or system.
    Released { requires: Vec<Receipt> },
    /// A host saw a reserved assignment.
    Assigned { task: u64, attempt: u64, budget: u64 },
    /// An independent host made a priced completion against its reservation.
    Completion { task: u64, attempt: u64, cumulative: u64 },
    /// A vanished worker lost these unacknowledged completions.
    LostTurns { task: u64, attempt: u64, turns: u32, spent: u64 },
    /// Independent lifetime expense, with lost unacknowledged spend permitted.
    Spend { task: u64, spent: u64, lost_bound: u64 },
    /// A system read used by a scenario's requirement judge.
    Read { resource: Name, state: Option<u64>, at: u64 },
    /// A system received an effect copy, including its pre-write state.
    Effect { connector: u16, effect: EffectObserved },
    /// A possibly applied attempt has not yet been recovered or held.
    Uncertain { connector: u16, key: SystemKey },
    /// A person explicitly permits the next attempt of an uncertain write.
    RetryDecided { connector: u16, key: SystemKey },
    /// One result reached its observed recipient.
    Result { task: u64, to: Source },
    /// One result reached its requester or one word reached its task.
    Reached(Receipt),
    /// A newly accepted word needs delivery, or the addressed task must close.
    Words { task: u64, message: u64 },
    /// A scenario declares the atomic batch it asked to create.
    Batch { members: Vec<u64> },
    /// Engine-only heap meter, excluding histories held by test neighbours.
    Heap { held: u64, maximum: u64 },
    /// Durable receipt view read afresh on a cold start.
    ColdStore { receipts: BTreeSet<Receipt> },
    /// One external load in the application's independently declared order.
    RestartStage { position: u32 },
    /// A cold start; the outside ledgers and deadlines survive it.
    Restart,
    /// One world iteration; the scenario chooses its finite story bound.
    Tick,
}

/// Which externally observable promise failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Promise {
    /// Permission from policy and the complete authority chain.
    Authority,
    /// Reservations, charges and independent expenses.
    Spend,
    /// Recovery guarantees and atomic task creation.
    Once,
    /// Durable prerequisites before outside outputs.
    Commit,
    /// Dependencies, delegates, writers, pools and conditional state.
    Order,
    /// Delivery, visibility and recovery deadlines.
    Lost,
    /// Owned objects and changes by other hands.
    Ownership,
    /// Engine heap and finite world iterations.
    Bounded,
}

/// A failed promise with the observation that made it observable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Violation {
    pub promise: Promise,
    pub why: String,
}

/// Persistent observer state; restart clears none of its evidence or deadlines.
#[derive(Debug)]
pub struct Referee {
    pub policy: Policy,
    last: Snapshot,
    commit: u64,
    first: BTreeMap<u64, u64>,
    made: BTreeMap<(u64, u64), u32>,
    decisions: BTreeMap<(u16, SystemKey), Decision>,
    copies: BTreeMap<(u16, SystemKey), (u32, u64)>,
    sent: BTreeMap<(u16, SystemKey), u32>,
    reads: BTreeMap<Name, (Option<u64>, u64)>,
    assigned: BTreeMap<(u64, u64), u64>,
    lost: BTreeMap<(u64, u64), u64>,
    waiting: BTreeMap<Receipt, u64>,
    reached: BTreeSet<Receipt>,
    seen: BTreeSet<Receipt>,
    uncertain: BTreeMap<(u16, SystemKey), u64>,
    retry: BTreeSet<(u16, SystemKey)>,
    steps: u64,
    restart_position: u32,
}

fn require(holds: bool, promise: Promise, why: impl Into<String>) -> Result<(), Violation> {
    if holds { Ok(()) } else { Err(Violation { promise, why: why.into() }) }
}

impl Referee {
    /// Start with the scenario's independent policy and an empty durable store.
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            last: Snapshot::default(),
            commit: 0,
            first: BTreeMap::new(),
            made: BTreeMap::new(),
            decisions: BTreeMap::new(),
            copies: BTreeMap::new(),
            sent: BTreeMap::new(),
            reads: BTreeMap::new(),
            assigned: BTreeMap::new(),
            lost: BTreeMap::new(),
            waiting: BTreeMap::new(),
            reached: BTreeSet::new(),
            seen: BTreeSet::new(),
            uncertain: BTreeMap::new(),
            retry: BTreeSet::new(),
            steps: 0,
            restart_position: 0,
        }
    }

    fn kind_allows(&self, connector: u16, given: u16, needed: u16) -> bool {
        given == needed || self.policy.implications.contains(&(connector, given, needed))
    }

    fn within(&self, a: &Scope, b: &Scope) -> bool {
        a.budget <= b.budget
            && a.tools & !b.tools == 0
            && a.notes & !b.notes == 0
            && a.descendants <= b.descendants
            && a.depth <= b.depth
            && b.deadline.is_none_or(|latest| a.deadline.is_some_and(|at| at <= latest))
            && a.executors.is_subset(&b.executors)
            && a.grants.iter().all(|a| {
                b.grants.iter().any(|b| {
                    a.connector == b.connector
                        && self.kind_allows(a.connector, b.kind, a.kind)
                        && b.pattern.includes(&a.pattern)
                })
            })
            && a.note_resources.iter().all(|(connector, pattern)| {
                b.note_resources.iter().any(|(other, outer)| connector == other && outer.includes(pattern))
            })
    }

    fn permits(&self, scope: &Scope, connector: u16, kind: u16, names: &[Name]) -> bool {
        names.iter().all(|name| {
            scope.grants.iter().any(|grant| {
                grant.connector == connector
                    && self.kind_allows(connector, grant.kind, kind)
                    && grant.pattern.covers(&name.path)
            })
        })
    }

    fn source<'a>(&'a self, snapshot: &'a Snapshot, task: &Task) -> Option<&'a Scope> {
        match task.source {
            Source::Task(parent) => snapshot.tasks.get(&parent).map(|task| &task.scope),
            Source::Person(person) => self.policy.people.get(&(task.project, person)),
            Source::Deployment => Some(&self.policy.deployment),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one atomic snapshot checks linked task, pool and effect evidence together"
    )]
    fn durable(&mut self, number: u64, snapshot: Snapshot, now: u64) -> Result<(), Violation> {
        require(number == self.commit + 1, Promise::Commit, "commits apply whole in number order")?;
        for task in snapshot.tasks.values() {
            if !self
                .last
                .tasks
                .get(&task.number)
                .is_some_and(|old| old.scope == task.scope && old.source == task.source)
            {
                let project = self.policy.projects.get(&task.project);
                let source = self.source(&snapshot, task);
                require(
                    project.is_some_and(|scope| self.within(&task.scope, scope))
                        && self.within(&task.scope, &self.policy.deployment)
                        && source.is_some_and(|scope| self.within(&task.scope, scope)),
                    Promise::Authority,
                    format!("task {} exceeds policy or its authority source", task.number),
                )?;
                if !self.first.contains_key(&task.number) {
                    if let Source::Task(parent) = task.source {
                        let scope = &snapshot.tasks[&parent].scope;
                        require(
                            scope.executors.contains(&task.executor) && scope.depth > 0 && scope.descendants > 0,
                            Promise::Authority,
                            "child's executor or delegation is outside its parent",
                        )?;
                    }
                    let mut source = task.source;
                    let mut depth = 1;
                    while let Source::Task(parent) = source {
                        let ancestor = &snapshot.tasks[&parent];
                        let made = self.made.entry((parent, ancestor.period.unwrap_or(0))).or_default();
                        *made += 1;
                        require(
                            *made <= ancestor.scope.descendants && depth <= ancestor.scope.depth,
                            Promise::Authority,
                            "lifetime descendants or depth exceed an ancestor grant",
                        )?;
                        source = ancestor.source;
                        depth += 1;
                        require(
                            usize::try_from(depth).expect("depth fits") <= snapshot.tasks.len() + 1,
                            Promise::Authority,
                            "authority chain contains a cycle",
                        )?;
                    }
                    self.first.insert(task.number, number);
                }
            }
            require(task.budget <= task.scope.budget, Promise::Authority, "allotment exceeds spend authority")?;
            let total = task.spent.checked_add(task.spent_below).and_then(|sum| sum.checked_add(task.reserved));
            require(
                total.is_some_and(|total| total <= task.budget)
                    && task.run_spent <= task.spent
                    && task.run_reserved <= task.reserved,
                Promise::Spend,
                format!("task {} overdraws its durable allotment", task.number),
            )?;
            if task.closed() {
                require(
                    task.delegates.iter().all(|child| snapshot.tasks.get(child).is_some_and(Task::closed)),
                    Promise::Order,
                    "task ended before its delegates closed",
                )?;
                if !self.last.tasks.get(&task.number).is_some_and(Task::closed) {
                    self.waiting.entry(Receipt::Ended(task.number)).or_insert(now + self.policy.delivery_bound);
                }
            }
            if let Some((_, holder)) = task.proposal {
                require(
                    match holder {
                        Source::Task(holder) => snapshot.tasks.get(&holder).is_some_and(|task| !task.closed()),
                        Source::Person(person) => self.policy.people.contains_key(&(task.project, person)),
                        Source::Deployment => self.policy.projects.contains_key(&task.project),
                    },
                    Promise::Lost,
                    "pending proposal is not visible to a live holder",
                )?;
            }
        }
        for (budget, spent, below, reserved) in &snapshot.balances {
            require(
                spent
                    .checked_add(*below)
                    .and_then(|sum| sum.checked_add(*reserved))
                    .is_some_and(|total| total <= *budget),
                Promise::Spend,
                "a funding period or person pool overdraws its allotment",
            )?;
        }
        let mut writers = BTreeSet::new();
        for (name, _, _) in &snapshot.writers {
            require(writers.insert(name), Promise::Order, "two writers own one resource")?;
        }
        for (pool, slots) in &snapshot.pools {
            let holders: BTreeSet<_> =
                snapshot.tasks.values().filter(|task| task.pools.contains(pool)).map(|task| task.number).collect();
            let old: BTreeSet<_> =
                self.last.tasks.values().filter(|task| task.pools.contains(pool)).map(|task| task.number).collect();
            require(
                holders.is_subset(&old) || holders.len() <= usize::try_from(*slots).expect("u32 fits"),
                Promise::Order,
                "pool admitted a holder beyond its known slots or while shrinking",
            )?;
        }
        for (key, attempt) in &snapshot.attempts {
            if let Some(previous) = self.last.attempts.get(key) {
                require(attempt.0 >= previous.0, Promise::Once, "durable attempt number went backwards")?;
                if attempt.0 == previous.0 {
                    require(
                        attempt.1 == previous.1,
                        Promise::Once,
                        "same attempt changed its absolute retry deadline",
                    )?;
                }
            }
        }
        let mut maxima: BTreeMap<(bool, u64), u64> = BTreeMap::new();
        for decision in &snapshot.decisions {
            let key = (decision.connector, decision.key);
            if let Some(earlier) = self.decisions.get(&key) {
                require(earlier == decision, Promise::Once, "an existing key changed its decided payload")?;
            } else {
                self.decision(decision, &snapshot, now)?;
                if decision.projection {
                    self.decisions.insert(key, decision.clone());
                    continue;
                }
                let payer = match decision.accepted_by {
                    Some(Source::Person(person)) => (true, person),
                    Some(Source::Task(task)) => (false, task),
                    Some(Source::Deployment) | None => (false, decision.key.task),
                };
                let charge = maxima.entry(payer).or_default();
                *charge = charge
                    .checked_add(self.policy.kinds[&(decision.connector, decision.kind)].price)
                    .ok_or_else(|| Violation { promise: Promise::Spend, why: "effect price sum overflowed".into() })?;
                self.decisions.insert(key, decision.clone());
            }
        }
        for ((person, payer), maximum) in maxima {
            let charged = if person {
                snapshot
                    .person_spend
                    .get(&payer)
                    .copied()
                    .unwrap_or(0)
                    .checked_sub(self.last.person_spend.get(&payer).copied().unwrap_or(0))
            } else {
                snapshot.tasks[&payer].spent.checked_sub(self.last.tasks.get(&payer).map_or(0, |task| task.spent))
            };
            require(
                charged.is_some_and(|charged| charged >= maximum),
                Promise::Spend,
                "commit undercharged its aggregate effect maxima",
            )?;
        }
        for batch in &snapshot.batches {
            self.batch(batch)?;
        }
        for key in snapshot.held_effects.iter().chain(&snapshot.settled_effects) {
            self.uncertain.remove(key);
        }
        self.commit = number;
        self.last = snapshot;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one committed effect checks task or projection policy, funding and requirements"
    )]
    fn decision(&self, decision: &Decision, snapshot: &Snapshot, now: u64) -> Result<(), Violation> {
        let task = snapshot.tasks.get(&decision.key.task);
        require(task.is_some(), Promise::Authority, "effect has no admitted task")?;
        let task = task.expect("checked task");
        if decision.projection {
            require(
                task.tracked && decision.accepted_by.is_none(),
                Promise::Authority,
                "projection requires a tracked goal and no proposal exception",
            )?;
        }
        if let Some(by) = decision.accepted_by {
            let acceptance = snapshot.acceptances.get(&(decision.connector, decision.key));
            let pending = self.last.tasks.get(&task.number).and_then(|task| task.proposal);
            require(
                acceptance.is_some_and(|accepted| {
                    accepted.by == by
                        && pending.is_some_and(|(number, holder)| {
                            number == accepted.proposal
                                && (holder == by
                                    || (holder == Source::Deployment
                                        && match by {
                                            Source::Person(person) => {
                                                self.policy.effect_accepters.contains(&(task.project, person))
                                            }
                                            Source::Task(_) | Source::Deployment => false,
                                        }))
                        })
                }),
                Promise::Authority,
                "effect exception has no prior proposal and durable covered-holder acceptance",
            )?;
        }
        let projection;
        let holder = if decision.projection {
            projection = Scope {
                grants: self.policy.projections.get(&task.project).cloned().unwrap_or_default(),
                ..self.policy.projects[&task.project].clone()
            };
            Some(&projection)
        } else {
            match decision.accepted_by {
                Some(Source::Person(person)) => self.policy.people.get(&(task.project, person)),
                Some(Source::Task(holder)) => snapshot.tasks.get(&holder).map(|task| &task.scope),
                Some(Source::Deployment) => Some(&self.policy.deployment),
                None => Some(&task.scope),
            }
        };
        require(holder.is_some(), Promise::Authority, "proposal acceptance names no covered holder")?;
        let scope = holder.expect("covered holder");
        let project = &self.policy.projects[&task.project];
        require(
            [scope, project, &self.policy.deployment].iter().all(|scope| {
                self.permits(scope, decision.connector, decision.kind, &decision.resources)
                    && scope.deadline.is_none_or(|deadline| now <= deadline)
            }),
            Promise::Authority,
            "effect exceeds the authority in force when decided",
        )?;
        let kind = self.policy.kinds.get(&(decision.connector, decision.kind));
        require(kind.is_some(), Promise::Authority, "effect kind is not configured")?;
        let price = kind.expect("configured kind").price;
        let charged = match decision.accepted_by {
            Some(Source::Person(person)) => snapshot
                .person_spend
                .get(&person)
                .copied()
                .unwrap_or(0)
                .checked_sub(self.last.person_spend.get(&person).copied().unwrap_or(0)),
            Some(Source::Task(holder)) => {
                snapshot.tasks[&holder].spent.checked_sub(self.last.tasks.get(&holder).map_or(0, |task| task.spent))
            }
            Some(Source::Deployment) | None => {
                task.spent.checked_sub(self.last.tasks.get(&task.number).map_or(0, |task| task.spent))
            }
        };
        require(
            decision.projection || charged.is_some_and(|charged| charged >= price),
            Promise::Spend,
            "priced effect was not charged its maximum in its deciding commit",
        )?;
        for requirement in &self.policy.requirements {
            if requirement.connector != decision.connector
                || requirement.kind != decision.kind
                || requirement.project.is_some_and(|project| project != task.project)
            {
                continue;
            }
            for resource in &decision.resources {
                if !requirement.pattern.covers(&resource.path) {
                    continue;
                }
                if let Some(freshness) = requirement.freshness {
                    let name = Name { connector: requirement.judge, path: resource.path.clone() };
                    require(
                        self.reads.get(&name).is_some_and(|(state, at)| {
                            *state == Some(decision.judged_state) && *at <= now && now - at <= freshness
                        }),
                        Promise::Authority,
                        "observed requirement was not met within freshness at decision",
                    )?;
                } else {
                    require(
                        decision.condition == Some(decision.judged_state),
                        Promise::Authority,
                        "guarded requirement has no system condition for its decided state",
                    )?;
                }
            }
        }
        Ok(())
    }

    fn batch(&self, members: &[u64]) -> Result<(), Violation> {
        let commits: BTreeSet<_> = members.iter().filter_map(|member| self.first.get(member)).collect();
        let present = members.iter().filter(|member| self.first.contains_key(member)).count();
        require(
            present == 0 || (present == members.len() && commits.len() == 1),
            Promise::Once,
            "an atomic task batch was only partly made, or crossed commits",
        )
    }

    fn effect(&mut self, connector: u16, effect: &EffectObserved) -> Result<(), Violation> {
        let key = (connector, effect.key);
        let decision = self.decisions.get(&key);
        require(decision.is_some(), Promise::Commit, "system received an effect without a durable decision")?;
        let decision = decision.expect("durable decision");
        let names: Vec<_> = effect.resources.iter().map(|name| Name { connector, path: name.0.clone() }).collect();
        require(
            effect.kind == decision.kind
                && names == decision.resources
                && effect.target == decision.target
                && effect.condition == decision.condition,
            Promise::Authority,
            "system saw a different kind, resource, target or condition than was authorized",
        )?;
        let recovery = self.policy.kinds[&(connector, effect.kind)].recovery;
        let sends = self.sent.entry(key).or_default();
        if recovery == Recovery::Unrecoverable && *sends > 0 && self.uncertain.contains_key(&key) {
            require(
                self.retry.remove(&key),
                Promise::Once,
                "uncertain unrecoverable effect repeated without a person",
            )?;
        }
        *sends += 1;
        require(
            recovery != Recovery::Unrecoverable
                || self.copies.get(&key).is_none_or(|copies| copies.0 == 0)
                || self.uncertain.contains_key(&key),
            Promise::Once,
            "a successful unrecoverable operation was repeated",
        )?;
        if !effect.applied {
            return Ok(());
        }
        let copies = self.copies.entry(key).or_insert((0, effect.target));
        require(
            match recovery {
                Recovery::Keyed | Recovery::Conditional => copies.0 == 0,
                Recovery::Idempotent => copies.1 == effect.target,
                Recovery::Unrecoverable => true,
            },
            Promise::Once,
            "recovery guarantee was broken by an applied copy",
        )?;
        copies.0 += 1;
        require(effect.before.len() == names.len(), Promise::Ownership, "system omitted pre-write evidence")?;
        for (name, before) in names.iter().zip(&effect.before) {
            if let Some(condition) = effect.condition {
                require(
                    before.state.unwrap_or(0) == condition,
                    Promise::Order,
                    "conditional effect applied to a different state than its decision saw",
                )?;
            }
            require(
                self.policy.owned.iter().any(|(owner, pattern)| *owner == connector && pattern.covers(&name.path))
                    || self.policy.participating.contains(name)
                    || self.policy.mechanics.contains(&(connector, effect.kind)),
                Promise::Ownership,
                "effect writes outside owned, participating or connector-managed objects",
            )?;
            require(
                before.state.is_none()
                    || before.state == Some(effect.target)
                    || before.owner.is_some_and(|owner| owner.deployment == effect.key.deployment)
                    || effect.condition == before.state,
                Promise::Ownership,
                "effect overwrote a change this deployment did not make",
            )?;
        }
        Ok(())
    }

    /// Check one outside observation and all deadlines at this simulated time.
    ///
    /// # Errors
    /// Returns the first promise contradicted by the observation.
    #[expect(clippy::too_many_lines, reason = "one total dispatch owns the referee's outside observations")]
    pub fn observe(&mut self, now: u64, observed: Observed) -> Result<(), Violation> {
        match observed {
            Observed::Durable { number, snapshot } => self.durable(number, snapshot, now)?,
            Observed::Released { requires } => {
                for receipt in requires {
                    require(
                        self.last.receipts.contains(&receipt),
                        Promise::Commit,
                        format!("outside delivery preceded its durable prerequisite: {receipt:?}"),
                    )?;
                    self.seen.insert(receipt);
                }
            }
            Observed::Assigned { task, attempt, budget } => {
                let row = self.last.tasks.get(&task);
                require(
                    row.is_some_and(|row| row.attempt == attempt)
                        && self.last.receipts.contains(&Receipt::Claim(task, attempt)),
                    Promise::Commit,
                    "host assigned before its task and claim were durable",
                )?;
                let row = row.expect("durable assignment");
                require(
                    row.dependencies.iter().all(|dependency| {
                        self.last.tasks.get(dependency).is_some_and(|task| task.phase == Phase::Done)
                    }),
                    Promise::Order,
                    "task starts before its dependencies are done and closed",
                )?;
                require(
                    budget <= row.run_reserved + row.run_spent,
                    Promise::Spend,
                    "host assignment has no prior maximum run reservation",
                )?;
                self.seen.insert(Receipt::Claim(task, attempt));
                self.assigned.insert((task, attempt), budget);
            }
            Observed::Completion { task, attempt, cumulative } => {
                require(
                    self.assigned.get(&(task, attempt)).is_some_and(|budget| cumulative <= *budget),
                    Promise::Spend,
                    "completion made beyond the previously reserved maximum",
                )?;
            }
            Observed::LostTurns { task, attempt, turns, spent } => {
                require(
                    turns <= 2
                        && (spent == 0 || turns > 0)
                        && self.assigned.get(&(task, attempt)).is_some_and(|reserved| spent <= *reserved),
                    Promise::Spend,
                    "lost spend exceeds the retained-turn or reserved allowance bound",
                )?;
                self.lost.insert((task, attempt), spent);
            }
            Observed::Spend { task, spent, lost_bound } => {
                let allowed: u64 =
                    self.lost.iter().filter(|((number, _), _)| *number == task).map(|(_, spent)| *spent).sum();
                require(
                    lost_bound <= allowed,
                    Promise::Spend,
                    "claimed lost-turn credit has no independent worker evidence",
                )?;
                require(
                    self.last
                        .tasks
                        .get(&task)
                        .is_some_and(|row| row.budget.checked_add(lost_bound).is_some_and(|maximum| spent <= maximum)),
                    Promise::Spend,
                    "independent spend exceeds budget plus lost unacknowledged turns",
                )?;
            }
            Observed::Read { resource, state, at } => {
                self.reads.insert(resource, (state, at));
            }
            Observed::Effect { connector, effect } => self.effect(connector, &effect)?,
            Observed::Uncertain { connector, key } => {
                if !self.last.held_effects.contains(&(connector, key))
                    && !self.last.settled_effects.contains(&(connector, key))
                {
                    self.uncertain.entry((connector, key)).or_insert(now + self.policy.uncertainty_bound);
                }
            }
            Observed::RetryDecided { connector, key } => {
                self.retry.insert((connector, key));
            }
            Observed::Result { task, to } => {
                require(
                    self.last.tasks.get(&task).is_some_and(|row| row.closed() && row.source == to),
                    Promise::Lost,
                    "result reached someone other than its requester",
                )?;
                self.waiting.remove(&Receipt::Ended(task));
                self.reached.insert(Receipt::Ended(task));
            }
            Observed::Reached(receipt) => {
                self.waiting.remove(&receipt);
                self.reached.insert(receipt);
            }
            Observed::Words { task, message } => {
                let receipt = Receipt::Word(task, message);
                if self.last.receipts.contains(&receipt) {
                    // Durable task inboxes count as delivery to the task even
                    // while execution is deliberately held by its requester.
                    self.reached.insert(receipt);
                } else {
                    self.waiting.entry(receipt).or_insert(now + self.policy.delivery_bound);
                }
            }
            Observed::Batch { members } => self.batch(&members)?,
            Observed::Heap { held, maximum } => {
                require(held <= maximum, Promise::Bounded, "engine heap exceeded worst_case")?;
            }
            Observed::ColdStore { receipts } => {
                for receipt in &self.seen {
                    let must_keep = match receipt {
                        Receipt::Task(_) | Receipt::Turn(..) | Receipt::Terminal(..) | Receipt::Ended(_) => true,
                        Receipt::Claim(task, attempt) | Receipt::Call(task, attempt, ..) => {
                            self.last.tasks.get(task).is_some_and(|row| !row.closed() && row.attempt == *attempt)
                        }
                        Receipt::SignIn(_) | Receipt::Party(..) | Receipt::Outbox(..) | Receipt::Word(..) => false,
                    };
                    require(
                        !must_keep || receipts.contains(receipt),
                        Promise::Commit,
                        format!("cold store lost an observed prerequisite: {receipt:?}"),
                    )?;
                }
            }
            Observed::RestartStage { position } => {
                require(position == self.restart_position, Promise::Order, "root performed a cold load out of order")?;
                self.restart_position += 1;
            }
            Observed::Restart => self.restart_position = 0,
            Observed::Tick => {
                self.steps += 1;
                require(
                    self.steps <= self.policy.story_steps,
                    Promise::Bounded,
                    "world exceeded its finite iteration bound",
                )?;
            }
        }
        self.check_at(now)
    }

    /// Check outstanding deliveries and uncertain effects, including after restart.
    ///
    /// # Errors
    /// Returns the first missed scenario deadline.
    pub fn check_at(&self, now: u64) -> Result<(), Violation> {
        for (receipt, deadline) in &self.waiting {
            let satisfied = self.reached.contains(receipt)
                || match receipt {
                    Receipt::Word(task, _) => self.last.tasks.get(task).is_some_and(Task::closed),
                    Receipt::Task(_)
                    | Receipt::Claim(..)
                    | Receipt::Turn(..)
                    | Receipt::Terminal(..)
                    | Receipt::Call(..)
                    | Receipt::SignIn(_)
                    | Receipt::Party(..)
                    | Receipt::Outbox(..)
                    | Receipt::Ended(_) => false,
                };
            require(
                satisfied || now <= *deadline,
                Promise::Lost,
                format!("delivery missed its deadline: {receipt:?}"),
            )?;
        }
        for deadline in self.uncertain.values() {
            require(now <= *deadline, Promise::Lost, "uncertain effect was neither settled nor held within its bound")?;
        }
        Ok(())
    }
}

/// Named obligations exposed to skein's scenario referee.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Obligation {
    /// Words or a result must reach their recipient.
    Delivery(Receipt),
    /// An uncertain effect must settle or become visibly held.
    Uncertainty(u16, SystemKey),
}

impl Referee {
    fn obligations(&self) -> BTreeMap<Obligation, u64> {
        self.waiting
            .iter()
            .filter(|(receipt, _)| {
                !self.reached.contains(*receipt)
                    && !matches!(receipt, Receipt::Word(task, _) if self.last.tasks.get(task).is_some_and(Task::closed))
            })
            .map(|(receipt, deadline)| (Obligation::Delivery(receipt.clone()), *deadline))
            .chain(
                self.uncertain
                    .iter()
                    .map(|((connector, key), deadline)| (Obligation::Uncertainty(*connector, *key), *deadline)),
            )
            .collect()
    }
}

impl skein_world::domain::Expectations for Referee {
    type Seen = Observed;
    type Name = Obligation;
    type Stimulus = ();

    fn observe(&mut self, seen: Self::Seen, judge: &mut skein_world::domain::Judge<Self::Name, Self::Stimulus>) {
        let before = self.obligations();
        let now = judge.now().as_nanos();
        let result = self.observe(now, seen);
        judge.check(result.is_ok(), format_args!("{result:?}"));
        let after = self.obligations();
        for name in before.keys().filter(|name| !after.contains_key(*name)) {
            let _met = judge.meet(name);
        }
        for (name, deadline) in after {
            if !judge.is_pending(&name) {
                judge.expect(name, skein_lib::Duration::from_nanos(deadline.saturating_sub(now)));
            }
        }
    }
}
