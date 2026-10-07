//! Admission of new priced turns and activation terminals over authentic task
//! counters.
//! Preflights lifecycle and eventual financial representability before mutation.
//! Keeps no receipt table; root owns exact transport payload replay proofs.
use crate::domain::{Domain, entrance, publish, record, refused, task_mut};
use crate::{Accepted, End, Limits, Refusal, Request, ResultFollowups, TaskResult};
use alloc::boxed::Box;
use skein_lib::{Env, Queue, ReplyTo};

fn check_charge(domain: &Domain, number: u64, cumulative: u64) -> Result<u64, Refusal> {
    let old = record(domain, number).expect("admission recipient live");
    let delta = cumulative.checked_sub(old.run_spent).ok_or(Refusal::Turn)?;
    if delta > old.run_reserved {
        return Err(Refusal::Funding);
    }
    let spent = old.numbers.spent.checked_add(delta).ok_or(Refusal::Funding)?;
    let _total = spent.checked_add(old.numbers.spent_below).ok_or(Refusal::Funding)?;
    if !crate::funders::representable(domain, number, delta) {
        return Err(Refusal::Funding);
    }
    Ok(spent)
}

fn post(
    domain: &mut Domain,
    environment: &Env<Limits>,
    number: u64,
    cumulative: u64,
    spent: u64,
    out: &mut Queue<Request>,
) {
    let task = task_mut(domain, number).expect("admitted recipient live");
    let delta = cumulative.checked_sub(task.record.run_spent).expect("monotonic checked");
    task.record.run_spent = cumulative;
    task.record.numbers.spent = spent;
    task.record.run_reserved = task.record.run_reserved.checked_sub(delta).expect("run allowance checked");
    task.record.numbers.reserved = task.record.numbers.reserved.checked_sub(delta).expect("run reservation checked");
    publish(domain, environment, number, out);
}

#[expect(clippy::too_many_arguments, reason = "one complete admission event")]
pub(crate) fn turn(
    domain: &mut Domain,
    environment: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    attempt: u64,
    turn: u32,
    read: Option<u64>,
    offered: Option<u64>,
    cumulative: u64,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Some(number), Refusal::NotReady, out);
    }
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let old = record(domain, number).expect("entrance checked");
    if attempt == 0 || old.attempt != attempt {
        return refused(to, Some(number), Refusal::Attempt, out);
    }
    if crate::run::run_attempt(&old.phase) != Some(attempt) || old.turn.checked_add(1) != Some(turn) {
        return refused(to, Some(number), Refusal::Turn, out);
    }
    let beyond_offer = match read {
        Some(number) => match offered {
            Some(high) => number > high,
            None => true,
        },
        None => false,
    };
    if beyond_offer || !crate::inbox::readable(domain, number, read) {
        return refused(to, Some(number), Refusal::Read, out);
    }
    let spent = match check_charge(domain, number, cumulative) {
        Ok(spent) => spent,
        Err(why) => return refused(to, Some(number), why, out),
    };
    let task = &mut task_mut(domain, number).expect("turn admitted").record;
    task.turn = turn;
    crate::inbox::take(task, read);
    task.ever_turned = true;
    post(domain, environment, number, cumulative, spent, out);
    out.push(Request::TurnAcknowledged { reply_to: to, task: number, attempt, turn, accepted: Accepted::New });
}

