//! Authentic finite periods/pools and actual task-allotment accounting
//! (domain/tasks.md, section 2; domain/authority.md, section 7).
//! Root authorizes new allocations; tasks reserves, charges and closes their
//! real financial links once, emitting rows for one root decision. No move
//! and retires superseded sources after their last reservations settle.
use crate::domain::{Domain, publish, record, refused, task_mut};
use crate::{Funder, Limits, Numbers, Refusal, Request, Stored};
use skein_lib::{Env, Queue, ReplyTo};

pub(crate) fn total(numbers: Numbers) -> Option<u64> {
    numbers.spent.checked_add(numbers.spent_below)
}

pub(crate) fn remaining(numbers: Numbers) -> Option<u64> {
    numbers.budget.checked_sub(total(numbers)?)
}

pub(crate) fn available(numbers: Numbers) -> Option<u64> {
    remaining(numbers)?.checked_sub(numbers.reserved)
}

/// Debit a root-authorized effect at its described maximum, exactly once in
/// the decision that creates its outbox entry (domain/authority.md, 7).
pub(crate) fn charge_effect(
    domain: &mut Domain,
    env: &Env<Limits>,
    funder: Funder,
    maximum: u64,
    out: &mut Queue<Request>,
) {
    if maximum == 0 {
        return;
    }
    match funder {
        Funder::Task(number) => {
            let row = record(domain, number).expect("checked effect task");
            assert!(
                available(row.numbers).expect("valid effect balance") >= maximum
                    && representable(domain, number, maximum),
                "root admitted the effect price"
            );
            let row = &mut task_mut(domain, number).expect("priced effect task").record;
            row.numbers.spent = row.numbers.spent.checked_add(maximum).expect("checked effect price");
            publish(domain, env, number, out);
        }
        Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => {
            let ledger = domain.funding.get_mut(&funder).expect("effect source opened in this decision");
            assert!(
                !ledger.closed && available(ledger.numbers).expect("valid effect source") >= maximum,
                "root admitted the holder's effect price"
            );
            ledger.numbers.spent = ledger.numbers.spent.checked_add(maximum).expect("effect maximum fits source");
            save_funding(domain, funder, out);
        }
    }
}

/// Actual task sources cannot end while any incoming allocation remains live.
pub(crate) fn funded_live(domain: &Domain, funder: u64) -> bool {
    for (number, _) in &domain.names {
        if *number != funder && record(domain, *number).expect("live name").funder == Funder::Task(funder) {
            return true;
        }
    }
    false
}

fn source_live(domain: &Domain, funder: Funder) -> bool {
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("live name");
        if task.funder == funder && task.recurring.is_some() {
            return true;
        }
    }
    false
}

pub(crate) fn can_reserve(
    domain: &Domain,
    creator: crate::Party,
    batch: &[crate::New],
    result_proposal: Option<u64>,
    direct_finish: Option<(u64, u64)>,
) -> bool {
    for new in batch {
        let budget = if new.recurring.is_some() { 0 } else { new.authority.budget.spend };
        if new.numbers != (Numbers { budget, spent: 0, spent_below: 0, reserved: 0 }) {
            return false;
        }
        match new.funder {
            Funder::Task(number) => {
                let Some(funder) = record(domain, number) else {
                    return false;
                };
                let ancestor = match creator {
                    crate::Party::Task(requester) => below(domain, requester, number, domain.names.len()),
                    crate::Party::Person(_) | crate::Party::Deployment { .. } => false,
                };
                let proposal_matches = match &funder.proposal {
                    Some(proposal) => Some(proposal.number) == result_proposal,
                    None => false,
                };
                let finishing_result = result_proposal.is_some()
                    && number
                        == match creator {
                            crate::Party::Task(task) => task,
                            crate::Party::Person(_) | crate::Party::Deployment { .. } => 0,
                        }
                    && funder.result_proposal
                    && proposal_matches;
                if !ancestor || funder.project != new.project || !(mutable(&funder.phase) || finishing_result) {
                    return false;
                }
                let mut after = funder.numbers;
                if let Some((finishing, charge)) = direct_finish
                    && finishing == number
                {
                    let Some(reserved) = after.reserved.checked_sub(funder.run_reserved) else { return false };
                    let Some(spent) = after.spent.checked_add(charge) else { return false };
                    after.reserved = reserved;
                    after.spent = spent;
                }
                for sibling in batch {
                    if sibling.funder == new.funder {
                        let Some(reserved) = after.reserved.checked_add(sibling.numbers.budget) else {
                            return false;
                        };
                        after.reserved = reserved;
                    }
                }
                if available(after).is_none() {
                    return false;
                }
            }
            Funder::Pool { project, .. } | Funder::Period { project, .. } | Funder::Recurring { project, .. } => {
                match new.funder {
                    Funder::Recurring { task, .. } if creator != crate::Party::Task(task) => return false,
                    Funder::Recurring { .. } | Funder::Task(_) | Funder::Pool { .. } | Funder::Period { .. } => {}
                }
                let Some(ledger) = domain.funding.get(&new.funder) else {
                    return false;
                };
                if project != new.project || ledger.closed {
                    return false;
                }
                let mut after = ledger.numbers;
                for sibling in batch {
                    if sibling.funder == new.funder {
                        let Some(reserved) = after.reserved.checked_add(sibling.numbers.budget) else {
                            return false;
                        };
                        after.reserved = reserved;
                    }
                }
                if available(after).is_none() {
                    return false;
                }
            }
        }
    }
    true
}

