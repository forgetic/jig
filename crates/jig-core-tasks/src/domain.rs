//! Live task and finite-source ownership, step dispatch and retry scheduling
//! (domain/tasks.md, sections 2, 5 and 10). Root supplies authorized events
//! and iteration time, routes outputs and owns durability/transport proofs.
//! Tasks never performs IO or keeps full historical results or root shadows.
use crate::{Active, Event, Fact, Limits, New, Party, Phase, Problem, Refusal, Request, Stored, TaskRecord, Tries};
use alloc::boxed::Box;
use skein_lib::{Deadlines, Env, Id, List, Map, Queue, ReplyTo, Rng, Slab, Time, Wall};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Alarm {
    pub until: Wall,
    pub due: Time,
}

#[derive(Debug)]
pub(crate) struct Task {
    pub record: TaskRecord,
    pub alarm: Option<Alarm>,
    pub observed_hold: Option<crate::Hold>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Startup {
    Restoring,
    Ready,
    Failed,
}

/// Bounded live task arena, immutable dependencies, retry timers, finite period/pool ledgers,
/// configured charters and deterministic retry randomness. Keeps bounded historical stubs,
/// transport receipt, connector state or mutable root shadow ledger. (domain/tasks.md, sections 2, 4–5 and 10).
#[derive(Debug)]
pub struct Domain {
    pub(crate) startup: Startup,
    pub(crate) tasks: Slab<Task>,
    pub(crate) names: Map<u64, Id<Task>>,
    pub(crate) stubs: Map<u64, crate::Stub>,
    pub(crate) alarms: Deadlines<u64>,
    pub(crate) timers: Deadlines<u64>,
    pub(crate) wakes: Deadlines<u64>,
    pub(crate) proposal_alarms: Deadlines<u64>,
    pub(crate) escalation_alarms: Deadlines<u64>,
    pub(crate) hold_alarms: Deadlines<u64>,
    pub(crate) hold_kinds: Map<crate::holds::KindKey, crate::HoldKind>,
    pub(crate) pools: Map<crate::Name, crate::PoolSlots>,
    pub(crate) next_pool: u64,
    pub(crate) writers: Map<crate::Name, crate::WriterSlot>,
    pub(crate) next_writer: u64,
    pub(crate) funding: Map<crate::Funder, crate::FundingRecord>,
    pub(crate) person_proposals: Map<u64, crate::PersonProposal>,
    pub(crate) charters: Box<[u32]>,
    pub(crate) rng: Rng,
    facts: Queue<Fact>,
    lost: u64,
}

/// One bounded live row projected for an authenticated tree or goal watch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ViewTask {
    pub number: u64,
    pub project: u32,
    pub requester: Party,
    pub phase: u32,
    pub tracked: Option<u32>,
}

/// Stable codes carried by the views child; details stay in task history.
#[must_use]
pub fn view_phase(phase: &Phase) -> u32 {
    match phase {
        Phase::Waiting => 0,
        Phase::Active(_) => 1,
        Phase::Closing(_) => 2,
        Phase::Held { .. } => 3,
        Phase::Ended(_) => 4,
    }
}

impl Domain {
    /// Borrowed live projection copied into a bounded result for snapshot construction.
    #[must_use]
    pub fn view_tasks(&self) -> Box<[ViewTask]> {
        let mut rows = List::with_capacity(self.names.capacity());
        for (number, _) in &self.names {
            let row = record(self, *number).expect("indexed live row");
            rows.push(ViewTask {
                number: *number,
                project: row.project,
                requester: row.requester,
                phase: view_phase(&row.phase),
                tracked: row.tracked,
            })
            .expect("one row per live name");
        }
        rows.into_boxed()
    }
    /// Create restoring state from validated `limits`, deterministic `seed` and unique configured
    /// `charters` bounded by `limits.charters`. Panics on invalid/unrepresentable limits or
    /// malformed charter configuration; no task becomes active until `Restore`/`Restored`
    /// completes.
    #[must_use]
    pub fn new(limits: &Limits, seed: u64, charters: Box<[u32]>) -> Domain {
        assert!(crate::worst_case(limits).is_some(), "task limits are valid");
        assert!(charters.len() <= usize::try_from(limits.charters).expect("u32 fits usize"), "configured charters fit");
        for (at, charter) in charters.iter().enumerate() {
            for earlier in charters.iter().take(at) {
                assert!(earlier != charter, "charters are unique");
            }
        }
        Domain {
            startup: Startup::Restoring,
            tasks: Slab::with_capacity(limits.tasks),
            names: Map::with_capacity(limits.tasks),
            stubs: Map::with_capacity(crate::limits::stub_capacity(limits).expect("valid stub bound")),
            alarms: Deadlines::with_capacity(limits.tasks),
            timers: Deadlines::with_capacity(limits.tasks.checked_mul(limits.subscriptions).expect("timer room")),
            wakes: Deadlines::with_capacity(limits.tasks),
            proposal_alarms: Deadlines::with_capacity(limits.tasks),
            escalation_alarms: Deadlines::with_capacity(limits.tasks),
            hold_alarms: Deadlines::with_capacity(limits.tasks),
            hold_kinds: Map::with_capacity(limits.hold_kinds),
            pools: Map::with_capacity(limits.pools),
            next_pool: 0,
            writers: Map::with_capacity(limits.tasks.checked_mul(limits.holdings).expect("writer room")),
            next_writer: 0,
            funding: Map::with_capacity(limits.funders),
            person_proposals: Map::with_capacity(limits.tasks),
            charters,
            rng: Rng::new(seed),
            facts: Queue::with_capacity(limits.facts),
            lost: 0,
        }
    }

