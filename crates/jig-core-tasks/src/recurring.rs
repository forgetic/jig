//! Core recurring procedure and its durable period cursor (domain/tasks.md, section 9).
//! A period trigger considers only its newest due period. The template and overlap choice live in
//! the task row; root supplies fresh task numbers, while this child owns the period allotment and
//! admits the whole batch with its cursor in one decision. It knows no wall clock or connector.
use crate::domain::{Domain, make, publish, record, task_mut};
use crate::{Active, Funder, Limits, New, Numbers, Party, Phase, RecurringOverlap, Request};
use alloc::boxed::Box;
use skein_lib::{Env, List, Queue, ReplyTo, Token};

fn active(phase: &Phase) -> bool {
    match phase {
        Phase::Active(_) => true,
        Phase::Waiting | Phase::Held { .. } | Phase::Closing(_) | Phase::Ended(_) => false,
    }
}

pub(crate) fn tick(domain: &mut Domain, env: &Env<Limits>, task: u64, period: u64, out: &mut Queue<Request>) {
    if !domain.ready() || period == 0 {
        return;
    }
    let Some(row) = record(domain, task) else { return };
    let Some(state) = &row.recurring else { return };
    if state.last_period >= period || !active(&row.phase) {
        return;
    }
    let project = row.project;
    let old_funder = row.funder;
    let live_delegates = !row.delegates.is_empty();
    let choice = state.template.overlap;
    let members = u32::try_from(state.template.batch.len()).expect("bounded template batch");
    let next = Funder::Period { project, period };
    if domain.funding.get(&next).is_none() {
        return;
    }
    if old_funder != next {
        task_mut(domain, task).expect("live recurring task").record.funder = next;
        publish(domain, env, task, out);
        crate::funders::retire(domain, out);
    }
    if live_delegates {
        let row = &mut task_mut(domain, task).expect("live recurring task").record;
        let state = row.recurring.as_mut().expect("core template");
        state.last_period = period;
        state.pending_period = match choice {
            RecurringOverlap::Skip => None,
            RecurringOverlap::Wait => Some(period),
        };
        publish(domain, env, task, out);
        return;
    }
    let row = &mut task_mut(domain, task).expect("recurring task live").record;
    row.made = 1;
    row.inbox = Box::new([]);
    publish(domain, env, task, out);
    out.push(Request::RecurringDue { task, period, members });
}

pub(crate) fn after_delegate(domain: &mut Domain, env: &Env<Limits>, task: u64, out: &mut Queue<Request>) {
    let Some(row) = record(domain, task) else { return };
    if !row.delegates.is_empty() {
        return;
    }
    let Some(state) = &row.recurring else { return };
    let Some(period) = state.pending_period else { return };
    let members = u32::try_from(state.template.batch.len()).expect("bounded template batch");
    let row = &mut task_mut(domain, task).expect("recurring task live").record;
    row.made = 1;
    row.inbox = Box::new([]);
    publish(domain, env, task, out);
    out.push(Request::RecurringDue { task, period, members });
}

pub(crate) fn make_batch(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    period: u64,
    numbers: &[u64],
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return;
    }
    let Some(row) = record(domain, task) else { return };
    let Some(state) = &row.recurring else { return };
    if !active(&row.phase)
        || !row.delegates.is_empty()
        || period < state.last_period
        || (period == state.last_period && state.pending_period != Some(period))
        || numbers.len() != state.template.batch.len()
        || numbers.is_empty()
    {
        return;
    }
    let project = row.project;
    let budget = row.authority.budget.spend;
    let funder = Funder::Recurring { project, task, period };
    let mut batch = state.template.batch.clone();
    for (index, member) in batch.iter_mut().enumerate() {
        member.number = *numbers.get(index).expect("template count checked");
        member.project = project;
        member.funder = funder;
        member.recurring = None;
        let mut dependencies = List::with_capacity(env.limits.dependencies);
        for dependency in &member.dependencies {
            let Ok(at) = usize::try_from(*dependency) else { return };
            let Some(index) = at.checked_sub(1) else { return };
            let Some(number) = numbers.get(index) else { return };
            if dependencies.push(*number).is_err() {
                return;
            }
        }
        member.dependencies = dependencies.into_boxed();
    }
    let Some(parent) = domain.funding.get(&Funder::Period { project, period }).copied() else { return };
    let mut saves = Queue::with_capacity(crate::max_out(&env.limits));
    if !crate::funders::carve_recurring(domain, project, task, period, budget, &mut saves) {
        return;
    }
    if crate::batch::check(domain, &env.limits, Party::Task(task), &batch).is_err() {
        domain.funding.remove(&funder);
        *domain.funding.get_mut(&Funder::Period { project, period }).expect("period retained") = parent;
        return;
    }
    for _ in 0..saves.len() {
        out.push(saves.pop().expect("saved ledger count"));
    }
    let state = task_mut(domain, task).expect("recurring task live").record.recurring.as_mut().expect("template");
    state.last_period = period;
    state.pending_period = None;
    task_mut(domain, task).expect("recurring task live").record.phase = Phase::Active(Active::Idle);
    publish(domain, env, task, out);
    make(domain, env, ReplyTo::new(Token::new(u64::MAX - 1)), Party::Task(task), batch, out);
}

pub(crate) fn valid_template(limits: &Limits, project: u32, budget: u64, batch: &[New]) -> bool {
    if batch.is_empty() || batch.len() > usize::try_from(limits.batch).expect("u32 fits") {
        return false;
    }
    let mut spend = 0_u64;
    for (index, member) in batch.iter().enumerate() {
        let Some(local) = index.checked_add(1) else { return false };
        if member.number != u64::try_from(local).expect("bounded batch")
            || member.project != project
            || member.recurring.is_some()
            || member.numbers
                != (Numbers { budget: member.authority.budget.spend, spent: 0, spent_below: 0, reserved: 0 })
            || !crate::batch::valid_spec(limits, &member.spec)
            || !crate::batch::valid_contract(limits, &member.contract)
            || !crate::batch::valid_authority(limits, &member.authority)
            || !crate::wake::valid(&member.wake)
        {
            return false;
        }
        let Some(next) = spend.checked_add(member.numbers.budget) else { return false };
        spend = next;
        for dependency in &member.dependencies {
            if *dependency == 0 || *dependency > u64::try_from(batch.len()).expect("bounded batch") {
                return false;
            }
        }
    }
    spend <= budget
}
