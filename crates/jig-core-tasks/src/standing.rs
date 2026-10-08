//! Period renewal for subscribed top-level procedures (domain/tasks.md, section 10;
//! domain/authority.md, section 7). Old delegates keep their old-period source.
use crate::domain::{Domain, publish, record, task_mut};
use crate::{Executor, Funder, FundingRecord, Limits, Numbers, Phase, Request};
use skein_lib::{Env, List, Queue};

#[expect(clippy::too_many_lines, reason = "one atomic period rollover with full preflight and durable postings")]
pub(crate) fn renew(domain: &mut Domain, env: &Env<Limits>, number: u64, period: u64, out: &mut Queue<Request>) {
    if !domain.ready() || period == 0 {
        return;
    }
    let Some(task) = record(domain, number) else { return };
    let procedure = match task.executor {
        Executor::Procedure { .. } => true,
        Executor::Agent { .. } | Executor::Person(_) => false,
    };
    let live = match task.phase {
        Phase::Waiting | Phase::Active(_) | Phase::Held { .. } => true,
        Phase::Closing(_) | Phase::Ended(_) => false,
    };
    if !procedure || !live || task.root != number || task.recurring.is_some() || task.subscriptions.is_empty() {
        return;
    }
    let project = task.project;
    let source = task.funder;
    let old_period = match source {
        Funder::Period { project: owner, period: old } | Funder::Pool { project: owner, period: old, .. }
            if owner == project && old < period =>
        {
            old
        }
        Funder::Task(_) | Funder::Recurring { .. } | Funder::Period { .. } | Funder::Pool { .. } => return,
    };
    let old = Funder::Period { project, period: old_period };
    let fresh = Funder::Period { project, period };
    let archived = Funder::Recurring { project, task: number, period: old_period };
    let budget = task.numbers.budget;
    let Some(spent) = crate::funders::total(task.numbers) else { return };
    let reserved = task.numbers.reserved;
    let made = task.made;
    if task.run_reserved != 0
        || domain.funding.contains_key(&archived)
        || domain.funding.len() == domain.funding.capacity()
        || match spent.checked_add(reserved) {
            Some(used) => used > budget,
            None => true,
        }
    {
        return;
    }
    let Some(old_row) = domain.funding.get(&old) else { return };
    let Some(new_row) = domain.funding.get(&fresh) else { return };
    let mut new_numbers = new_row.numbers;
    let Some(next_reserved) = new_numbers.reserved.checked_add(budget) else { return };
    new_numbers.reserved = next_reserved;
    if old_row.closed || new_row.closed || crate::funders::available(new_numbers).is_none() {
        return;
    }
    match source {
        Funder::Pool { .. } => {
            let Some(pool) = domain.funding.get(&source) else { return };
            if pool.closed
                || pool.parent != Some(old)
                || pool.numbers.budget.checked_sub(budget).is_none()
                || pool.numbers.reserved.checked_sub(budget).is_none()
            {
                return;
            }
        }
        Funder::Period { .. } => {}
        Funder::Task(_) | Funder::Recurring { .. } => unreachable!("standing source checked"),
    }
    let ledger = FundingRecord {
        funder: archived,
        parent: Some(old),
        numbers: Numbers { budget, spent: 0, spent_below: spent, reserved },
        made,
        closed: false,
    };
    assert!(domain.funding.insert(archived, ledger) == Ok(None), "standing ledger preflighted");
    match source {
        Funder::Pool { .. } => {
            let pool = domain.funding.get_mut(&source).expect("pool preflighted");
            pool.numbers.budget = pool.numbers.budget.checked_sub(budget).expect("pool budget preflighted");
            pool.numbers.reserved = pool.numbers.reserved.checked_sub(budget).expect("pool reservation preflighted");
            crate::funders::save_funding(domain, source, out);
        }
        Funder::Period { .. } => {}
        Funder::Task(_) | Funder::Recurring { .. } => unreachable!("standing source checked"),
    }
    domain.funding.get_mut(&fresh).expect("new period preflighted").numbers = new_numbers;
    crate::funders::save_funding(domain, archived, out);
    crate::funders::save_funding(domain, fresh, out);
    let mut children = List::with_capacity(env.limits.tasks);
    for (child, _) in &domain.names {
        if record(domain, *child).expect("live name").funder == Funder::Task(number) {
            children.push(*child).expect("bounded live task set");
        }
    }
    for child in &children {
        task_mut(domain, *child).expect("live funded delegate").record.funder = archived;
        publish(domain, env, *child, out);
    }
    let task = &mut task_mut(domain, number).expect("standing task live").record;
    task.funder = fresh;
    task.numbers = Numbers { budget, spent: 0, spent_below: 0, reserved: 0 };
    task.made = 1;
    publish(domain, env, number, out);
    crate::funders::retire(domain, out);
}
