//! Person-task lifecycle at the tasks hub (domain/tasks.md, section 5.4;
//! domain/people.md, section 8). Root authenticates people and role membership;
//! this child retains the one durable role claim and checks the answer contract.
use crate::domain::{Domain, entrance, publish, record, refused, task_mut};
use crate::{Active, Closing, Ending, Executor, Limits, PersonAddress, Phase, Refusal, Request, Stage, TaskResult};
use skein_lib::{Env, Queue, ReplyTo};

pub(crate) fn take(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    person: u64,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let row = record(domain, number).expect("entrance names task");
    match row.executor {
        Executor::Person(PersonAddress::Role(_)) => {}
        Executor::Person(PersonAddress::Person(_)) | Executor::Agent { .. } | Executor::Procedure { .. } => {
            return refused(to, Some(number), Refusal::Executor, out);
        }
    }
    if !active(&row.phase) || row.taken_by.is_some() || person == 0 {
        return refused(to, Some(number), Refusal::State, out);
    }
    task_mut(domain, number).expect("live person task").record.taken_by = Some(person);
    publish(domain, env, number, out);
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn hand_back(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    person: u64,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let row = record(domain, number).expect("entrance names task");
    match row.executor {
        Executor::Person(PersonAddress::Role(_)) => {}
        Executor::Person(PersonAddress::Person(_)) | Executor::Agent { .. } | Executor::Procedure { .. } => {
            return refused(to, Some(number), Refusal::Executor, out);
        }
    }
    if !active(&row.phase) || row.taken_by != Some(person) {
        return refused(to, Some(number), Refusal::State, out);
    }
    task_mut(domain, number).expect("live person task").record.taken_by = None;
    publish(domain, env, number, out);
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn answer(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    person: u64,
    result: TaskResult,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let row = record(domain, number).expect("entrance names task");
    let addressed = match row.executor {
        Executor::Person(PersonAddress::Person(address)) => address == person && address != 0,
        Executor::Person(PersonAddress::Role(_)) => row.taken_by == Some(person) && person != 0,
        Executor::Agent { .. } | Executor::Procedure { .. } => false,
    };
    if !addressed || !active(&row.phase) {
        return refused(to, Some(number), Refusal::State, out);
    }
    if !row.delegates.is_empty() {
        return refused(to, Some(number), Refusal::LiveDelegates, out);
    }
    if !crate::run::valid_result(&row.contract, &result, &env.limits) {
        return refused(to, Some(number), Refusal::Contract, out);
    }
    let ending = match result {
        TaskResult::Failure { reason } => Ending::Failed { reason },
        TaskResult::Report { .. } | TaskResult::Verdict { .. } | TaskResult::Change { .. } => Ending::Done(result),
    };
    let row = &mut task_mut(domain, number).expect("live person task").record;
    row.taken_by = None;
    row.phase = Phase::Closing(Closing { stage: Stage::Delegates, ending });
    publish(domain, env, number, out);
    out.push(Request::Done { reply_to: to });
}

fn active(phase: &Phase) -> bool {
    match phase {
        Phase::Active(Active::Due | Active::Idle) => true,
        Phase::Waiting
        | Phase::Active(
            Active::Preparing | Active::Claimed { .. } | Active::Running { .. } | Active::BackingOff { .. },
        )
        | Phase::Closing(_)
        | Phase::Held { .. }
        | Phase::Ended(_) => false,
    }
}
