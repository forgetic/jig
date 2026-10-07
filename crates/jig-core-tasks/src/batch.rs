//! Whole-batch structural and financial admission (domain/tasks.md, sections 4 and 10). Bounded immutable dependencies and delegation waits are checked
//! together before mutation; tasks does not judge root-authorized authority.
use crate::domain::{Domain, record};
use crate::{Authority, Contract, Executor, Last, Limits, New, Parameter, Party, Phase, Problem, Refusal, Spec, Was};
use skein_lib::List;

fn problem(task: Option<u64>, why: Refusal) -> Problem {
    Problem { task, why, blocked_by: None }
}

pub(crate) fn check(domain: &Domain, limits: &Limits, creator: Party, batch: &[New]) -> Result<(), Problem> {
    check_with_result_proposal(domain, limits, creator, batch, None, None)
}

/// Preflight an agent verdict's own delegates after its current run reservation and charge settle.
pub(crate) fn check_direct_followups(
    domain: &Domain,
    limits: &Limits,
    proposer: u64,
    charge: u64,
    batch: &[New],
) -> Result<(), Problem> {
    check_with_result_proposal(domain, limits, Party::Task(proposer), batch, None, Some((proposer, charge)))
}

/// A result proposal may admit its accepted batch while the proposer closes.
pub(crate) fn check_result_followups(
    domain: &Domain,
    limits: &Limits,
    creator: Party,
    proposal: u64,
    batch: &[New],
) -> Result<(), Problem> {
    check_with_result_proposal(domain, limits, creator, batch, Some(proposal), None)
}

fn check_with_result_proposal(
    domain: &Domain,
    limits: &Limits,
    creator: Party,
    batch: &[New],
    result_proposal: Option<u64>,
    direct_finish: Option<(u64, u64)>,
) -> Result<(), Problem> {
    let size = u32::try_from(batch.len()).unwrap_or(u32::MAX);
    if size == 0 {
        return Err(problem(None, Refusal::Empty));
    }
    if size > limits.batch {
        return Err(problem(None, Refusal::Batch));
    }
    if domain.names.len().saturating_add(size) > limits.tasks
        || domain.tasks.capacity().saturating_sub(domain.tasks.len()) < size
    {
        return Err(problem(None, Refusal::Live));
    }
    let parent = match creator {
        Party::Task(number) => {
            let Some(parent) = record(domain, number) else {
                return Err(problem(Some(number), Refusal::Unknown));
            };
            if let Some(proposal) = result_proposal
                && !matches_result_action(parent, proposal, batch)
            {
                return Err(problem(Some(number), Refusal::Reference));
            }
            match &parent.phase {
                Phase::Closing(_)
                    if result_proposal.is_some()
                        && parent.result_proposal
                        && result_proposal_number(parent) == result_proposal => {}
                Phase::Closing(_) | Phase::Ended(_) => return Err(problem(Some(number), Refusal::State)),
                Phase::Held { was, .. } => match was {
                    Was::Closing(_)
                        if result_proposal.is_some()
                            && parent.result_proposal
                            && result_proposal_number(parent) == result_proposal => {}
                    Was::Closing(_) => return Err(problem(Some(number), Refusal::State)),
                    Was::Waiting | Was::Active(_) => {}
                },
                Phase::Waiting | Phase::Active(_) => {}
            }
            if u32::try_from(parent.delegates.len()).unwrap_or(u32::MAX).saturating_add(size) > limits.delegates {
                return Err(problem(Some(number), Refusal::Delegates));
            }
            let Some(result_bytes) =
                usize::try_from(limits.result_bytes).expect("u32 fits usize").checked_mul(batch.len())
            else {
                return Err(problem(Some(number), Refusal::Busy));
            };
            if !crate::inbox::room(domain, limits, number, size, result_bytes) {
                return Err(problem(Some(number), Refusal::Busy));
            }
            if parent.depth.saturating_add(1) > limits.depth {
                return Err(problem(Some(number), Refusal::Depth));
            }
            let root = record(domain, parent.root).expect("a live task's root is live");
            if match root.made.checked_add(size) {
                Some(total) => total > limits.tree_tasks,
                None => true,
            } {
                return Err(problem(Some(number), Refusal::Tree));
            }
            Some(parent)
        }
        Party::Person(_) | Party::Deployment { .. } => None,
    };
    check_members(domain, limits, creator, batch, parent)?;
    crate::holds::check_batch(domain, limits, batch)?;
    if !crate::funders::can_reserve(domain, creator, batch, result_proposal, direct_finish) {
        return Err(problem(None, Refusal::Funding));
    }
    if !acyclic(domain, limits, creator, batch) {
        return Err(problem(Some(batch.first().expect("nonempty batch admitted").number), Refusal::Cycle));
    }
    Ok(())
}

