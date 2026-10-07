//! Person adoption of a live task, with one reservation transfer and requester-tree rewrite
//! (domain/tasks.md, section 6; domain/authority.md, section 7). Root authenticates the
//! adopter and policy rights; this child checks every local row before changing any of them.
use crate::domain::{Domain, entrance, publish, record, refused, task_mut};
use crate::{Change, Funder, Limits, Numbers, Party, Phase, Refusal, Request, Was};
use skein_lib::{Env, List, Queue, ReplyTo};

fn descendant(domain: &Domain, number: u64, ancestor: u64, bound: u32) -> bool {
    let mut at = Some(number);
    for _ in 0..bound {
        let Some(current) = at else { return false };
        if current == ancestor {
            return true;
        }
        at = match record(domain, current) {
            Some(task) => match task.requester {
                Party::Task(parent) => Some(parent),
                Party::Person(_) | Party::Deployment { .. } => None,
            },
            None => None,
        };
    }
    false
}

fn mutable(phase: &Phase) -> bool {
    match phase {
        Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => true,
        Phase::Closing(_) | Phase::Held { was: Was::Closing(_), .. } | Phase::Ended(_) => false,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one authenticated person move carries its target, funding period and reason"
)]
#[expect(clippy::too_many_lines, reason = "one move preflights and updates its requester and funding cohort")]
pub(crate) fn apply(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    person: u64,
    period: u64,
    pool_budget: u64,
    period_budget: u64,
    reason: &[u8],
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let old = record(domain, number).expect("entrance named live task");
    if person == 0 || old.requester == Party::Person(person) {
        return refused(to, Some(number), Refusal::State, out);
    }
    if !mutable(&old.phase) {
        return refused(to, Some(number), Refusal::State, out);
    }
    if reason.len() > usize::try_from(env.limits.message_bytes).expect("bounded reason size") {
        return refused(to, Some(number), Refusal::Read, out);
    }
    let project = old.project;
    let from = old.requester;
    let previous_root = old.root;
    let previous_depth = old.depth;
    let source = old.funder;
    let Some(spent) = crate::funders::total(old.numbers) else {
        return refused(to, Some(number), Refusal::Funding, out);
    };
    let Some(left) = crate::funders::remaining(old.numbers) else {
        return refused(to, Some(number), Refusal::Funding, out);
    };
    let destination = Funder::Pool { project, person, period };
    let period_source = Funder::Period { project, period };
    for (task, _) in &domain.names {
        let row = record(domain, *task).expect("live name");
        let exhausted_escalation = match row.escalation {
            crate::Escalation::Waiting { revision: u64::MAX, .. } => true,
            crate::Escalation::Waiting { .. }
            | crate::Escalation::Unheld { .. }
            | crate::Escalation::Routing { .. }
            | crate::Escalation::Rejected { .. } => false,
        };
        let exhausted_proposal = match &row.proposal {
            Some(proposal) => match proposal.state {
                crate::ProposalState::Pending { .. } => row.revision == u64::MAX,
                crate::ProposalState::Accepted { .. }
                | crate::ProposalState::Rejected { .. }
                | crate::ProposalState::Withdrawn => false,
            },
            None => false,
        };
        if exhausted_escalation || exhausted_proposal {
            return refused(to, Some(*task), Refusal::State, out);
        }
    }
    let mut subtree = List::with_capacity(env.limits.tasks);
    for (child, _) in &domain.names {
        if descendant(domain, *child, number, env.limits.tasks) {
            let row = record(domain, *child).expect("subtree name live");
            if !mutable(&row.phase)
                || row.revision == u64::MAX
                || row.root != previous_root
                || row.depth < previous_depth
            {
                return refused(to, Some(*child), Refusal::State, out);
            }
            subtree.push(*child).expect("live task bound");
        }
    }
    let old_parent = match from {
        Party::Task(parent) => {
            let Some(parent_row) = record(domain, parent) else {
                return refused(to, Some(number), Refusal::Reference, out);
            };
            if !parent_row.references.contains(&number)
                && parent_row.references.len() >= usize::try_from(env.limits.references).expect("reference bound")
            {
                return refused(to, Some(number), Refusal::Reference, out);
            }
            Some(parent)
        }
        Party::Person(_) | Party::Deployment { .. } => None,
    };
    if source != destination {
        let source_numbers = match source {
            Funder::Task(parent) => match record(domain, parent) {
                Some(parent) => parent.numbers,
                None => return refused(to, Some(number), Refusal::Funding, out),
            },
            Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => match domain.funding.get(&source)
            {
                Some(ledger) => ledger.numbers,
                None => return refused(to, Some(number), Refusal::Funding, out),
            },
        };
        if source_numbers.reserved.checked_sub(old.numbers.budget).is_none()
            || source_numbers.spent_below.checked_add(spent).is_none()
        {
            return refused(to, Some(number), Refusal::Funding, out);
        }
        let mut after = match domain.funding.get(&destination) {
            Some(pool) if !pool.closed => pool.numbers,
            Some(_) => return refused(to, Some(number), Refusal::Funding, out),
            None => Numbers { budget: pool_budget, spent: 0, spent_below: 0, reserved: 0 },
        };
        after.reserved = match after.reserved.checked_add(left) {
            Some(reserved) => reserved,
            None => return refused(to, Some(number), Refusal::Funding, out),
        };
        if crate::funders::available(after).is_none() {
            return refused(to, Some(number), Refusal::Funding, out);
        }
        let normalized = Numbers { budget: left, spent: 0, spent_below: 0, reserved: old.numbers.reserved };
        if crate::funders::available(normalized).is_none() {
            return refused(to, Some(number), Refusal::Funding, out);
        }
    }
    let create_pool = domain.funding.get(&destination).is_none();
    let create_period = domain.funding.get(&period_source).is_none();
    if create_period && !create_pool {
        return refused(to, Some(number), Refusal::Funding, out);
    }
    if create_pool {
        let period_numbers = match domain.funding.get(&period_source) {
            Some(ledger) if !ledger.closed => ledger.numbers,
            Some(_) => return refused(to, Some(number), Refusal::Funding, out),
            None => {
                for (funder, _) in &domain.funding {
                    match *funder {
                        Funder::Period { project: old_project, period: old_period }
                            if old_project == project && old_period >= period =>
                        {
                            return refused(to, Some(number), Refusal::Funding, out);
                        }
                        Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } | Funder::Period { .. } => {}
                    }
                }
                Numbers { budget: period_budget, spent: 0, spent_below: 0, reserved: 0 }
            }
        };
        let mut after = period_numbers;
        after.reserved = match after.reserved.checked_add(pool_budget) {
            Some(reserved) => reserved,
            None => return refused(to, Some(number), Refusal::Funding, out),
        };
        if crate::funders::available(after).is_none() {
            return refused(to, Some(number), Refusal::Funding, out);
        }
        let needed = if create_period { 2 } else { 1 };
        if match domain.funding.len().checked_add(needed) {
            Some(count) => count > domain.funding.capacity(),
            None => true,
        } {
            return refused(to, Some(number), Refusal::Busy, out);
        }
    }
    if create_period {
        let record = crate::FundingRecord {
            funder: period_source,
            parent: None,
            numbers: Numbers { budget: period_budget, spent: 0, spent_below: 0, reserved: 0 },
            closed: false,
        };
        assert!(domain.funding.insert(period_source, record) == Ok(None), "period room preflighted");
        crate::funders::save_funding(domain, period_source, out);
    }
    if create_pool {
        let period_row = domain.funding.get_mut(&period_source).expect("period preflighted");
        period_row.numbers.reserved =
            period_row.numbers.reserved.checked_add(pool_budget).expect("pool carve preflighted");
        let record = crate::FundingRecord {
            funder: destination,
            parent: Some(period_source),
            numbers: Numbers { budget: pool_budget, spent: 0, spent_below: 0, reserved: 0 },
            closed: false,
        };
        assert!(domain.funding.insert(destination, record) == Ok(None), "pool room preflighted");
        crate::funders::save_funding(domain, period_source, out);
        crate::funders::save_funding(domain, destination, out);
    }
    if source != destination {
        match source {
            Funder::Task(parent) => {
                let budget = record(domain, number).expect("target live").numbers.budget;
                let row = task_mut(domain, parent).expect("checked task source");
                row.record.numbers.reserved =
                    row.record.numbers.reserved.checked_sub(budget).expect("source preflight");
                row.record.numbers.spent_below =
                    row.record.numbers.spent_below.checked_add(spent).expect("source preflight");
                publish(domain, env, parent, out);
            }
            Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => {
                let budget = record(domain, number).expect("target live").numbers.budget;
                let ledger = domain.funding.get_mut(&source).expect("checked finite source");
                ledger.numbers.reserved = ledger.numbers.reserved.checked_sub(budget).expect("source preflight");
                ledger.numbers.spent_below = ledger.numbers.spent_below.checked_add(spent).expect("source preflight");
                crate::funders::save_funding(domain, source, out);
            }
        }
        let ledger = domain.funding.get_mut(&destination).expect("checked new pool");
        ledger.numbers.reserved = ledger.numbers.reserved.checked_add(left).expect("destination preflight");
        crate::funders::save_funding(domain, destination, out);
    }
    if let Some(parent) = old_parent {
        let parent_row = task_mut(domain, parent).expect("old requester live");
        let mut delegates = List::with_capacity(env.limits.delegates);
        for delegate in &parent_row.record.delegates {
            if *delegate != number {
                delegates.push(*delegate).expect("delegate subset");
            }
        }
        parent_row.record.delegates = delegates.into_boxed();
        if !parent_row.record.references.contains(&number) {
            let mut references = List::with_capacity(env.limits.references);
            for reference in &parent_row.record.references {
                references.push(*reference).expect("old references bounded");
            }
            references.push(number).expect("reference room preflighted");
            parent_row.record.references = references.into_boxed();
        }
        publish(domain, env, parent, out);
    }
    {
        let task = task_mut(domain, number).expect("moving target live");
        task.record.requester = Party::Person(person);
        if source != destination {
            task.record.funder = destination;
            task.record.numbers.budget = left;
            task.record.numbers.spent = 0;
            task.record.numbers.spent_below = 0;
            task.record.authority.budget.spend = left;
        }
    }
    for child in subtree.into_boxed() {
        let row = task_mut(domain, child).expect("subtree live");
        assert!(row.record.root == previous_root, "subtree has one root");
        row.record.root = number;
        row.record.depth = row.record.depth.checked_sub(previous_depth).expect("subtree depth preflight");
        crate::control::history(domain, child, Party::Person(person), Change::Moved, reason, out);
        publish(domain, env, child, out);
    }
    let mut waiting = List::with_capacity(env.limits.tasks);
    for (task, _) in &domain.names {
        waiting.push(*task).expect("live task bound");
    }
    for task in waiting.into_boxed() {
        let row = record(domain, task).expect("live name");
        match row.escalation {
            crate::Escalation::Waiting { .. } => out.push(Request::EscalationNeeded {
                context: crate::escalation::context(domain, task).expect("waiting task context"),
            }),
            crate::Escalation::Unheld { .. }
            | crate::Escalation::Routing { .. }
            | crate::Escalation::Rejected { .. } => {}
        }
        if let Some(proposal) = &row.proposal {
            match proposal.state {
                crate::ProposalState::Pending { .. } => {
                    out.push(Request::ProposalRerouteNeeded { proposer: task, proposal: proposal.number });
                }
                crate::ProposalState::Accepted { .. }
                | crate::ProposalState::Rejected { .. }
                | crate::ProposalState::Withdrawn => {}
            }
        }
    }
    out.push(Request::Done { reply_to: to });
}