    /// Pure borrowed lookup of authentic period/pool accounting for root policy checks; returns
    /// `None` when absent. The table is bounded by `Limits::funders`; no readiness transition,
    /// allocation or output occurs and the root must not persist a second mutable ledger.
    #[must_use]
    pub fn funding(&self, funder: crate::Funder) -> Option<&crate::FundingRecord> {
        self.funding.get(&funder)
    }

    /// Pure remaining ledger slots for a root-administered period/pool pair.
    #[must_use]
    pub fn funding_room(&self) -> u32 {
        self.funding.capacity().saturating_sub(self.funding.len())
    }

    /// Current opaque executor identity for root procedure routing.
    #[must_use]
    pub fn executor(&self, task: u64) -> Option<crate::Executor> {
        Some(record(self, task)?.executor)
    }

    /// Stable tree root for a connector's task-scoped names.
    #[must_use]
    pub fn root(&self, task: u64) -> Option<u64> {
        Some(record(self, task)?.root)
    }

    /// Borrowed current task facts for a connector projection in the same root decision.
    #[must_use]
    pub fn task(&self, number: u64) -> Option<&TaskRecord> {
        record(self, number)
    }

    /// Current authentic holder of an opaque exclusive resource.
    #[must_use]
    pub fn holder(&self, resource: &crate::Name) -> Option<u64> {
        crate::holds::writer_holder(self, resource)
    }

    /// Current authentic writer slot, including a lost attempt awaiting a fresh read.
    #[must_use]
    pub fn writer(&self, resource: &crate::Name) -> Option<&crate::WriterSlot> {
        self.writers.get(resource)
    }

    /// Current deployment-owned core recurring task identities for one project.
    #[must_use]
    pub fn recurring_tasks(&self, project: u32) -> Box<[u64]> {
        let mut numbers = List::with_capacity(self.names.capacity());
        for (number, _) in &self.names {
            let row = record(self, *number).expect("live name");
            if row.project == project && row.recurring.is_some() {
                numbers.push(*number).expect("one identity per task");
            }
        }
        numbers.into_boxed()
    }

    /// Borrow a recurring task's durable template for root authority checks.
    #[must_use]
    pub fn recurring_template(&self, task: u64) -> Option<&crate::RecurringTemplate> {
        Some(&record(self, task)?.recurring.as_ref()?.template)
    }

    /// Next fenced step for a due procedure, or none when it is not ready to step.
    #[must_use]
    pub fn procedure_due(&self, task: u64) -> Option<(u16, u32, u64)> {
        let record = record(self, task)?;
        if record.phase != Phase::Active(Active::Due) {
            return None;
        }
        if record.recurring.is_some() {
            return None;
        }
        match record.executor {
            crate::Executor::Agent { .. } | crate::Executor::Person(_) => None,
            crate::Executor::Procedure { connector, code } => Some((connector, code, record.attempt.checked_add(1)?)),
        }
    }