fn matches_result_action(parent: &crate::TaskRecord, number: u64, batch: &[New]) -> bool {
    let proposed = match &parent.proposal {
        Some(proposal) if parent.result_proposal && proposal.number == number => match &proposal.action {
            crate::ProposalAction::Batch(members) => members,
            crate::ProposalAction::Amend { .. }
            | crate::ProposalAction::Widen { .. }
            | crate::ProposalAction::Release { .. } => return false,
        },
        Some(_) | None => return false,
    };
    if proposed.len() != batch.len() {
        return false;
    }
    for (old, new) in proposed.iter().zip(batch) {
        if old.number != new.number
            || old.project != new.project
            || old.executor != new.executor
            || old.spec != new.spec
            || old.contract != new.contract
            || old.authority != new.authority
            || old.numbers != new.numbers
            || old.dependencies != new.dependencies
            || old.holdings != new.holdings
            || old.wake != new.wake
            || old.recurring != new.recurring
            || old.tracked != new.tracked
        {
            return false;
        }
    }
    true
}

#[expect(clippy::manual_map, reason = "step code uses a closed match rather than a closure")]
fn result_proposal_number(parent: &crate::TaskRecord) -> Option<u64> {
    if !parent.result_proposal {
        return None;
    }
    match &parent.proposal {
        Some(proposal) => Some(proposal.number),
        None => None,
    }
}

#[expect(clippy::too_many_lines, reason = "one batch member validates its complete creation shape")]
fn check_members(
    domain: &Domain,
    limits: &Limits,
    creator: Party,
    batch: &[New],
    parent: Option<&crate::TaskRecord>,
) -> Result<(), Problem> {
    for (at, new) in batch.iter().enumerate() {
        let number = Some(new.number);
        if domain.names.contains_key(&new.number) {
            return Err(problem(number, Refusal::Duplicate));
        }
        for earlier in batch.iter().take(at) {
            if earlier.number == new.number {
                return Err(problem(number, Refusal::Duplicate));
            }
        }
        if let Some(parent) = parent
            && new.project != parent.project
        {
            return Err(problem(number, Refusal::Project));
        }
        match creator {
            Party::Deployment { project } if project != new.project => return Err(problem(number, Refusal::Project)),
            Party::Deployment { .. } | Party::Task(_) | Party::Person(_) => {}
        }
        let mut in_project = 0_u32;
        for (_, id) in &domain.names {
            if domain.tasks.get(*id).expect("name indexes live task").record.project == new.project {
                in_project = in_project.saturating_add(1);
            }
        }
        for sibling in batch {
            if sibling.project == new.project {
                in_project = in_project.saturating_add(1);
            }
        }
        if in_project > limits.project_tasks {
            return Err(problem(number, Refusal::Project));
        }
        match new.executor {
            Executor::Agent { charter } => {
                let mut known = false;
                for configured in &domain.charters {
                    if *configured == charter {
                        known = true;
                    }
                }
                if !known {
                    return Err(problem(number, Refusal::Executor));
                }
            }
            Executor::Procedure { code, .. } => {
                if code == 0 {
                    return Err(problem(number, Refusal::Executor));
                }
            }
            Executor::Person(crate::PersonAddress::Person(0)) => {
                return Err(problem(number, Refusal::Executor));
            }
            Executor::Person(crate::PersonAddress::Person(_) | crate::PersonAddress::Role(_)) => {}
        }
        if !valid_recurring(limits, creator, new) {
            return Err(problem(number, Refusal::Spec));
        }
        if !valid_spec(limits, &new.spec) {
            return Err(problem(number, Refusal::Spec));
        }
        if !crate::wake::valid(&new.wake) {
            return Err(problem(number, Refusal::Spec));
        }
        if !valid_contract(limits, &new.contract) {
            return Err(problem(number, Refusal::Contract));
        }
        if !valid_authority(limits, &new.authority) {
            return Err(problem(number, Refusal::AuthorityShape));
        }
        if new.dependencies.len() > usize::try_from(limits.dependencies).expect("u32 fits usize") {
            return Err(problem(number, Refusal::Dependencies));
        }
        for (index, dependency) in new.dependencies.iter().enumerate() {
            for earlier in new.dependencies.iter().take(index) {
                if earlier == dependency {
                    return Err(problem(number, Refusal::Dependencies));
                }
            }
            let mut known = false;
            for sibling in batch {
                if sibling.number == *dependency {
                    known = true;
                }
            }
            if !known && let Some(parent) = parent {
                for delegate in &parent.delegates {
                    if delegate == dependency && domain.names.contains_key(dependency) {
                        known = true;
                    }
                }
            }
            if !known && let Some(parent) = parent {
                known = contains(&parent.references, *dependency) && domain.names.contains_key(dependency);
            }
            if !known {
                return Err(problem(number, Refusal::Dependencies));
            }
        }
    }
    Ok(())
}