pub(crate) fn reserve(domain: &mut Domain, env: &Env<Limits>, batch: &[crate::New], out: &mut Queue<Request>) {
    for new in batch {
        match new.funder {
            Funder::Task(number) => {
                let task = task_mut(domain, number).expect("actual funder admitted");
                task.record.numbers.reserved =
                    task.record.numbers.reserved.checked_add(new.numbers.budget).expect("reservation admitted");
                publish(domain, env, number, out);
            }
            // Ordinary external reservations are owned here, in the Make commit.
            Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => {
                let ledger = domain.funding.get_mut(&new.funder).expect("finite source admitted");
                ledger.numbers.reserved =
                    ledger.numbers.reserved.checked_add(new.numbers.budget).expect("reservation admitted");
                save_funding(domain, new.funder, out);
            }
        }
    }
}

/// Replace one live allotment and its authentic reservation in one child decision.
/// The caller checks policy authority; this preflights both finite balances.
pub(crate) fn can_resize(domain: &Domain, number: u64, budget: u64) -> bool {
    let Some(task) = record(domain, number) else { return false };
    let mut next = task.numbers;
    next.budget = budget;
    if available(next).is_none() {
        return false;
    }
    let source = match task.funder {
        Funder::Task(parent) => match record(domain, parent) {
            Some(parent) => parent.numbers,
            None => return false,
        },
        Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => {
            match domain.funding.get(&task.funder) {
                Some(ledger) if !ledger.closed => ledger.numbers,
                Some(_) | None => return false,
            }
        }
    };
    let mut next_source = source;
    let Some(released) = source.reserved.checked_sub(task.numbers.budget) else { return false };
    let Some(reserved) = released.checked_add(budget) else {
        return false;
    };
    next_source.reserved = reserved;
    available(next_source).is_some()
}

pub(crate) fn resize(domain: &mut Domain, env: &Env<Limits>, number: u64, budget: u64, out: &mut Queue<Request>) {
    assert!(can_resize(domain, number, budget), "allotment preflighted");
    let task = record(domain, number).expect("live allotment");
    let source = task.funder;
    let old = task.numbers.budget;
    match source {
        Funder::Task(parent) => {
            let reserved = record(domain, parent).expect("live task source").numbers.reserved;
            let replaced = reserved
                .checked_sub(old)
                .expect("reservation preflighted")
                .checked_add(budget)
                .expect("reservation preflighted");
            task_mut(domain, parent).expect("live task source").record.numbers.reserved = replaced;
            publish(domain, env, parent, out);
        }
        Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => {
            let ledger = domain.funding.get_mut(&source).expect("finite source");
            ledger.numbers.reserved = ledger
                .numbers
                .reserved
                .checked_sub(old)
                .expect("reservation preflighted")
                .checked_add(budget)
                .expect("reservation preflighted");
            save_funding(domain, source, out);
        }
    }
    task_mut(domain, number).expect("live allotment").record.numbers.budget = budget;
}