    /// Borrowed current creation ceiling; no mutable task ledger is copied
    /// into the root or retained after this call's decision.
    #[must_use]
    pub fn delegation(&self, task: u64) -> Option<crate::DelegationContext> {
        let record = record(self, task)?;
        let made_below = record.made.checked_sub(1)?;
        let tasks_left = record.authority.delegation.tasks.checked_sub(made_below)?;
        Some(crate::DelegationContext {
            project: record.project,
            requester: record.requester,
            deciding: match record.phase {
                Phase::Waiting | Phase::Active(_) => match record.executor {
                    crate::Executor::Agent { .. } => true,
                    crate::Executor::Procedure { .. } | crate::Executor::Person(_) => false,
                },
                Phase::Closing(_) | Phase::Held { .. } | Phase::Ended(_) => false,
            },
            authority: record.authority.clone(),
            numbers: record.numbers,
            tasks_left,
        })
    }

    /// Clone one bounded current proposal for a root authority and holder decision.
    #[must_use]
    pub fn proposal(&self, proposer: u64, number: u64) -> Option<crate::Proposal> {
        crate::proposals::context(self, proposer, number)
    }

    /// Whether this exact pending proposal was made by the proposer's result.
    #[must_use]
    pub fn result_proposal(&self, proposer: u64, number: u64) -> bool {
        match record(self, proposer) {
            Some(row) => match &row.proposal {
                Some(proposal) => row.result_proposal && proposal.number == number,
                None => false,
            },
            None => false,
        }
    }

    /// Clone one pending person-origin goal proposal for the root's authority check.
    #[must_use]
    pub fn person_proposal(&self, proposer: u64, number: u64) -> Option<crate::PersonProposal> {
        let proposal = self.person_proposals.get(&number)?;
        if proposal.proposer == proposer { Some(proposal.clone()) } else { None }
    }

    /// Clone one bounded held decision context for root's current route.
    #[must_use]
    pub fn escalation(&self, task: u64) -> Option<Box<crate::EscalationContext>> {
        crate::escalation::context(self, task)
    }

    #[must_use]
    pub(crate) fn ready(&self) -> bool {
        self.startup == Startup::Ready
    }

    /// Reclaim retired live task slots at the parent's iteration reclaim point after outputs have
    /// been routed; emits no persistence or lifecycle output.
    pub fn reclaim(&mut self) {
        self.tasks.reclaim();
    }

    /// Remove one optional content-free observation from the bounded diagnostic queue; keeping or
    /// dropping facts changes no decision, durability barrier or reply.
    pub fn pop_fact(&mut self) -> Option<Fact> {
        self.facts.pop()
    }
}

pub(crate) fn output_bound(limits: &Limits) -> Option<u32> {
    limits
        .tasks
        .checked_mul(20)?
        .checked_add(limits.batch.checked_mul(2)?)?
        .checked_add(limits.funders.checked_mul(3)?)?
        .checked_add(limits.tasks.checked_mul(limits.subscriptions)?.checked_mul(2)?)?
        .checked_add(crate::limits::stub_capacity(limits)?)?
        .checked_add(limits.tasks.checked_mul(limits.holdings)?.checked_mul(3)?)?
        .checked_add(8)
}

/// Required free `Request` slots for one `step` or `fire` under validated `limits`, including
/// a possible stale-stub erase for every bounded live name. Cascades are bounded by the live
/// set; panics if bound arithmetic is invalid. Caller counts output payload copies separately.
#[must_use]
pub fn max_out(limits: &Limits) -> u32 {
    output_bound(limits).expect("task limits admit output bound")
}

