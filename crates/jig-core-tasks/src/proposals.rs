//! Durable proposals owned by live proposers (domain/tasks.md, section 9).
//! The root decides authority, funding and holder coverage. This child keeps
//! one pending proposal per live task, its action and current recipient; it
//! never knows role membership or a worker call identity. Terminal proposals
//! leave live memory as historical store rows.
use crate::domain::{Domain, publish, record, refused, task_mut};
use crate::{Amendment, Authority, Change, History, Limits, New, Party, Phase, Refusal, Request, Stored, Was};
use alloc::boxed::Box;
use skein_lib::{Env, Queue, ReplyTo, Time, Wall};

fn kind(action: &ProposalAction) -> ProposalKind {
    match action {
        ProposalAction::Batch(_) => ProposalKind::Batch,
        ProposalAction::Amend { .. } => ProposalKind::Amend,
        ProposalAction::Widen { .. } => ProposalKind::Widen,
        ProposalAction::Release { .. } => ProposalKind::Release,
    }
}

fn word(proposal: &Proposal, at: Wall) -> crate::Word {
    crate::Word {
        number: proposal.number,
        from: Party::Task(proposal.proposer),
        kind: crate::MessageKind::Proposal {
            proposer: proposal.proposer,
            proposal: proposal.number,
            kind: kind(&proposal.action),
        },
        words: if proposal.reason.is_empty() { Box::from(&b"Proposal"[..]) } else { proposal.reason.clone() },
        at,
        hits: 1,
        eligible: true,
    }
}

/// The holder's decision entry is projected from its proposer's durable row.
/// It never occupies or consumes an inbox reservation.
pub(crate) fn waiting_for(domain: &Domain, holder: u64) -> Box<[crate::Word]> {
    let mut waiting = skein_lib::List::with_capacity(domain.names.len());
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("indexed proposal owner");
        if let Some(proposal) = &task.proposal {
            match proposal.state {
                ProposalState::Pending { holder: ProposalHolder::Task(target), since } if target == holder => {
                    waiting.push(word(proposal, since)).expect("one proposal per live task");
                }
                ProposalState::Pending { .. }
                | ProposalState::Accepted { .. }
                | ProposalState::Rejected { .. }
                | ProposalState::Withdrawn => {}
            }
        }
    }
    waiting.into_boxed()
}

fn wake_holder(domain: &mut Domain, env: &Env<Limits>, proposal: &Proposal, out: &mut Queue<Request>) {
    let (task, since) = match proposal.state {
        ProposalState::Pending { holder: ProposalHolder::Task(task), since } => (task, since),
        ProposalState::Pending { .. }
        | ProposalState::Accepted { .. }
        | ProposalState::Rejected { .. }
        | ProposalState::Withdrawn => return,
    };
    if record(domain, task).is_none() {
        out.push(Request::ProposalStalled {
            proposer: proposal.proposer,
            proposal: proposal.number,
            holder: ProposalHolder::Task(task),
        });
        return;
    }
    crate::wake::after_message(domain, env, task, None, word(proposal, since), out);
}

/// A task action waiting for a holder's authority and funding.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ProposalAction {
    /// Whole batch with IDs allocated on proposal admission, before reservation.
    Batch(Box<[New]>),
    /// Change an existing live task after its holder accepts the needed authority.
    Amend { task: u64, amendment: Amendment },
    /// Widen a task's current authority.
    Widen { task: u64, authority: Authority },
    /// Release a held task whose decision needs the holder.
    Release { task: u64 },
}

/// Current decision recipient, selected by root from the nearest covering holder.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProposalHolder {
    /// Ancestor task.
    Task(u64),
    /// Actual person requester above the root.
    Person(u64),
    /// People whose project role permits this kind of decision; there is no further holder.
    Policy { project: u32, kind: ProposalKind },
}

/// Decision family used for the final policy recipient.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProposalKind {
    Batch,
    Amend,
    Widen,
    Release,
}

/// One proposal's current or historical decision state.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ProposalState {
    /// No holder has decided yet.
    Pending { holder: ProposalHolder, since: Wall },
    /// Accepted by a covered holder in the same commit as its action.
    Accepted { by: Party },
    /// Rejected with words delivered to the proposer.
    Rejected { by: Party, reason: Box<[u8]> },
    /// Proposer withdrew it or its live basis closed.
    Withdrawn,
}