pub(crate) fn end(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let task = record(domain, number).expect("ending task live");
    let funder = task.funder;
    let budget = task.numbers.budget;
    let spent = total(task.numbers).expect("total checked on charge and restore");
    match funder {
        Funder::Task(parent) => {
            let parent_number = parent;
            let parent =
                task_mut(domain, parent_number).expect("actual funder remains live until its allocations close");
            parent.record.numbers.reserved =
                parent.record.numbers.reserved.checked_sub(budget).expect("allotment reserved on admission");
            parent.record.numbers.spent_below = parent
                .record
                .numbers
                .spent_below
                .checked_add(spent)
                .expect("root ensures priced aggregates fit accounting unit");
            publish(domain, env, parent_number, out);
        }
        // Post once against the actual original source in the task-end commit.
        Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => {
            let ledger = domain.funding.get_mut(&funder).expect("actual source preserved");
            ledger.numbers.reserved = ledger.numbers.reserved.checked_sub(budget).expect("allotment reserved");
            ledger.numbers.spent_below =
                ledger.numbers.spent_below.checked_add(spent).expect("priced aggregate fits unit");
            save_funding(domain, funder, out);
            retire(domain, out);
        }
    }
}

pub(crate) fn links(domain: &Domain, bound: u32) -> bool {
    for (funder, ledger) in &domain.funding {
        let mut reserved = 0_u64;
        for (_, other) in &domain.funding {
            if other.parent == Some(*funder) && !other.closed {
                let Some(sum) = reserved.checked_add(other.numbers.budget) else {
                    return false;
                };
                reserved = sum;
            }
        }
        for (number, _) in &domain.names {
            let task = record(domain, *number).expect("live name");
            if task.funder == *funder {
                if ledger.closed {
                    return false;
                }
                let Some(sum) = reserved.checked_add(task.numbers.budget) else {
                    return false;
                };
                reserved = sum;
            }
        }
        if reserved != ledger.numbers.reserved {
            return false;
        }
        if ledger.closed && ledger.numbers.reserved != 0 {
            return false;
        }
        if let Some(parent) = ledger.parent
            && !domain.funding.contains_key(&parent)
        {
            return false;
        }
    }
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("live name");
        let mut reserved = task.run_reserved;
        for (child, _) in &domain.names {
            let child = record(domain, *child).expect("live name");
            if child.funder == Funder::Task(*number) {
                let Some(total) = reserved.checked_add(child.numbers.budget) else {
                    return false;
                };
                reserved = total;
            }
        }
        if reserved != task.numbers.reserved || available(task.numbers).is_none() {
            return false;
        }
        let mut at = Some(*number);
        for hop in 0..bound {
            let Some(current) = at else {
                break;
            };
            let Some(node) = record(domain, current) else {
                return false;
            };
            if node.project != task.project {
                return false;
            }
            at = match node.funder {
                Funder::Task(parent) => {
                    let ancestor = match node.requester {
                        crate::Party::Task(requester) => below(domain, requester, parent, bound),
                        crate::Party::Person(_) | crate::Party::Deployment { .. } => false,
                    };
                    if !ancestor {
                        return false;
                    }
                    if parent == *number || hop.saturating_add(1) == bound {
                        return false;
                    }
                    Some(parent)
                }
                Funder::Pool { project, .. } | Funder::Period { project, .. } | Funder::Recurring { project, .. } => {
                    if project != task.project || !domain.funding.contains_key(&node.funder) {
                        return false;
                    }
                    None
                }
            };
        }
    }
    true
}