/// Apply one root-issued typed event to `domain` using the iteration clocks and immutable limits in
/// `env`, then advance bounded dependency/closing cascades when ready. Caller reserves `max_out`
/// free slots. Reply-bearing inputs produce one terminal reply; notifications may emit no output.
/// Root checks authority, fences exact transport replay and commits saves/erases with resulting
/// effects before external replies.
#[expect(clippy::too_many_lines, reason = "the closed task event vocabulary dispatches to focused handlers")]
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::Kinds { connector, kinds } => crate::holds::kinds(domain, &env.limits, connector, &kinds),
        Event::Slots { pool, slots } => crate::holds::slots(domain, &env.limits, pool, slots, out),
        Event::AllocationGone { pool, task } => crate::holds::allocation_gone(domain, env, &pool, task, out),
        Event::WakeProcedure { task } => {
            let wake = match record(domain, task) {
                Some(row) => match row.executor {
                    crate::Executor::Procedure { .. } => row.phase == Phase::Active(Active::Idle),
                    crate::Executor::Agent { .. } | crate::Executor::Person(_) => false,
                },
                None => false,
            };
            if wake {
                task_mut(domain, task).expect("live procedure").record.phase = Phase::Active(Active::Due);
                publish(domain, env, task, out);
                activate(domain, task, out);
            }
        }
        Event::ProposePerson { reply_to, proposal } => {
            crate::proposals::propose_person(domain, env, reply_to, proposal, out);
        }
        Event::DecidePersonProposal { reply_to, proposer, proposal, by, message, decision } => {
            crate::proposals::decide_person(domain, env, reply_to, proposer, proposal, by, message, decision, out);
        }
        Event::TickRecurring { task, period } => crate::recurring::tick(domain, env, task, period, out),
        Event::RecurringBatch { task, period, numbers } => {
            crate::recurring::make_batch(domain, env, task, period, &numbers, out);
        }
        Event::Procedure { reply_to, task, step, decision } => {
            crate::procedure::stepped(domain, env, reply_to, task, step, decision, out);
        }
        Event::TakePerson { reply_to, task, person } => {
            crate::person::take(domain, env, reply_to, task, person, out);
        }
        Event::HandBackPerson { reply_to, task, person } => {
            crate::person::hand_back(domain, env, reply_to, task, person, out);
        }
        Event::AnswerPerson { reply_to, task, person, result } => {
            crate::person::answer(domain, env, reply_to, task, person, result, out);
        }
        Event::Propose { reply_to, proposal } => {
            crate::proposals::propose(domain, env, reply_to, proposal, out);
        }
        Event::DecideProposal { reply_to, proposer, proposal, message, by, decision } => {
            crate::proposals::decide(domain, env, reply_to, proposer, proposal, message, by, decision, out);
        }
        Event::StalledProposal { proposer, proposal, holder } => {
            crate::proposals::stalled(domain, env, proposer, proposal, holder, out);
        }
        Event::WithdrawProposal { reply_to, proposer, proposal } => {
            crate::proposals::withdraw(domain, env, reply_to, proposer, proposal, out);
        }
        Event::Control { reply_to, by, task, control } => {
            crate::control::apply(domain, env, reply_to, by, task, control, out);
        }
        Event::Amend { reply_to, by, task, message, stop_run, amendment } => {
            crate::control::amend(domain, env, reply_to, by, task, message, stop_run, amendment, out);
        }
        Event::Prioritise { reply_to, project, by, goals } => {
            crate::control::prioritise(domain, env, reply_to, project, by, &goals, out);
        }
        Event::Move { reply_to, task, person, period, pool_budget, period_budget, reason } => {
            crate::moving::apply(domain, env, reply_to, task, person, period, pool_budget, period_budget, &reason, out);
        }
        Event::Subscribe { reply_to, task, subscription } => {
            crate::subscriptions::subscribe(domain, env, reply_to, task, subscription, false, out);
        }
        Event::SubscribeTopic { reply_to, task, subscription } => {
            crate::subscriptions::subscribe(domain, env, reply_to, task, subscription, true, out);
        }
        Event::Unsubscribe { reply_to, task, subscription } => {
            crate::subscriptions::unsubscribe(domain, env, reply_to, task, subscription, out);
        }
        Event::Notice { task, word } => crate::subscriptions::notice(domain, env, task, word, out),
        Event::Introduce { reply_to, by, left, right } => {
            crate::refs::introduce(domain, env, reply_to, by, left, right, out);
        }
        Event::RememberStub { stub } => crate::closing::remember_stub(domain, stub, out),
        Event::InspectEscalations { reply_to, project } => {
            let result = crate::escalation::project_contexts(domain, &env.limits, project);
            out.push(Request::EscalationsInspected { reply_to, result });
            return;
        }
        Event::RecheckEscalations { reply_to, project } => {
            let result = match crate::escalation::project_contexts(domain, &env.limits, project) {
                Ok(contexts) => {
                    for context in contexts {
                        out.push(Request::EscalationNeeded { context: Box::new(context) });
                    }
                    Ok(())
                }
                Err(refusal) => Err(refusal),
            };
            out.push(Request::EscalationsRechecked { reply_to, result });
            return;
        }
        Event::InspectEscalation { reply_to, task } => {
            out.push(Request::EscalationInspected { reply_to, context: crate::escalation::context(domain, task) });
        }
        Event::RoutedEscalation { task, revision, holder, entry } => {
            crate::escalation::routed(domain, env, task, revision, holder, entry, out);
        }
        Event::DecideEscalation { reply_to, task, revision, by, entry, decision } => {
            crate::escalation::decide(domain, env, reply_to, task, revision, by, entry, decision, out);
        }
        Event::OpenPeriod { reply_to, project, period, budget } => {
            crate::funders::open(domain, reply_to, project, period, budget, out);
        }
        Event::CarvePool { reply_to, project, person, period, budget } => {
            crate::funders::carve(domain, reply_to, project, person, period, budget, out);
        }
        Event::ResizePool { reply_to, project, person, period, budget } => {
            crate::funders::resize_pool(domain, reply_to, project, person, period, budget, out);
        }
        Event::Make { reply_to, creator, batch } => make(domain, env, reply_to, creator, batch, out),
        Event::MakeResultFollowups { reply_to, proposer, proposal, batch } => {
            make_result_followups(domain, env, reply_to, proposer, proposal, batch, out);
        }
        Event::Message { reply_to, project, task, word } => {
            crate::inbox::message(domain, env, reply_to, project, task, word, out);
        }
        Event::DelegateResult { task, word } => {
            crate::inbox::delegate_result(domain, env, task, word, out);
            crate::recurring::after_delegate(domain, env, task, out);
        }
        Event::Prepare { reply_to, task } => crate::run::prepare(domain, env, reply_to, task, out),
        Event::Claim { reply_to, task, attempt, budget, writes } => {
            crate::run::claim(domain, env, reply_to, task, attempt, budget, &writes, out);
        }
        Event::ReadAfresh { resource } => crate::writers::read_afresh(domain, &resource, out),
        Event::EffectInFlight { reply_to, task, resource, entry } => {
            crate::writers::effect_in_flight(domain, &env.limits, reply_to, task, resource, entry, out);
        }
        Event::EffectSettled { resource, entry } => crate::writers::effect_settled(domain, &resource, entry, out),
        Event::Turn { reply_to, task, attempt, turn, read, offered, cumulative } => {
            crate::admission::turn(domain, env, reply_to, task, attempt, turn, read, offered, cumulative, out);
        }
        Event::Started { task, attempt } => crate::run::started(domain, env, task, attempt, out),
        Event::Activation { reply_to, task, attempt, end, saved, cause } => match cause {
            crate::Cause::Priced { cumulative } => {
                crate::admission::activation(domain, env, reply_to, task, attempt, end, saved, cumulative, out);
            }
            crate::Cause::Unpriced => match end {
                crate::End::FinishedWithFollowups { .. } => {
                    refused(reply_to, Some(task), Refusal::Contract, out);
                }
                crate::End::Finished { .. } | crate::End::Parked | crate::End::Failed(_) | crate::End::Refused => {
                    crate::run::activation(domain, env, reply_to, task, attempt, end, saved, out);
                }
            },
        },
        Event::PreparationFailed { task } => crate::run::preparation_failed(domain, env, task, out),
        Event::Hold { task, why } => crate::run::hold(domain, env, task, why, out),
        Event::EffectsSettled { task } => crate::closing::effects_settled(domain, env, task, out),
        Event::Retained { task, root, holding } => crate::holds::retained(domain, env, task, root, &holding, out),
        Event::Settled { task } => crate::closing::settled(domain, env, task, out),
        Event::Restore { record } => crate::stored::restore(domain, env, record, out),
        Event::Restored => {
            crate::stored::restored(domain, env, out);
            if domain.ready() {
                crate::proposals::rearm_all(domain, env);
                crate::proposals::wake_restored(domain, env, out);
                crate::escalation::rearm_all(domain, env);
                crate::escalation::wake_restored(domain, env, out);
            }
        }
    }
    if domain.ready() {
        crate::holds::expire(domain, env, out);
        crate::closing::progress(domain, env, out);
    }
}