/// Root-numbered proposal whose action and reason remain durable.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Proposal {
    pub number: u64,
    pub proposer: u64,
    pub project: u32,
    pub action: ProposalAction,
    pub reason: Box<[u8]>,
    pub as_holder: bool,
    pub state: ProposalState,
}

/// A person's pending goal request, kept separately from task-origin proposals.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PersonProposal {
    pub number: u64,
    pub proposer: u64,
    pub project: u32,
    pub goal: New,
    pub state: PersonProposalState,
}

/// Durable decision on a person-origin goal; terminal rows remain in the store.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PersonProposalState {
    Pending { since: Wall },
    Accepted { by: Party },
    Rejected { by: Party, reason: Box<[u8]>, message: u64, at: Wall },
}

/// A restored or newly proposed goal has the same bounded shape as a future root make.
pub(crate) fn valid_person_proposal(proposal: &PersonProposal, limits: &Limits) -> bool {
    if proposal.number == 0
        || proposal.proposer == 0
        || proposal.project == 0
        || proposal.goal.number == 0
        || proposal.goal.project != proposal.project
        || proposal.goal.tracked.is_none()
        || proposal.goal.recurring.is_some()
        || !proposal.goal.dependencies.is_empty()
        || !crate::valid_spec(limits, &proposal.goal.spec)
        || !crate::valid_contract(limits, &proposal.goal.contract)
        || !crate::valid_authority(limits, &proposal.goal.authority)
        || !crate::wake::valid(&proposal.goal.wake)
        || proposal.goal.numbers.budget == 0
        || proposal.goal.numbers.spent != 0
        || proposal.goal.numbers.spent_below != 0
        || proposal.goal.numbers.reserved != 0
    {
        return false;
    }
    let executor = match proposal.goal.executor {
        crate::Executor::Agent { charter } => charter != 0,
        crate::Executor::Person(_) | crate::Executor::Procedure { .. } => false,
    };
    let funder = match proposal.goal.funder {
        crate::Funder::Pool { project, person, .. } => project == proposal.project && person == proposal.proposer,
        crate::Funder::Task(_) | crate::Funder::Period { .. } | crate::Funder::Recurring { .. } => false,
    };
    let state = match &proposal.state {
        PersonProposalState::Pending { .. } => true,
        PersonProposalState::Accepted { by } => match by {
            Party::Person(person) => *person != 0,
            Party::Task(_) | Party::Deployment { .. } => false,
        },
        PersonProposalState::Rejected { by, reason, message, .. } => match by {
            Party::Person(person) => {
                *person != 0
                    && *message != 0
                    && reason.len() <= usize::try_from(limits.message_bytes).expect("u32 fits usize")
            }
            Party::Task(_) | Party::Deployment { .. } => false,
        },
    };
    executor && funder && state
}

/// Admit one policy-bound person proposal without creating its goal task.
pub(crate) fn propose_person(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    proposal: PersonProposal,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    if !valid_person_proposal(&proposal, &env.limits)
        || match proposal.state {
            PersonProposalState::Pending { .. } => false,
            PersonProposalState::Accepted { .. } | PersonProposalState::Rejected { .. } => true,
        }
    {
        return refused(to, None, Refusal::Read, out);
    }
    if domain.person_proposals.len() >= env.limits.tasks || domain.person_proposals.contains_key(&proposal.number) {
        return refused(to, None, Refusal::Busy, out);
    }
    for (_, old) in &domain.person_proposals {
        if old.proposer == proposal.proposer {
            return refused(to, None, Refusal::Busy, out);
        }
    }
    let number = proposal.number;
    let saved = domain.person_proposals.insert(number, proposal.clone());
    assert!(saved == Ok(None), "admitted person proposal has room");
    out.push(Request::Save { record: Stored::PersonProposal(Box::new(proposal)) });
    out.push(Request::PersonProposed { reply_to: to, proposal: number });
}