fn valid_recurring(limits: &Limits, creator: Party, new: &New) -> bool {
    match &new.recurring {
        Some(template) => {
            let source = match new.funder {
                crate::Funder::Period { project, .. } => project == new.project,
                crate::Funder::Task(_) | crate::Funder::Pool { .. } | crate::Funder::Recurring { .. } => false,
            };
            let procedure = match new.executor {
                Executor::Procedure { code, .. } => code != 0,
                Executor::Agent { .. } | Executor::Person(_) => false,
            };
            procedure
                && creator == (Party::Deployment { project: new.project })
                && source
                && template.key != 0
                && new.numbers == (crate::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 })
                && crate::recurring::valid_template(limits, new.project, new.authority.budget.spend, &template.batch)
        }
        None => true,
    }
}

pub(crate) fn contains(numbers: &[u64], number: u64) -> bool {
    for candidate in numbers {
        if *candidate == number {
            return true;
        }
    }
    false
}

/// Pure bounded shape check used by the root before retaining a call body.
#[must_use]
pub fn valid_spec(limits: &Limits, spec: &Spec) -> bool {
    if spec.parameters.len() > usize::try_from(limits.parameters).expect("u32 fits usize")
        || spec.inputs.len() > usize::try_from(limits.inputs).expect("u32 fits usize")
    {
        return false;
    }
    for (at, input) in spec.inputs.iter().enumerate() {
        for earlier in spec.inputs.iter().take(at) {
            if earlier == input {
                return false;
            }
        }
    }
    let mut bytes = spec.words.len();
    for parameter in &spec.parameters {
        match parameter {
            Parameter::Bytes { value, .. } => {
                let Some(total) = bytes.checked_add(value.len()) else {
                    return false;
                };
                bytes = total;
            }
            Parameter::Number { .. } | Parameter::Resource { .. } => {}
        }
    }
    bytes <= usize::try_from(limits.spec_bytes).expect("u32 fits usize")
}

/// Pure bounded result-contract shape check before retaining a call body.
#[must_use]
pub fn valid_contract(limits: &Limits, contract: &Contract) -> bool {
    match contract {
        Contract::Report { words } | Contract::Change { words, .. } => *words <= limits.result_bytes,
        Contract::Verdict { choices } => {
            if choices.is_empty() || choices.len() > usize::try_from(limits.contract_choices).expect("u32 fits usize") {
                return false;
            }
            for (at, choice) in choices.iter().enumerate() {
                if choice.words > limits.result_bytes || choice.followups > limits.batch {
                    return false;
                }
                for earlier in choices.iter().take(at) {
                    if earlier.code == choice.code {
                        return false;
                    }
                }
            }
            true
        }
    }
}