/// Expire at most one due retry deadline using `env.now` after successful restoration, then advance
/// bounded closing/readiness cascades. Caller reserves `max_out` free `Request` slots and drives
/// later iterations while due; wall correction does not reproject an already armed deadline.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    crate::subscriptions::timer_due(domain, env, out);
    crate::wake::fire(domain, env, out);
    crate::proposals::fire(domain, env, out);
    crate::escalation::fire(domain, env, out);
    crate::holds::expire(domain, env, out);
    if let Some(number) = domain.alarms.expire(env.now)
        && let Some(task) = task_mut(domain, number)
    {
        match task.record.phase {
            Phase::Active(Active::BackingOff { .. }) => {
                task.record.phase = Phase::Active(Active::Due);
                publish(domain, env, number, out);
                activate(domain, number, out);
            }
            Phase::Waiting
            | Phase::Active(
                Active::Idle | Active::Due | Active::Preparing | Active::Claimed { .. } | Active::Running { .. },
            )
            | Phase::Closing(_)
            | Phase::Held { .. }
            | Phase::Ended(_) => {}
        }
    }
    crate::closing::progress(domain, env, out);
}

pub(crate) fn record(domain: &Domain, number: u64) -> Option<&TaskRecord> {
    Some(&domain.tasks.get(*domain.names.get(&number)?).expect("name indexes live task").record)
}