/// Finish one exact policy decision after root has made an accepted goal.
#[expect(clippy::too_many_arguments, reason = "one exact person proposal decision and its output")]
pub(crate) fn decide_person(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    proposer: u64,
    number: u64,
    by: Party,
    message: Option<u64>,
    decision: ProposalDecision,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    let Some(old) = domain.person_proposals.get(&number) else {
        return out.push(Request::PersonProposalDecided {
            reply_to: to,
            proposer,
            number,
            outcome: ProposalOutcome::Stale,
        });
    };
    if old.proposer != proposer {
        return refused(to, None, Refusal::Unknown, out);
    }
    if match old.state {
        PersonProposalState::Pending { .. } => false,
        PersonProposalState::Accepted { .. } | PersonProposalState::Rejected { .. } => true,
    } {
        return out.push(Request::PersonProposalDecided {
            reply_to: to,
            proposer,
            number,
            outcome: ProposalOutcome::Stale,
        });
    }
    let outcome = match &decision {
        ProposalDecision::Accept => {
            if record(domain, old.goal.number).is_none() {
                return refused(to, None, Refusal::State, out);
            }
            ProposalOutcome::Accepted
        }
        ProposalDecision::Reject { reason } => {
            if reason.len() > usize::try_from(env.limits.message_bytes).expect("u32 fits usize")
                || match message {
                    Some(number) => number == 0,
                    None => true,
                }
            {
                return refused(to, None, Refusal::Read, out);
            }
            ProposalOutcome::Rejected
        }
        ProposalDecision::Pass { .. } => return refused(to, None, Refusal::State, out),
    };
    let mut proposal = domain.person_proposals.remove(&number).expect("checked pending proposal");
    proposal.state = match decision {
        ProposalDecision::Accept => PersonProposalState::Accepted { by },
        ProposalDecision::Reject { reason } => PersonProposalState::Rejected {
            by,
            reason,
            message: message.expect("checked rejection message"),
            at: env.wall,
        },
        ProposalDecision::Pass { .. } => unreachable!("person proposal has final policy holder"),
    };
    out.push(Request::Save { record: Stored::PersonProposal(Box::new(proposal)) });
    out.push(Request::PersonProposalDecided { reply_to: to, proposer, number, outcome });
}

/// Root-authenticated decision for the current holder.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ProposalDecision {
    Accept,
    Reject { reason: Box<[u8]> },
    Pass { holder: ProposalHolder },
}

/// Child terminal for an exact proposal decision.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProposalOutcome {
    Accepted,
    Rejected,
    Passed,
    Withdrawn,
    Stale,
}

/// Borrowed current proposal clone for root's short-lived authority decision.
#[must_use]
pub fn context(domain: &Domain, proposer: u64, number: u64) -> Option<Proposal> {
    let task = record(domain, proposer)?;
    match &task.proposal {
        Some(proposal) if proposal.number == number => Some((**proposal).clone()),
        Some(_) | None => None,
    }
}

fn history(domain: &mut Domain, proposal: Proposal, by: Party, change: Change, out: &mut Queue<Request>) {
    let task = task_mut(domain, proposal.proposer).expect("proposal task live");
    task.record.revision = task.record.revision.checked_add(1).expect("proposal revision preflighted");
    out.push(Request::Save {
        record: Stored::History(History {
            task: proposal.proposer,
            revision: task.record.revision,
            by,
            change,
            reason: proposal.reason.clone(),
            proposal: Some(Box::new(proposal)),
        }),
    });
}

fn arm(domain: &mut Domain, env: &Env<Limits>, proposer: u64, since: Wall) {
    let until = since.as_nanos().saturating_add(env.limits.proposal_stall.as_nanos());
    let remaining = until.saturating_sub(env.wall.as_nanos());
    let due = Time::from_nanos(env.now.as_nanos().saturating_add(remaining));
    assert!(domain.proposal_alarms.arm(proposer, due).is_ok(), "one pending proposal per task");
}

/// Reproject durable pending waits from wall time after complete live restore.
pub(crate) fn rearm_all(domain: &mut Domain, env: &Env<Limits>) {
    let mut pending = skein_lib::List::with_capacity(env.limits.tasks);
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("live proposal owner");
        if let Some(proposal) = &task.proposal {
            match proposal.state {
                ProposalState::Pending { holder: ProposalHolder::Task(_) | ProposalHolder::Person(_), since } => {
                    pending.push((*number, since)).expect("pending tasks bounded");
                }
                ProposalState::Pending { holder: ProposalHolder::Policy { .. }, .. }
                | ProposalState::Accepted { .. }
                | ProposalState::Rejected { .. }
                | ProposalState::Withdrawn => {}
            }
        }
    }
    for &(number, since) in &pending {
        arm(domain, env, number, since);
    }
}