/// Pure bounded permission-value shape check before retaining a call body.
#[must_use]
pub fn valid_authority(limits: &Limits, value: &Authority) -> bool {
    if value.grants.len() > usize::try_from(limits.authority_grants).expect("u32 fits usize")
        || value.note_resources.len() > usize::try_from(limits.authority_grants).expect("u32 fits usize")
        || value.delegation.kinds.len() > usize::try_from(limits.executor_kinds).expect("u32 fits usize")
        || value.notes.0 & !7 != 0
    {
        return false;
    }
    let mut bytes = 0_usize;
    for grant in &value.grants {
        if grant.pattern.segments.len() > usize::try_from(limits.authority_segments).expect("u32 fits usize") {
            return false;
        }
        for segment in &grant.pattern.segments {
            let Some(total) = bytes.checked_add(segment.len()) else {
                return false;
            };
            bytes = total;
        }
        match &grant.pattern.last {
            Last::Exact(last) | Last::Open(last) => {
                let Some(total) = bytes.checked_add(last.len()) else {
                    return false;
                };
                bytes = total;
            }
        }
    }
    for scope in &value.note_resources {
        if scope.pattern.segments.len() > usize::try_from(limits.authority_segments).expect("u32 fits usize") {
            return false;
        }
        for segment in &scope.pattern.segments {
            let Some(total) = bytes.checked_add(segment.len()) else { return false };
            bytes = total;
        }
        match &scope.pattern.last {
            Last::Exact(last) | Last::Open(last) => {
                let Some(total) = bytes.checked_add(last.len()) else { return false };
                bytes = total;
            }
        }
    }
    bytes <= usize::try_from(limits.authority_bytes).expect("u32 fits usize")
}

/// Include the delegation edges added to an existing creator, as well as all
/// immutable existing dependencies: a reference to an ancestor can otherwise
/// create a wait cycle without a cycle among the new tasks themselves.
pub(crate) fn acyclic(domain: &Domain, limits: &Limits, creator: Party, batch: &[New]) -> bool {
    let mut ordered = List::with_capacity(limits.tasks);
    let total = domain.names.len().saturating_add(u32::try_from(batch.len()).unwrap_or(u32::MAX));
    for _ in 0..total {
        let mut next = None;
        for (number, _) in &domain.names {
            if contains(ordered.as_slice(), *number) {
                continue;
            }
            let task = record(domain, *number).expect("indexed task live");
            let mut ready = true;
            for dependency in &task.dependencies {
                if domain.names.contains_key(dependency) && !contains(ordered.as_slice(), *dependency) {
                    ready = false;
                }
            }
            for delegate in &task.delegates {
                if !contains(ordered.as_slice(), *delegate) {
                    ready = false;
                }
            }
            if creator == Party::Task(*number) {
                for new in batch {
                    if !contains(ordered.as_slice(), new.number) {
                        ready = false;
                    }
                }
            }
            if ready {
                next = Some(*number);
                break;
            }
        }
        if next.is_none() {
            for new in batch {
                if contains(ordered.as_slice(), new.number) {
                    continue;
                }
                let mut ready = true;
                for dependency in &new.dependencies {
                    if (domain.names.contains_key(dependency) || batch_has(batch, *dependency))
                        && !contains(ordered.as_slice(), *dependency)
                    {
                        ready = false;
                    }
                }
                if ready {
                    next = Some(new.number);
                    break;
                }
            }
        }
        let Some(number) = next else {
            return false;
        };
        ordered.push(number).expect("admitted graph bounded by live task capacity");
    }
    true
}

fn batch_has(batch: &[New], number: u64) -> bool {
    for new in batch {
        if new.number == number {
            return true;
        }
    }
    false
}
