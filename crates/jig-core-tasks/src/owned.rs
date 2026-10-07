//! Pure borrowed durable-row heap measurement for root journal/load admission
//! (domain/tasks.md, section 2; domain/engine.md, sections 5.3 and 5.6).
//! Measures existing ownership without allocating or cloning; shape admission
//! remains with tasks and authority decisions remain with root policy checks.
use crate::{
    Authority, Contract, Ending, Last, Parameter, PersonProposalState, Phase, Proposal, ProposalAction, ProposalState,
    Spec, Stored, TaskRecord, TaskResult, Was,
};
use core::mem::{size_of, size_of_val};

/// Pure borrowed measurement for root journal/load byte admission: counts existing boxed records,
/// slice backing arrays and nested bytes, excluding the inline `Stored` slot. `Ledger` own no heap.
/// Returns `None` on sum overflow; allocates/copies nothing and admits neither shape nor authority.
/// Root separately counts inline slots and queues.
#[must_use]
pub fn stored_bytes(record: &Stored) -> Option<u64> {
    match record {
        Stored::PersonProposal(row) => {
            let mut total = bytes(size_of::<crate::PersonProposal>())?
                .checked_add(spec_bytes(&row.goal.spec)?)?
                .checked_add(authority_bytes(&row.goal.authority)?)?
                .checked_add(contract_bytes(&row.goal.contract)?)?
                .checked_add(bytes(size_of_val(&*row.goal.dependencies))?)?;
            match &row.state {
                PersonProposalState::Rejected { reason, .. } => {
                    total = total.checked_add(bytes(reason.len())?)?;
                }
                PersonProposalState::Pending { .. } | PersonProposalState::Accepted { .. } => {}
            }
            Some(total)
        }
        Stored::History(row) => {
            let mut total = bytes(row.reason.len())?;
            if let Some(proposal) = &row.proposal {
                total = total.checked_add(proposal_bytes(proposal)?)?;
            }
            Some(total)
        }
        Stored::Live(task) | Stored::Ended(task) => task_bytes(task),
        Stored::Ledger(_) => Some(0),
    }
}

fn bytes(length: usize) -> Option<u64> {
    u64::try_from(length).ok()
}

fn spec_bytes(spec: &Spec) -> Option<u64> {
    let mut total = bytes(spec.words.len())?
        .checked_add(bytes(size_of_val(&*spec.parameters))?)?
        .checked_add(bytes(size_of_val(&*spec.inputs))?)?;
    for parameter in &spec.parameters {
        match parameter {
            Parameter::Bytes { value, .. } => total = total.checked_add(bytes(value.len())?)?,
            Parameter::Number { .. } | Parameter::Resource { .. } => {}
        }
    }
    Some(total)
}

fn authority_bytes(authority: &Authority) -> Option<u64> {
    let mut total = bytes(size_of_val(&*authority.grants))?
        .checked_add(bytes(size_of_val(&*authority.note_resources))?)?
        .checked_add(bytes(size_of_val(&*authority.delegation.kinds))?)?;
    for grant in &authority.grants {
        total = total.checked_add(bytes(size_of_val(&*grant.pattern.segments))?)?;
        for segment in &grant.pattern.segments {
            total = total.checked_add(bytes(segment.len())?)?;
        }
        match &grant.pattern.last {
            Last::Exact(word) | Last::Open(word) => total = total.checked_add(bytes(word.len())?)?,
        }
    }
    for scope in &authority.note_resources {
        total = total.checked_add(bytes(size_of_val(&*scope.pattern.segments))?)?;
        for segment in &scope.pattern.segments {
            total = total.checked_add(bytes(segment.len())?)?;
        }
        match &scope.pattern.last {
            Last::Exact(word) | Last::Open(word) => total = total.checked_add(bytes(word.len())?)?,
        }
    }
    Some(total)
}

fn contract_bytes(contract: &Contract) -> Option<u64> {
    match contract {
        Contract::Verdict { choices } => bytes(size_of_val(&**choices)),
        Contract::Report { .. } | Contract::Change { .. } => Some(0),
    }
}

fn result_bytes(result: &TaskResult) -> Option<u64> {
    match result {
        TaskResult::Report { words } | TaskResult::Verdict { words, .. } | TaskResult::Change { words, .. } => {
            bytes(words.len())
        }
        TaskResult::Failure { reason } => bytes(reason.len()),
    }
}

fn ending_bytes(ending: &Ending) -> Option<u64> {
    match ending {
        Ending::Done(result) => result_bytes(result),
        Ending::Failed { reason } => bytes(reason.len()),
        Ending::Cancelled { reason, result } => {
            let reason = bytes(reason.len())?;
            match result {
                Some(result) => reason.checked_add(result_bytes(result)?),
                None => Some(reason),
            }
        }
    }
}