/// Once restoration has rearmed deadlines, make every durable task-holder
/// entry visible again to an idle or adopted run.
pub(crate) fn wake_restored(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let mut waiting = skein_lib::List::with_capacity(env.limits.tasks);
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("restored name live");
        if let Some(proposal) = &task.proposal {
            match proposal.state {
                ProposalState::Pending { holder: ProposalHolder::Task(_), .. } => {
                    waiting.push((*number, proposal.number)).expect("one pending proposal per task");
                }
                ProposalState::Pending { .. }
                | ProposalState::Accepted { .. }
                | ProposalState::Rejected { .. }
                | ProposalState::Withdrawn => {}
            }
        }
    }
    for &(proposer, number) in &waiting {
        let proposal = context(domain, proposer, number).expect("restored pending proposal");
        wake_holder(domain, env, &proposal, out);
    }
}

/// A holder that becomes held or closing cannot decide its waiting entries.
pub(crate) fn holder_unavailable(domain: &Domain, holder: u64, out: &mut Queue<Request>) {
    for (proposer, _) in &domain.names {
        let task = record(domain, *proposer).expect("indexed proposal owner");
        if let Some(proposal) = &task.proposal {
            match proposal.state {
                ProposalState::Pending { holder: ProposalHolder::Task(target), .. } if target == holder => {
                    out.push(Request::ProposalStalled {
                        proposer: *proposer,
                        proposal: proposal.number,
                        holder: ProposalHolder::Task(holder),
                    });
                }
                ProposalState::Pending { .. }
                | ProposalState::Accepted { .. }
                | ProposalState::Rejected { .. }
                | ProposalState::Withdrawn => {}
            }
        }
    }
}

/// Expire one configured nonfinal wait; root computes fresh coverage before
/// returning `StalledProposal` in the same decision.
pub(crate) fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some(proposer) = domain.proposal_alarms.expire(env.now) else { return };
    let Some(task) = record(domain, proposer) else { return };
    let Some(proposal) = &task.proposal else { return };
    match proposal.state {
        ProposalState::Pending { holder: holder @ (ProposalHolder::Task(_) | ProposalHolder::Person(_)), .. } => {
            out.push(Request::ProposalStalled { proposer, proposal: proposal.number, holder });
        }
        ProposalState::Pending { holder: ProposalHolder::Policy { .. }, .. }
        | ProposalState::Accepted { .. }
        | ProposalState::Rejected { .. }
        | ProposalState::Withdrawn => {}
    }
}

/// Apply root's fresh next-holder choice only if the exact old proposal still waits.
#[expect(clippy::too_many_arguments, reason = "one reroute carries its old holder, revision and replacement")]
pub(crate) fn stalled(
    domain: &mut Domain,
    env: &Env<Limits>,
    proposer: u64,
    number: u64,
    from: ProposalHolder,
    revision: u64,
    holder: ProposalHolder,
    out: &mut Queue<Request>,
) {
    let Some(mut proposal) = context(domain, proposer, number) else { return };
    let current = match proposal.state {
        ProposalState::Pending { holder, .. } => holder,
        ProposalState::Accepted { .. } | ProposalState::Rejected { .. } | ProposalState::Withdrawn => return,
    };
    let task_revision = record(domain, proposer).expect("live proposal").revision;
    if current != from || task_revision != revision || current == holder || task_revision == u64::MAX {
        return;
    }
    proposal.state = ProposalState::Pending { holder, since: env.wall };
    task_mut(domain, proposer).expect("live proposal").record.proposal = Some(Box::new(proposal.clone()));
    history(domain, proposal, Party::Task(proposer), Change::ProposalPassed, out);
    publish(domain, env, proposer, out);
    match holder {
        ProposalHolder::Task(_) | ProposalHolder::Person(_) => arm(domain, env, proposer, env.wall),
        ProposalHolder::Policy { .. } => domain.proposal_alarms.cancel(proposer),
    }
    let pending = context(domain, proposer, number).expect("rerouted proposal live");
    wake_holder(domain, env, &pending, out);
}