/// Owned finite external period or person-pool accounting; tasks mutates the authentic counters and
/// emits typed ledger saves for the root's atomic commit. (domain/tasks.md, sections 2–3).
/// (domain/authority.md, section 7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FundingRecord {
    /// Durable period/pool identity; task sources use live `TaskRecord` counters instead of a
    /// separate ledger.
    pub funder: Funder,
    /// Pool's original project period; a period has `None`, and no current event changes this
    /// link.
    pub parent: Option<Funder>,
    /// Authentic finite accounting owned by tasks; external ledgers have zero direct spent and
    /// receive settled expense in `spent_below`. Root borrows these values for policy checks.
    pub numbers: Numbers,
    /// Lifetime task allotment used by a standing root's archived period; zero for other ledgers.
    pub made: u32,
    /// A superseded source is closed after its last reservation settles; its saved row retains
    /// the period's history and cannot fund new work.
    pub closed: bool,
}

pub(crate) fn save_funding(domain: &Domain, funder: Funder, out: &mut Queue<Request>) {
    let ledger = *domain.funding.get(&funder).expect("funding admitted");
    out.push(Request::Save { record: Stored::Ledger(ledger) });
}

pub(crate) fn open(domain: &mut Domain, to: ReplyTo, project: u32, period: u64, budget: u64, out: &mut Queue<Request>) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    let funder = Funder::Period { project, period };
    if domain.funding.contains_key(&funder) {
        return refused(to, None, Refusal::Duplicate, out);
    }
    // Monotonic identities prevent reintroducing a reset's old period.
    for (other, _) in &domain.funding {
        match *other {
            Funder::Period { project: p, period: old } => {
                if p == project && old >= period {
                    return refused(to, None, Refusal::Funding, out);
                }
            }
            Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } => {}
        }
    }
    if domain.funding.len() == domain.funding.capacity() {
        return refused(to, None, Refusal::Busy, out);
    }
    let record = FundingRecord {
        funder,
        parent: None,
        numbers: Numbers { budget, spent: 0, spent_below: 0, reserved: 0 },
        made: 0,
        closed: false,
    };
    assert!(domain.funding.insert(funder, record) == Ok(None), "period admitted");
    save_funding(domain, funder, out);
    retire(domain, out);
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn carve(
    domain: &mut Domain,
    to: ReplyTo,
    project: u32,
    person: u64,
    period: u64,
    budget: u64,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    let funder = Funder::Pool { project, person, period };
    let parent = Funder::Period { project, period };
    if domain.funding.contains_key(&funder) {
        return refused(to, None, Refusal::Duplicate, out);
    }
    let Some(old) = domain.funding.get(&parent) else {
        return refused(to, None, Refusal::Funding, out);
    };
    let mut after = old.numbers;
    let Some(reserved) = after.reserved.checked_add(budget) else {
        return refused(to, None, Refusal::Funding, out);
    };
    after.reserved = reserved;
    if old.closed || newer_period(domain, project, period) || available(after).is_none() {
        return refused(to, None, Refusal::Funding, out);
    }
    if domain.funding.len() == domain.funding.capacity() {
        return refused(to, None, Refusal::Busy, out);
    }
    let record = FundingRecord {
        funder,
        parent: Some(parent),
        numbers: Numbers { budget, spent: 0, spent_below: 0, reserved: 0 },
        made: 0,
        closed: false,
    };
    assert!(domain.funding.insert(funder, record) == Ok(None), "pool admitted");
    domain.funding.get_mut(&parent).expect("period admitted").numbers = after;
    save_funding(domain, parent, out);
    save_funding(domain, funder, out);
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn resize_pool(
    domain: &mut Domain,
    to: ReplyTo,
    project: u32,
    person: u64,
    period: u64,
    budget: u64,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    let funder = Funder::Pool { project, person, period };
    let parent = Funder::Period { project, period };
    let Some(pool) = domain.funding.get(&funder).copied() else {
        return refused(to, None, Refusal::Unknown, out);
    };
    let Some(period_record) = domain.funding.get(&parent).copied() else {
        return refused(to, None, Refusal::Funding, out);
    };
    if pool.closed || period_record.closed || pool.parent != Some(parent) || newer_period(domain, project, period) {
        return refused(to, None, Refusal::Funding, out);
    }
    let mut next_pool = pool.numbers;
    next_pool.budget = budget;
    if available(next_pool).is_none() {
        return refused(to, None, Refusal::Funding, out);
    }
    let mut next_period = period_record.numbers;
    let Some(released) = next_period.reserved.checked_sub(pool.numbers.budget) else {
        return refused(to, None, Refusal::Funding, out);
    };
    let Some(reserved) = released.checked_add(budget) else {
        return refused(to, None, Refusal::Funding, out);
    };
    next_period.reserved = reserved;
    if available(next_period).is_none() {
        return refused(to, None, Refusal::Funding, out);
    }
    domain.funding.get_mut(&funder).expect("pool preflighted").numbers = next_pool;
    domain.funding.get_mut(&parent).expect("period preflighted").numbers = next_period;
    save_funding(domain, parent, out);
    save_funding(domain, funder, out);
    out.push(Request::Done { reply_to: to });
}

/// Reserve a recurring task's full per-period allotment from that project's period.
pub(crate) fn carve_recurring(
    domain: &mut Domain,
    project: u32,
    task: u64,
    period: u64,
    budget: u64,
    out: &mut Queue<Request>,
) -> bool {
    let funder = Funder::Recurring { project, task, period };
    let parent = Funder::Period { project, period };
    if domain.funding.contains_key(&funder) || domain.funding.len() == domain.funding.capacity() {
        return false;
    }
    let Some(old) = domain.funding.get(&parent) else { return false };
    let mut after = old.numbers;
    let Some(reserved) = after.reserved.checked_add(budget) else { return false };
    after.reserved = reserved;
    if old.closed || newer_period(domain, project, period) || available(after).is_none() {
        return false;
    }
    let record = FundingRecord {
        funder,
        parent: Some(parent),
        numbers: Numbers { budget, spent: 0, spent_below: 0, reserved: 0 },
        made: 1,
        closed: false,
    };
    assert!(domain.funding.insert(funder, record) == Ok(None), "recurring allotment admitted");
    domain.funding.get_mut(&parent).expect("period admitted").numbers = after;
    save_funding(domain, parent, out);
    save_funding(domain, funder, out);
    true
}

pub(crate) fn restore_funding(domain: &mut Domain, ledger: FundingRecord) -> bool {
    if domain.funding.contains_key(&ledger.funder)
        || domain.funding.len() == domain.funding.capacity()
        || total(ledger.numbers).is_none()
        || available(ledger.numbers).is_none()
        || (ledger.closed && ledger.numbers.reserved != 0)
        || ledger.numbers.spent != 0
        || ledger.made > domain.names.capacity()
    {
        return false;
    }
    let valid = match ledger.funder {
        Funder::Task(_) => false,
        Funder::Period { .. } => ledger.parent.is_none() && (!ledger.closed || ledger.numbers.reserved == 0),
        Funder::Pool { project, period, .. } | Funder::Recurring { project, period, .. } => {
            ledger.parent == Some(Funder::Period { project, period })
        }
    };
    if !valid {
        return false;
    }
    assert!(domain.funding.insert(ledger.funder, ledger) == Ok(None), "restored ledger admitted");
    true
}

fn newer_period(domain: &Domain, project: u32, period: u64) -> bool {
    for (funder, _) in &domain.funding {
        match *funder {
            Funder::Period { project: other, period: next } if other == project && next > period => return true,
            Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } | Funder::Period { .. } => {}
        }
    }
    false
}