fn phase_bytes(phase: &Phase) -> Option<u64> {
    match phase {
        Phase::Closing(closing) => ending_bytes(&closing.ending),
        Phase::Ended(ending) => ending_bytes(ending),
        Phase::Held { was, .. } => match was {
            Was::Closing(closing) => ending_bytes(&closing.ending),
            Was::Waiting | Was::Active(_) => Some(0),
        },
        Phase::Waiting | Phase::Active(_) => Some(0),
    }
}

fn proposal_bytes(proposal: &Proposal) -> Option<u64> {
    let mut total = bytes(size_of::<Proposal>())?.checked_add(bytes(proposal.reason.len())?)?;
    match &proposal.state {
        ProposalState::Rejected { reason, .. } => {
            total = total.checked_add(bytes(reason.len())?)?;
        }
        ProposalState::Pending { .. } | ProposalState::Accepted { .. } | ProposalState::Withdrawn => {}
    }
    match &proposal.action {
        ProposalAction::Batch(batch) => {
            total = total.checked_add(bytes(size_of_val(&**batch))?)?;
            for member in batch {
                total = total
                    .checked_add(spec_bytes(&member.spec)?)?
                    .checked_add(authority_bytes(&member.authority)?)?
                    .checked_add(contract_bytes(&member.contract)?)?
                    .checked_add(bytes(size_of_val(&*member.dependencies))?)?;
            }
        }
        ProposalAction::Amend { amendment, .. } => {
            total = total.checked_add(bytes(amendment.reason.len())?)?;
            if let Some(spec) = &amendment.spec {
                total = total.checked_add(spec_bytes(spec)?)?;
            }
            if let Some(authority) = &amendment.authority {
                total = total.checked_add(authority_bytes(authority)?)?;
            }
            if let Some(dependencies) = &amendment.dependencies {
                total = total.checked_add(bytes(size_of_val(&**dependencies))?)?;
            }
        }
        ProposalAction::Widen { authority, .. } => total = total.checked_add(authority_bytes(authority)?)?,
        ProposalAction::Release { .. } => {}
    }
    Some(total)
}

fn task_bytes(task: &TaskRecord) -> Option<u64> {
    let mut inbox_bytes = bytes(size_of_val(&*task.inbox))?;
    for word in &task.inbox {
        inbox_bytes = inbox_bytes.checked_add(bytes(word.words.len())?)?;
    }
    bytes(size_of::<TaskRecord>())?
        .checked_add(match &task.recurring {
            Some(state) => {
                let mut total = bytes(size_of::<crate::RecurringState>())?
                    .checked_add(bytes(size_of_val(&*state.template.batch))?)?;
                for member in &state.template.batch {
                    total = total
                        .checked_add(spec_bytes(&member.spec)?)?
                        .checked_add(authority_bytes(&member.authority)?)?
                        .checked_add(contract_bytes(&member.contract)?)?
                        .checked_add(bytes(size_of_val(&*member.dependencies))?)?;
                }
                total
            }
            None => 0,
        })?
        .checked_add(spec_bytes(&task.spec)?)?
        .checked_add(authority_bytes(&task.authority)?)?
        .checked_add(contract_bytes(&task.contract)?)?
        .checked_add(phase_bytes(&task.phase)?)?
        .checked_add(match &task.escalation {
            crate::Escalation::Rejected { reason, .. } => bytes(reason.len())?,
            crate::Escalation::Unheld { .. }
            | crate::Escalation::Routing { .. }
            | crate::Escalation::Waiting { .. } => 0,
        })?
        .checked_add(match &task.proposal {
            Some(proposal) => proposal_bytes(proposal)?,
            None => 0,
        })?
        .checked_add(bytes(size_of_val(&*task.dependencies))?)?
        .checked_add(bytes(size_of_val(&*task.delegates))?)?
        .checked_add(bytes(size_of_val(&*task.references))?)?
        .checked_add(bytes(size_of_val(&*task.questions))?)?
        .checked_add(bytes(size_of_val(&*task.subscriptions))?)?
        .checked_add(bytes(size_of_val(&*task.waiting_on))?)?
        .checked_add(inbox_bytes)?
        .checked_add(bytes(size_of_val(&*task.saved))?)?
        .checked_add(task.saved.iter().try_fold(0u64, |total, resource| {
            let mut held = total.checked_add(bytes(size_of_val(&*resource.path))?)?;
            for segment in &resource.path {
                held = held.checked_add(bytes(segment.len())?)?;
            }
            Some(held)
        })?)
}