/// Admit a checked action once for a live proposer, with no source reservation.
pub(crate) fn propose(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    proposal: Proposal,
    out: &mut Queue<Request>,
) {
    if proposal.number == 0
        || proposal.proposer == 0
        || proposal.reason.len() > usize::try_from(env.limits.message_bytes).expect("u32 fits usize")
    {
        return refused(to, Some(proposal.proposer), Refusal::Read, out);
    }
    let Some(task) = record(domain, proposal.proposer) else {
        return refused(to, Some(proposal.proposer), Refusal::Unknown, out);
    };
    if task.project != proposal.project || task.proposal.is_some() || task.revision == u64::MAX {
        return refused(to, Some(proposal.proposer), Refusal::Busy, out);
    }
    if !crate::inbox::room(
        domain,
        &env.limits,
        proposal.proposer,
        1,
        usize::try_from(env.limits.message_bytes).expect("u32 fits usize"),
    ) {
        return refused(to, Some(proposal.proposer), Refusal::Busy, out);
    }
    match task.phase {
        Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => {}
        Phase::Closing(_) | Phase::Held { was: Was::Closing(_), .. } | Phase::Ended(_) => {
            return refused(to, Some(proposal.proposer), Refusal::State, out);
        }
    }
    match proposal.state {
        ProposalState::Pending { .. } => {}
        ProposalState::Accepted { .. } | ProposalState::Rejected { .. } | ProposalState::Withdrawn => {
            return refused(to, Some(proposal.proposer), Refusal::State, out);
        }
    }
    install(domain, env, proposal, false, out);
    out.push(Request::Done { reply_to: to });
}

/// Check the one proposal a verdict creates before any result expense is posted.
pub(crate) fn check_result(domain: &Domain, env: &Env<Limits>, task: u64, proposal: &Proposal) -> bool {
    if proposal.number == 0
        || proposal.proposer != task
        || proposal.as_holder
        || proposal.reason.len() > usize::try_from(env.limits.message_bytes).expect("u32 fits usize")
    {
        return false;
    }
    let Some(row) = record(domain, task) else { return false };
    if row.project != proposal.project || row.proposal.is_some() || row.revision == u64::MAX {
        return false;
    }
    if !crate::inbox::room(
        domain,
        &env.limits,
        task,
        1,
        usize::try_from(env.limits.message_bytes).expect("u32 fits usize"),
    ) {
        return false;
    }
    match (&row.phase, &proposal.action, &proposal.state) {
        (Phase::Active(_), ProposalAction::Batch(batch), ProposalState::Pending { holder, .. }) => {
            let holder_valid = match holder {
                ProposalHolder::Task(number) => *number != 0 && *number != task,
                ProposalHolder::Person(number) => *number != 0,
                ProposalHolder::Policy { project, kind } => *project == row.project && *kind == ProposalKind::Batch,
            };
            if !holder_valid
                || batch.is_empty()
                || batch.len() > usize::try_from(env.limits.batch).expect("u32 fits usize")
            {
                return false;
            }
            for (at, member) in batch.iter().enumerate() {
                if member.number == 0
                    || member.project != row.project
                    || member.funder != crate::Funder::Task(task)
                    || !crate::batch::valid_spec(&env.limits, &member.spec)
                    || !member.spec.inputs.is_empty()
                    || !crate::batch::valid_contract(&env.limits, &member.contract)
                    || !crate::batch::valid_authority(&env.limits, &member.authority)
                    || !crate::wake::valid(&member.wake)
                    || member.dependencies.len() > usize::try_from(env.limits.dependencies).expect("u32 fits usize")
                {
                    return false;
                }
                for earlier in batch.iter().take(at) {
                    if earlier.number == member.number {
                        return false;
                    }
                }
            }
            true
        }
        (Phase::Waiting | Phase::Closing(_) | Phase::Held { .. } | Phase::Ended(_), _, _)
        | (
            Phase::Active(_),
            ProposalAction::Amend { .. } | ProposalAction::Widen { .. } | ProposalAction::Release { .. },
            _,
        )
        | (
            Phase::Active(_),
            _,
            ProposalState::Accepted { .. } | ProposalState::Rejected { .. } | ProposalState::Withdrawn,
        ) => false,
    }
}

/// Install a preflighted result proposal in the same decision that takes its verdict.
pub(crate) fn propose_result(domain: &mut Domain, env: &Env<Limits>, proposal: Proposal, out: &mut Queue<Request>) {
    install(domain, env, proposal, true, out);
}