pub(crate) fn task_mut(domain: &mut Domain, number: u64) -> Option<&mut Task> {
    let id = *domain.names.get(&number)?;
    Some(domain.tasks.get_mut(id).expect("name indexes live task"))
}

pub(crate) fn snapshot(domain: &Domain, capacity: u32) -> List<u64> {
    let mut numbers = List::with_capacity(capacity);
    for (number, _) in &domain.names {
        numbers.push(*number).expect("one snapshot entry per live name");
    }
    numbers
}

pub(crate) fn fact(domain: &mut Domain, observation: Fact) {
    if domain.facts.try_push(observation).is_err() {
        domain.lost = domain.lost.saturating_add(1);
    }
}

pub(crate) fn refused(to: ReplyTo, task: Option<u64>, why: Refusal, out: &mut Queue<Request>) {
    out.push(Request::Refused { reply_to: to, problem: Problem { task, why, blocked_by: None } });
}

pub(crate) fn entrance(domain: &Domain, to: ReplyTo, number: u64) -> Result<ReplyTo, (ReplyTo, Refusal)> {
    if !domain.ready() {
        return Err((to, Refusal::NotReady));
    }
    if !domain.names.contains_key(&number) {
        return Err((to, Refusal::Unknown));
    }
    Ok(to)
}

pub(crate) fn activate(domain: &Domain, number: u64, out: &mut Queue<Request>) {
    let task = record(domain, number).expect("activation names live task");
    match task.executor {
        crate::Executor::Person(_) => return,
        crate::Executor::Agent { .. } | crate::Executor::Procedure { .. } => {}
    }
    let waiting = crate::proposals::waiting_for(domain, number);
    let escalations = crate::escalation::waiting_for(domain, number);
    let capacity = task.inbox.len().checked_add(waiting.len()).expect("bounded inbox and proposals");
    let capacity = capacity.checked_add(escalations.len()).expect("bounded inbox and decisions");
    let mut unordered = List::with_capacity(u32::try_from(capacity).expect("bounded inbox and proposals"));
    for word in &task.inbox {
        unordered.push(word.clone()).expect("actual inbox counted");
    }
    for word in waiting {
        unordered.push(word).expect("virtual proposal counted");
    }
    for word in escalations {
        unordered.push(word).expect("virtual escalation counted");
    }
    let mut inbox = List::with_capacity(unordered.len());
    let mut previous = 0_u64;
    for _ in 0..unordered.len() {
        let mut next: Option<&crate::Word> = None;
        for candidate in &unordered {
            if candidate.number > previous {
                next = match next {
                    Some(current) if current.number < candidate.number => Some(current),
                    Some(_) | None => Some(candidate),
                };
            }
        }
        let next = next.expect("distinct globally numbered inbox entries");
        previous = next.number;
        inbox.push(next.clone()).expect("ordered inbox room");
    }
    let mut delegates = List::with_capacity(u32::try_from(task.delegates.len()).expect("bounded delegates"));
    for child in &task.delegates {
        let child_record = record(domain, *child).expect("live delegate named by requester");
        delegates
            .push(crate::DelegateState { task: *child, phase: child_record.phase.clone() })
            .expect("bounded delegate snapshot");
    }
    out.push(Request::Activate {
        context: Box::new(crate::RunContext {
            task: number,
            last_message: match inbox.last() {
                Some(word) => task.last_message.max(word.number),
                None => task.last_message,
            },
            inbox: inbox.into_boxed(),
            delegates: delegates.into_boxed(),
            dependencies: task.dependencies.clone(),
            saved: task.saved.clone(),
            previous_attempt: task.attempt,
            ever_turned: task.ever_turned,
            tries: task.tries,
            invalid_result: task.invalid_result,
            project: task.project,
            executor: task.executor,
            spec: task.spec.clone(),
            contract: task.contract.clone(),
            requester: task.requester,
            authority: task.authority.clone(),
            numbers: task.numbers,
        }),
    });
}