#[expect(clippy::too_many_arguments, reason = "one complete admission event")]
#[expect(clippy::too_many_lines, reason = "one charged result preflights and settles its follow-ups atomically")]
pub(crate) fn activation(
    domain: &mut Domain,
    environment: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    attempt: u64,
    end: End,
    saved: Option<Box<[crate::SavedResource]>>,
    cumulative: u64,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Some(number), Refusal::NotReady, out);
    }
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let old = record(domain, number).expect("entrance checked");
    if attempt == 0 || crate::run::run_attempt(&old.phase) != Some(attempt) || old.last_answer == Some(attempt) {
        return refused(to, Some(number), Refusal::Attempt, out);
    }
    if !crate::run::saved_within(saved.as_deref(), &environment.limits) {
        return refused(to, Some(number), Refusal::Contract, out);
    }
    let spent = match check_charge(domain, number, cumulative) {
        Ok(spent) => spent,
        Err(why) => return refused(to, Some(number), why, out),
    };
    let charge = cumulative.checked_sub(old.run_spent).expect("charge preflighted");
    match &end {
        End::Finished { result, cancel_delegates } => {
            if crate::run::result_bytes(result)
                > usize::try_from(environment.limits.result_bytes).expect("u32 fits usize")
            {
                return refused(to, Some(number), Refusal::Contract, out);
            }
            let closing = match old.phase {
                crate::Phase::Closing(_) | crate::Phase::Held { was: crate::Was::Closing(_), .. } => true,
                crate::Phase::Waiting
                | crate::Phase::Active(_)
                | crate::Phase::Held { was: crate::Was::Waiting | crate::Was::Active(_), .. }
                | crate::Phase::Ended(_) => false,
            };
            if !closing
                && crate::run::valid_result(&old.contract, result, &environment.limits)
                && !cancel_delegates
                && !old.delegates.is_empty()
            {
                return out.push(Request::Refused {
                    reply_to: to,
                    problem: crate::Problem::live_delegates(number, &old.delegates),
                });
            }
        }
        End::FinishedWithFollowups { result, followups } => {
            if crate::run::result_bytes(result)
                > usize::try_from(environment.limits.result_bytes).expect("u32 fits usize")
            {
                return refused(to, Some(number), Refusal::Contract, out);
            }
            if !old.narrowing
                && crate::run::invalid_followups(&old.contract, result, followups, &environment.limits).is_none()
            {
                match result {
                    TaskResult::Verdict { .. } => {}
                    TaskResult::Report { .. } | TaskResult::Change { .. } | TaskResult::Failure { .. } => {
                        return refused(to, Some(number), Refusal::Contract, out);
                    }
                }
                if !old.delegates.is_empty() {
                    return out.push(Request::Refused {
                        reply_to: to,
                        problem: crate::Problem::live_delegates(number, &old.delegates),
                    });
                }
                match followups {
                    ResultFollowups::Delegates(batch) => {
                        if let Err(problem) =
                            crate::batch::check_direct_followups(domain, &environment.limits, number, charge, batch)
                        {
                            return out.push(Request::Refused { reply_to: to, problem });
                        }
                    }
                    ResultFollowups::Proposal(proposal) => {
                        if !crate::proposals::check_result(domain, environment, number, proposal) {
                            return refused(to, Some(number), Refusal::Contract, out);
                        }
                    }
                }
            }
        }
        End::Parked | End::Failed(_) | End::Refused => {}
    }
    let result_admitted = match &end {
        End::FinishedWithFollowups { result, followups } => {
            !old.narrowing
                && crate::run::invalid_followups(&old.contract, result, followups, &environment.limits).is_none()
        }
        End::Finished { .. } | End::Parked | End::Failed(_) | End::Refused => false,
    };
    post(domain, environment, number, cumulative, spent, out);
    crate::run::activation(domain, environment, to, number, attempt, end.clone(), saved, out);
    match (&end, result_admitted) {
        (End::FinishedWithFollowups { followups: ResultFollowups::Delegates(batch), .. }, true) => {
            crate::domain::make_admitted(domain, environment, crate::Party::Task(number), batch.clone(), out);
        }
        (End::FinishedWithFollowups { followups: ResultFollowups::Proposal(proposal), .. }, true) => {
            crate::proposals::propose_result(domain, environment, (**proposal).clone(), out);
        }
        (End::FinishedWithFollowups { .. }, false)
        | (End::Finished { .. } | End::Parked | End::Failed(_) | End::Refused, _) => {}
    }
}