// The closed rows remain durable history. Their counters do not move again.
// The pool posts its expense once and returns its original allotment to its period.
pub(crate) fn retire(domain: &mut Domain, out: &mut Queue<Request>) {
    let count = domain.funding.len();
    for _ in 0..count {
        let mut ready = None;
        for (funder, row) in &domain.funding {
            let period = match *funder {
                Funder::Pool { project, period, .. } | Funder::Recurring { project, period, .. } => {
                    Some((project, period))
                }
                Funder::Task(_) | Funder::Period { .. } => None,
            };
            if let Some((project, period)) = period
                && !row.closed
                && row.numbers.reserved == 0
                && newer_period(domain, project, period)
            {
                ready = Some((*funder, row.parent.expect("allotment's period"), row.numbers));
                break;
            }
        }
        let Some((pool, parent, numbers)) = ready else { break };
        let period = domain.funding.get_mut(&parent).expect("original period kept");
        period.numbers.reserved = period.numbers.reserved.checked_sub(numbers.budget).expect("pool reserved");
        period.numbers.spent_below = period
            .numbers
            .spent_below
            .checked_add(total(numbers).expect("valid pool"))
            .expect("pool spend representable");
        domain.funding.get_mut(&pool).expect("pool kept").closed = true;
        save_funding(domain, pool, out);
        save_funding(domain, parent, out);
    }
    let count = domain.funding.len();
    for _ in 0..count {
        let mut ready = None;
        for (funder, row) in &domain.funding {
            match *funder {
                Funder::Period { project, period }
                    if !row.closed
                        && row.numbers.reserved == 0
                        && newer_period(domain, project, period)
                        && !source_live(domain, *funder) =>
                {
                    ready = Some(*funder);
                    break;
                }
                Funder::Period { .. } | Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } => {}
            }
        }
        let Some(period) = ready else { break };
        domain.funding.get_mut(&period).expect("period kept").closed = true;
        save_funding(domain, period, out);
    }
}