pub(crate) fn publish(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    crate::proposals::withdraw_on_close(domain, number, out);
    let task = task_mut(domain, number).expect("published task is live");
    let held = match task.record.phase {
        Phase::Held { why, .. } => Some(why),
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => None,
    };
    let became_held = held.is_some() && task.observed_hold != held;
    let unavailable = match task.record.phase {
        Phase::Held { .. } | Phase::Closing(_) | Phase::Ended(_) => true,
        Phase::Waiting | Phase::Active(_) => false,
    };
    task.observed_hold = held;
    let escalation = crate::escalation::begin(&mut task.record);
    let until = match task.record.phase {
        Phase::Active(Active::BackingOff { until }) => Some(until),
        Phase::Waiting
        | Phase::Active(
            Active::Idle | Active::Due | Active::Preparing | Active::Claimed { .. } | Active::Running { .. },
        )
        | Phase::Closing(_)
        | Phase::Held { .. }
        | Phase::Ended(_) => None,
    };
    let alarm = match until {
        Some(until) => match task.alarm {
            Some(old) if old.until == until => Some(old),
            Some(_) | None => Some(Alarm {
                until,
                due: env.now.saturating_add(skein_lib::Duration::from_nanos(
                    until.as_nanos().saturating_sub(env.wall.as_nanos()),
                )),
            }),
        },
        None => None,
    };
    task.alarm = alarm;
    out.push(Request::Save { record: Stored::Live(Box::new(task.record.clone())) });
    crate::escalation::schedule(domain, env, number);
    if escalation {
        out.push(Request::EscalationNeeded {
            context: crate::escalation::context(domain, number).expect("new held person context"),
        });
    }
    match alarm {
        Some(alarm) => {
            let armed = domain.alarms.arm(number, alarm.due);
            assert!(armed.is_ok(), "one alarm per live task");
        }
        None => {
            domain.alarms.cancel(number);
        }
    }
    if became_held {
        crate::subscriptions::notify_state(domain, number, crate::NoticeState::Held, &[], out);
    }
    if unavailable {
        crate::proposals::holder_unavailable(domain, number, out);
        crate::escalation::holder_unavailable(domain, number, out);
    }
}

pub(crate) fn make(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    creator: Party,
    batch: Box<[New]>,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    if let Err(problem) = crate::batch::check(domain, &env.limits, creator, &batch) {
        return out.push(Request::Refused { reply_to: to, problem });
    }
    let numbers = make_admitted(domain, env, creator, batch, out);
    out.push(Request::Made { reply_to: to, tasks: numbers });
}

fn make_result_followups(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    proposer: u64,
    proposal: u64,
    batch: Box<[New]>,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Some(proposer), Refusal::NotReady, out);
    }
    if let Err(problem) =
        crate::batch::check_result_followups(domain, &env.limits, Party::Task(proposer), proposal, &batch)
    {
        return out.push(Request::Refused { reply_to: to, problem });
    }
    let numbers = make_admitted(domain, env, Party::Task(proposer), batch, out);
    out.push(Request::Made { reply_to: to, tasks: numbers });
}