fn install(domain: &mut Domain, env: &Env<Limits>, proposal: Proposal, result: bool, out: &mut Queue<Request>) {
    let number = proposal.proposer;
    let proposal_id = proposal.number;
    let row = &mut task_mut(domain, number).expect("proposer live").record;
    row.proposal = Some(Box::new(proposal.clone()));
    row.result_proposal = result;
    history(domain, proposal, Party::Task(number), Change::Proposed, out);
    publish(domain, env, number, out);
    let pending = context(domain, number, proposal_id).expect("proposal admitted");
    match pending.state {
        ProposalState::Pending { holder: ProposalHolder::Task(_) | ProposalHolder::Person(_), since } => {
            arm(domain, env, number, since);
        }
        ProposalState::Pending { holder: ProposalHolder::Policy { .. }, .. } => {}
        ProposalState::Accepted { .. } | ProposalState::Rejected { .. } | ProposalState::Withdrawn => {
            unreachable!("pending admission")
        }
    }
    wake_holder(domain, env, &pending, out);
}

/// Advance the checked holder or finish the decision. Root executes an accepted
/// action before calling this entry in the same synchronous decision.
#[expect(clippy::too_many_arguments, reason = "one exact proposal decision carries its caller and message")]
#[expect(clippy::too_many_lines, reason = "one proposal terminal preflights, persists, and notifies atomically")]
pub(crate) fn decide(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    proposer: u64,
    number: u64,
    message: Option<u64>,
    by: Party,
    decision: ProposalDecision,
    out: &mut Queue<Request>,
) {
    let Some(mut proposal) = context(domain, proposer, number) else {
        return out.push(Request::ProposalDecided { reply_to: to, proposer, number, outcome: ProposalOutcome::Stale });
    };
    if !holder_is(proposal.state.clone(), by) {
        return refused(to, Some(proposer), Refusal::Reference, out);
    }
    if record(domain, proposer).expect("pending proposer live").revision == u64::MAX {
        return refused(to, Some(proposer), Refusal::State, out);
    }
    let last_message = record(domain, proposer).expect("pending proposer live").last_message;
    match (&decision, message) {
        (ProposalDecision::Accept | ProposalDecision::Reject { .. }, Some(number)) if number > last_message => {}
        (ProposalDecision::Pass { .. }, None) => {}
        (ProposalDecision::Accept | ProposalDecision::Reject { .. } | ProposalDecision::Pass { .. }, _) => {
            return refused(to, Some(proposer), Refusal::Read, out);
        }
    }
    let words = match &decision {
        ProposalDecision::Reject { reason } => reason.clone(),
        ProposalDecision::Accept | ProposalDecision::Pass { .. } => Box::new([]),
    };
    let outcome = match decision {
        ProposalDecision::Accept => {
            proposal.state = ProposalState::Accepted { by };
            ProposalOutcome::Accepted
        }
        ProposalDecision::Reject { reason } => {
            if reason.len() > usize::try_from(env.limits.message_bytes).expect("u32 fits usize") {
                return refused(to, Some(proposer), Refusal::Read, out);
            }
            proposal.state = ProposalState::Rejected { by, reason };
            ProposalOutcome::Rejected
        }
        ProposalDecision::Pass { holder } => {
            proposal.state = ProposalState::Pending { holder, since: env.wall };
            ProposalOutcome::Passed
        }
    };
    let task = task_mut(domain, proposer).expect("pending proposer live");
    task.record.proposal = match outcome {
        ProposalOutcome::Passed => Some(Box::new(proposal.clone())),
        ProposalOutcome::Accepted | ProposalOutcome::Rejected | ProposalOutcome::Withdrawn | ProposalOutcome::Stale => {
            None
        }
    };
    if match outcome {
        ProposalOutcome::Accepted | ProposalOutcome::Rejected => true,
        ProposalOutcome::Passed | ProposalOutcome::Withdrawn | ProposalOutcome::Stale => false,
    } {
        task.record.result_proposal = false;
    }
    let change = match outcome {
        ProposalOutcome::Accepted => Change::ProposalAccepted,
        ProposalOutcome::Rejected => Change::ProposalRejected,
        ProposalOutcome::Passed => Change::ProposalPassed,
        ProposalOutcome::Withdrawn | ProposalOutcome::Stale => unreachable!("decision outcome"),
    };
    history(domain, proposal, by, change, out);
    publish(domain, env, proposer, out);
    match outcome {
        ProposalOutcome::Passed => {
            let pending = record(domain, proposer).expect("passed proposer live").proposal.as_ref().expect("pending");
            match pending.state {
                ProposalState::Pending { holder: ProposalHolder::Task(_) | ProposalHolder::Person(_), since } => {
                    arm(domain, env, proposer, since);
                }
                ProposalState::Pending { holder: ProposalHolder::Policy { .. }, .. } => {
                    domain.proposal_alarms.cancel(proposer);
                }
                ProposalState::Accepted { .. } | ProposalState::Rejected { .. } | ProposalState::Withdrawn => {
                    unreachable!("passed pending")
                }
            }
        }
        ProposalOutcome::Accepted | ProposalOutcome::Rejected | ProposalOutcome::Withdrawn | ProposalOutcome::Stale => {
            domain.proposal_alarms.cancel(proposer);
        }
    }
    match outcome {
        ProposalOutcome::Accepted | ProposalOutcome::Rejected => {
            let message = message.expect("terminal message preflighted");
            crate::inbox::proposal_decision(
                domain,
                env,
                proposer,
                crate::Word {
                    number: message,
                    from: by,
                    kind: crate::MessageKind::ProposalDecision {
                        proposal: number,
                        accepted: outcome == ProposalOutcome::Accepted,
                    },
                    words,
                    at: env.wall,
                    hits: 1,
                    eligible: true,
                },
                out,
            );
        }
        ProposalOutcome::Passed | ProposalOutcome::Withdrawn | ProposalOutcome::Stale => {}
    }
    if outcome == ProposalOutcome::Passed {
        let pending = context(domain, proposer, number).expect("passed proposal live");
        wake_holder(domain, env, &pending, out);
    }
    out.push(Request::ProposalDecided { reply_to: to, proposer, number, outcome });
}