// The final period will eventually receive every still-open aggregate under
// it. Counting each live node's own/closed spend once bounds every intermediate
// task and pool posting as well. No accepted charge can overflow settlement.
pub(crate) fn original_period(domain: &Domain, number: u64) -> Option<Funder> {
    let mut funder = record(domain, number)?.funder;
    for _ in 0..domain.names.len().checked_add(2)? {
        match funder {
            Funder::Task(number) => funder = record(domain, number)?.funder,
            Funder::Pool { .. } | Funder::Recurring { .. } => funder = domain.funding.get(&funder)?.parent?,
            Funder::Period { .. } => return Some(funder),
        }
    }
    None
}

/// Lifetime tree allotment for the creator's own funding period.
pub(crate) fn tree_made(domain: &Domain, creator: u64, root: u64) -> Option<u32> {
    let source = original_period(domain, creator)?;
    let current = original_period(domain, root)?;
    if source == current {
        return Some(record(domain, root)?.made);
    }
    let (project, period) = match source {
        Funder::Period { project, period } => (project, period),
        Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } => return None,
    };
    Some(domain.funding.get(&Funder::Recurring { project, task: root, period })?.made)
}

pub(crate) fn representable(domain: &Domain, number: u64, delta: u64) -> bool {
    let Some(period) = original_period(domain, number) else {
        return false;
    };
    let Some(ledger) = domain.funding.get(&period) else {
        return false;
    };
    let Some(base) = total(ledger.numbers) else {
        return false;
    };
    let Some(mut eventual) = base.checked_add(delta) else {
        return false;
    };
    for (_, pool) in &domain.funding {
        if pool.parent == Some(period) && !pool.closed {
            let Some(spent) = total(pool.numbers) else {
                return false;
            };
            let Some(sum) = eventual.checked_add(spent) else {
                return false;
            };
            eventual = sum;
        }
    }
    for (number, _) in &domain.names {
        if original_period(domain, *number) == Some(period) {
            let Some(spent) = total(record(domain, *number).expect("live name").numbers) else {
                return false;
            };
            let Some(sum) = eventual.checked_add(spent) else {
                return false;
            };
            eventual = sum;
        }
    }
    true
}

fn below(domain: &Domain, number: u64, ancestor: u64, bound: u32) -> bool {
    let mut at = Some(number);
    for _ in 0..bound {
        let Some(number) = at else {
            return false;
        };
        if number == ancestor {
            return true;
        }
        at = match record(domain, number) {
            Some(task) => match task.requester {
                crate::Party::Task(parent) => Some(parent),
                crate::Party::Person(_) | crate::Party::Deployment { .. } => None,
            },
            None => None,
        };
    }
    false
}

fn mutable(phase: &crate::Phase) -> bool {
    match phase {
        crate::Phase::Waiting
        | crate::Phase::Active(_)
        | crate::Phase::Held { was: crate::Was::Waiting | crate::Was::Active(_), .. } => true,
        crate::Phase::Closing(_) | crate::Phase::Ended(_) | crate::Phase::Held { was: crate::Was::Closing(_), .. } => {
            false
        }
    }
}