/// Construct a whole batch after its entrance and funding were preflighted in this decision.
#[expect(clippy::manual_map, reason = "the subset uses a closed match instead of a closure")]
#[expect(clippy::too_many_lines, reason = "one whole admitted batch constructs every durable member")]
pub(crate) fn make_admitted(
    domain: &mut Domain,
    env: &Env<Limits>,
    creator: Party,
    batch: Box<[New]>,
    out: &mut Queue<Request>,
) -> Box<[u64]> {
    crate::funders::reserve(domain, env, &batch, out);
    let parent = match creator {
        Party::Task(number) => Some(number),
        Party::Person(_) | Party::Deployment { .. } => None,
    };
    let (root, depth) = match parent {
        Some(number) => {
            let parent = record(domain, number).expect("creator admitted");
            (Some(parent.root), parent.depth.checked_add(1).expect("depth admitted"))
        }
        None => (None, 0),
    };
    let mut numbers = List::with_capacity(env.limits.batch);
    for new in &batch {
        numbers.push(new.number).expect("batch admitted");
    }
    let count = numbers.len();
    let mut handed = List::with_capacity(env.limits.batch);
    if let Some(number) = parent {
        let creator = record(domain, number).expect("creator admitted");
        let creator_holds = creator.holds_taken;
        let held = creator.holdings.clone();
        let mut remaining =
            List::with_capacity(env.limits.tasks.checked_mul(env.limits.holdings).expect("bounded live holds"));
        for holding in &held {
            let mut to = None;
            for new in &batch {
                if !creator_holds {
                    break;
                }
                for wanted in &new.holdings {
                    if wanted == holding {
                        to = Some(new.number);
                        break;
                    }
                }
                if to.is_some() {
                    break;
                }
            }
            match to {
                Some(child) => {
                    let mut known = false;
                    for existing in &handed {
                        if *existing == child {
                            known = true;
                            break;
                        }
                    }
                    if !known {
                        handed.push(child).expect("one handed recipient per batch member");
                    }
                }
                None => remaining.push(holding.clone()).expect("retained held set fits"),
            }
        }
        task_mut(domain, number).expect("creator admitted").record.holdings = remaining.into_boxed();
        let old = task_mut(domain, number).expect("creator admitted");
        old.record.delegates = append(env.limits.delegates, &old.record.delegates, &batch);
        // Count each made task in all ancestors, so ending a delegate does not
        // make lifetime tree capacity reappear.
        let mut ancestor = Some(number);
        for _ in 0..env.limits.tasks {
            let Some(number) = ancestor else {
                break;
            };
            let task = task_mut(domain, number).expect("all ancestors of live task are live");
            task.record.made = task.record.made.checked_add(count).expect("tree count admitted");
            ancestor = match task.record.requester {
                Party::Task(parent) => Some(parent),
                Party::Person(_) | Party::Deployment { .. } => None,
            };
            publish(domain, env, number, out);
        }
    }
    for new in batch {
        let number = new.number;
        let task = Task {
            record: TaskRecord {
                created_at: env.wall,
                ended_at: None,
                revision: 0,
                narrowing: false,
                result_position: 0,
                escalation: crate::Escalation::Unheld { revision: 0 },
                proposal: None,
                result_proposal: false,
                number,
                project: new.project,
                requester: creator,
                run_spent: 0,
                run_reserved: 0,
                root: root.unwrap_or(number),
                depth,
                executor: new.executor,
                taken_by: None,
                tracked: new.tracked,
                recurring: match new.recurring {
                    Some(template) => Some(Box::new(crate::RecurringState {
                        template: *template,
                        last_period: 0,
                        pending_period: None,
                    })),
                    None => None,
                },
                spec: new.spec,
                contract: new.contract,
                authority: new.authority,
                numbers: new.numbers,
                funder: new.funder,
                waiting_on: new.dependencies.clone(),
                holdings: new.holdings,
                holds_taken: false,
                hold_wait_since: None,
                dependencies: new.dependencies,
                delegates: Box::new([]),
                references: Box::new([]),
                questions: Box::new([]),
                subscriptions: Box::new([]),
                wake: new.wake,
                turn: 0,
                last_message: 0,
                inbox: Box::new([]),
                saved: Box::new([]),
                ever_turned: false,
                made: 1,
                attempt: 0,
                last_answer: None,
                tries: Tries::NONE,
                invalid_result: None,
                refusals: 0,
                phase: Phase::Waiting,
            },
            alarm: None,
            observed_hold: None,
        };
        let id = domain.tasks.insert(task).expect("batch slab room admitted");
        let indexed = domain.names.insert(number, id);
        assert!(indexed == Ok(None), "batch names admitted");
        let mut inherited = false;
        for child in &handed {
            if *child == number {
                inherited = true;
                break;
            }
        }
        if inherited {
            crate::holds::take(domain, env, number, out);
            task_mut(domain, number).expect("handed child live").record.phase = Phase::Active(Active::Due);
        }
        publish(domain, env, number, out);
        fact(domain, Fact::Made { task: number, requester: creator });
        if inherited {
            activate(domain, number, out);
        }
    }
    numbers.into_boxed()
}

fn append(capacity: u32, old: &[u64], batch: &[New]) -> Box<[u64]> {
    let mut numbers = List::with_capacity(capacity);
    for number in old {
        numbers.push(*number).expect("existing numbers admitted");
    }
    for new in batch {
        numbers.push(new.number).expect("new numbers admitted");
    }
    numbers.into_boxed()
}