fn holder_is(state: ProposalState, by: Party) -> bool {
    match state {
        ProposalState::Pending { holder: ProposalHolder::Task(task), .. } => by == Party::Task(task),
        ProposalState::Pending { holder: ProposalHolder::Person(person), .. } => by == Party::Person(person),
        ProposalState::Pending { holder: ProposalHolder::Policy { .. }, .. } => match by {
            Party::Person(_) => true,
            Party::Task(_) | Party::Deployment { .. } => false,
        },
        ProposalState::Accepted { .. } | ProposalState::Rejected { .. } | ProposalState::Withdrawn => false,
    }
}

/// Closing withdraws other pending proposals; a result proposal survives until decision.
pub(crate) fn withdraw_on_close(domain: &mut Domain, number: u64, out: &mut Queue<Request>) {
    let Some(task) = record(domain, number) else { return };
    let closing = match &task.phase {
        Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. } => match &closing.ending {
            crate::Ending::Cancelled { .. } => true,
            crate::Ending::Done(_) | crate::Ending::Failed { .. } => !task.result_proposal,
        },
        Phase::Waiting
        | Phase::Active(_)
        | Phase::Held { was: Was::Waiting | Was::Active(_), .. }
        | Phase::Ended(_) => false,
    };
    if !closing {
        return;
    }
    let mut proposal = match task_mut(domain, number).expect("closing task live").record.proposal.take() {
        Some(proposal) => *proposal,
        None => return,
    };
    task_mut(domain, number).expect("closing task live").record.result_proposal = false;
    proposal.state = ProposalState::Withdrawn;
    history(domain, proposal, Party::Task(number), Change::ProposalWithdrawn, out);
    domain.proposal_alarms.cancel(number);
}

/// The proposer explicitly withdraws one live pending action.
pub(crate) fn withdraw(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    proposer: u64,
    number: u64,
    out: &mut Queue<Request>,
) {
    let Some(mut proposal) = context(domain, proposer, number) else {
        return out.push(Request::ProposalDecided { reply_to: to, proposer, number, outcome: ProposalOutcome::Stale });
    };
    if record(domain, proposer).expect("pending proposer live").result_proposal {
        return refused(to, Some(proposer), Refusal::State, out);
    }
    if record(domain, proposer).expect("pending proposer live").revision == u64::MAX {
        return refused(to, Some(proposer), Refusal::State, out);
    }
    task_mut(domain, proposer).expect("pending proposer live").record.proposal = None;
    task_mut(domain, proposer).expect("pending proposer live").record.result_proposal = false;
    proposal.state = ProposalState::Withdrawn;
    history(domain, proposal, Party::Task(proposer), Change::ProposalWithdrawn, out);
    domain.proposal_alarms.cancel(proposer);
    publish(domain, env, proposer, out);
    out.push(Request::ProposalDecided { reply_to: to, proposer, number, outcome: ProposalOutcome::Withdrawn });
}
