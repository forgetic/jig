//! Fenced agent-task preparation, claims, activation terminals and holds
//! (domain/tasks.md, sections 5.2, 5.5 and 5.6). Root supplies actual run
//! events; tasks checks lifecycle/result shape and emits bounded state changes.
//! Priced inputs use combined accounting admission; exact replay is root-owned.
use crate::domain::{Domain, entrance, fact, publish, record, refused, task_mut};
use crate::{
    Accepted, Active, Class, Closing, Contract, End, Ending, Fact, Hold, Limits, Phase, Refusal, Request,
    SavedResource, Stage, TaskResult, Tries, Was,
};
use alloc::boxed::Box;
use skein_lib::{Env, Queue, ReplyTo};

pub(crate) fn saved_within(saved: Option<&[SavedResource]>, limits: &Limits) -> bool {
    let Some(resources) = saved else { return true };
    if resources.len() > usize::try_from(limits.saved_resources).expect("u32 fits usize") {
        return false;
    }
    let mut previous = None;
    for resource in resources {
        if resource.path.is_empty()
            || resource.path.len() > usize::try_from(limits.authority_segments).expect("u32 fits usize")
        {
            return false;
        }
        let mut bytes = 0usize;
        for segment in &resource.path {
            let Some(sum) = bytes.checked_add(segment.len()) else { return false };
            bytes = sum;
        }
        if bytes > usize::try_from(limits.authority_bytes).expect("u32 fits usize") {
            return false;
        }
        if let Some(before) = previous
            && before >= resource
        {
            return false;
        }
        previous = Some(resource);
    }
    true
}

pub(crate) fn prepare(domain: &mut Domain, env: &Env<Limits>, to: ReplyTo, number: u64, out: &mut Queue<Request>) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let task = task_mut(domain, number).expect("entrance names task");
    match task.record.executor {
        crate::Executor::Agent { .. } => {}
        crate::Executor::Procedure { .. } | crate::Executor::Person(_) => {
            return refused(to, Some(number), Refusal::Executor, out);
        }
    }
    if task.record.phase != Phase::Active(Active::Due) {
        return refused(to, Some(number), Refusal::State, out);
    }
    task.record.phase = Phase::Active(Active::Preparing);
    publish(domain, env, number, out);
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn claim(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    attempt: u64,
    budget: u64,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let task = task_mut(domain, number).expect("entrance names task");
    if attempt <= task.record.attempt {
        return refused(to, Some(number), Refusal::Attempt, out);
    }
    if task.record.phase != Phase::Active(Active::Preparing) {
        return refused(to, Some(number), Refusal::State, out);
    }
    let fits = match crate::funders::available(task.record.numbers) {
        Some(left) => budget != 0 && budget <= left,
        None => false,
    };
    if !fits {
        return refused(to, Some(number), Refusal::Funding, out);
    }
    task.record.numbers.reserved = task.record.numbers.reserved.checked_add(budget).expect("available budget");
    task.record.run_reserved = budget;
    task.record.attempt = attempt;
    task.record.run_spent = 0;
    task.record.turn = 0;
    task.record.phase = Phase::Active(Active::Claimed { attempt });
    publish(domain, env, number, out);
    fact(domain, Fact::Claimed { task: number, attempt });
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn started(domain: &mut Domain, env: &Env<Limits>, number: u64, attempt: u64, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    if let Some(task) = task_mut(domain, number)
        && task.record.phase == Phase::Active(Active::Claimed { attempt })
    {
        task.record.phase = Phase::Active(Active::Running { attempt });
        publish(domain, env, number, out);
    }
}

pub(crate) fn run_attempt(phase: &Phase) -> Option<u64> {
    match phase {
        Phase::Active(Active::Claimed { attempt } | Active::Running { attempt })
        | Phase::Closing(Closing { stage: Stage::Run { attempt }, .. })
        | Phase::Held {
            was:
                Was::Active(Active::Claimed { attempt } | Active::Running { attempt })
                | Was::Closing(Closing { stage: Stage::Run { attempt }, .. }),
            ..
        } => Some(*attempt),
        Phase::Waiting
        | Phase::Active(Active::Idle | Active::Due | Active::Preparing | Active::BackingOff { .. })
        | Phase::Closing(Closing {
            stage: Stage::Delegates | Stage::Effects | Stage::Releases | Stage::Settled, ..
        })
        | Phase::Held {
            was:
                Was::Waiting
                | Was::Active(Active::Idle | Active::Due | Active::Preparing | Active::BackingOff { .. })
                | Was::Closing(Closing {
                    stage: Stage::Delegates | Stage::Effects | Stage::Releases | Stage::Settled, ..
                }),
            ..
        }
        | Phase::Ended(_) => None,
    }
}

pub(crate) fn valid_result(contract: &Contract, result: &TaskResult, limits: &Limits) -> bool {
    match result {
        TaskResult::Failure { reason } => match contract {
            Contract::Report { .. } | Contract::Verdict { .. } | Contract::Change { .. } => {
                reason.len() <= usize::try_from(limits.result_bytes).expect("u32 fits usize")
            }
        },
        TaskResult::Report { words } => match contract {
            Contract::Report { words: limit } => words.len() <= usize::try_from(*limit).expect("u32 fits usize"),
            Contract::Verdict { .. } | Contract::Change { .. } => false,
        },
        TaskResult::Verdict { code, words } => match contract {
            Contract::Verdict { choices } => {
                for choice in choices {
                    if choice.code == *code && words.len() <= usize::try_from(choice.words).expect("u32 fits usize") {
                        return true;
                    }
                }
                false
            }
            Contract::Report { .. } | Contract::Change { .. } => false,
        },
        TaskResult::Change { connector: actual_connector, kind: actual_kind, words, .. } => match contract {
            Contract::Change { connector, kind, words: limit } => {
                connector == actual_connector
                    && kind == actual_kind
                    && words.len() <= usize::try_from(*limit).expect("u32 fits usize")
            }
            Contract::Report { .. } | Contract::Verdict { .. } => false,
        },
    }
}

pub(crate) fn result_bytes(result: &TaskResult) -> usize {
    match result {
        TaskResult::Report { words } | TaskResult::Verdict { words, .. } | TaskResult::Change { words, .. } => {
            words.len()
        }
        TaskResult::Failure { reason } => reason.len(),
    }
}

fn ending(result: TaskResult) -> Ending {
    match result {
        TaskResult::Failure { reason } => Ending::Failed { reason },
        TaskResult::Report { .. } | TaskResult::Verdict { .. } | TaskResult::Change { .. } => Ending::Done(result),
    }
}

#[expect(clippy::too_many_arguments, reason = "one fenced activation terminal")]
pub(crate) fn activation(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    attempt: u64,
    end: End,
    saved: Option<Box<[SavedResource]>>,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Some(number), Refusal::NotReady, out);
    }
    let Some(old) = record(domain, number) else {
        return refused(to, Some(number), Refusal::Unknown, out);
    };
    if old.last_answer == Some(attempt) {
        return out.push(Request::Acknowledged { reply_to: to, task: number, attempt, accepted: Accepted::Already });
    }
    if run_attempt(&old.phase) != Some(attempt) {
        return refused(to, Some(number), Refusal::Attempt, out);
    }
    if !saved_within(saved.as_deref(), &env.limits) {
        return refused(to, Some(number), Refusal::Contract, out);
    }
    let held = match old.phase {
        Phase::Held { why, .. } => Some(why),
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => None,
    };
    let closing = match &old.phase {
        Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. } => Some(closing.clone()),
        Phase::Waiting
        | Phase::Active(_)
        | Phase::Held { was: Was::Waiting | Was::Active(_), .. }
        | Phase::Ended(_) => None,
    };
    let narrowed = old.narrowing;
    let end = if narrowed { End::Parked } else { end };
    let end = match end {
        End::Finished { result, cancel_delegates } => {
            if valid_result(&old.contract, &result, &env.limits) {
                End::Finished { result, cancel_delegates }
            } else {
                End::Failed(Class::Invalid)
            }
        }
        End::Parked | End::Failed(_) | End::Refused => end,
    };
    let refuses_live = match &end {
        End::Finished { cancel_delegates, .. } => !cancel_delegates,
        End::Parked | End::Failed(_) | End::Refused => false,
    };
    if closing.is_none() && refuses_live && !old.delegates.is_empty() {
        return refused(to, Some(number), Refusal::LiveDelegates, out);
    }
    let next = if let Some(mut closing) = closing {
        closing.stage = Stage::Delegates;
        match &mut closing.ending {
            Ending::Cancelled { result: partial, .. } => match end {
                End::Finished { result, .. } => *partial = Some(result),
                End::Parked | End::Failed(_) | End::Refused => {}
            },
            Ending::Done(_) | Ending::Failed { .. } => {}
        }
        Phase::Closing(closing)
    } else {
        match end {
            End::Finished { result, cancel_delegates } => {
                if cancel_delegates {
                    crate::closing::cancel_delegates(domain, env, number, b"", out);
                }
                Phase::Closing(Closing { stage: Stage::Delegates, ending: ending(result) })
            }
            End::Parked => {
                let task = task_mut(domain, number).expect("terminal names live task");
                task.record.tries = Tries::NONE;
                task.record.refusals = 0;
                if narrowed { Phase::Active(Active::Due) } else { Phase::Active(Active::Idle) }
            }
            End::Failed(class) => failure(domain, env, number, class),
            End::Refused => pause(domain, env, number),
        }
    };
    let task = task_mut(domain, number).expect("terminal names live task");
    task.record.numbers.reserved =
        task.record.numbers.reserved.checked_sub(task.record.run_reserved).expect("run held");
    task.record.run_reserved = 0;
    task.record.last_answer = Some(attempt);
    task.record.narrowing = false;
    if let Some(tags) = saved {
        task.record.saved = tags;
    }
    task.record.phase = match held {
        Some(why) => Phase::Held { was: was(next), why },
        None => next,
    };
    publish(domain, env, number, out);
    if narrowed && record(domain, number).expect("amended task live").phase == Phase::Active(Active::Due) {
        crate::domain::activate(domain, number, out);
    }
    out.push(Request::Acknowledged { reply_to: to, task: number, attempt, accepted: Accepted::New });
}

pub(crate) fn was(phase: Phase) -> Was {
    match phase {
        Phase::Waiting => Was::Waiting,
        Phase::Active(active) => Was::Active(active),
        Phase::Closing(closing) => Was::Closing(closing),
        Phase::Held { was, .. } => was,
        Phase::Ended(_) => unreachable!("ended task cannot be held"),
    }
}

fn wall_after(env: &Env<Limits>, duration: skein_lib::Duration) -> skein_lib::Wall {
    skein_lib::Wall::from_nanos(env.wall.as_nanos().saturating_add(duration.as_nanos()))
}

fn failure(domain: &mut Domain, env: &Env<Limits>, number: u64, class: Class) -> Phase {
    let task = task_mut(domain, number).expect("failure names live task");
    task.record.tries.add(class);
    task.record.refusals = 0;
    let times = task.record.tries.of(class);
    let retry = env.limits.retries.of(class);
    fact(domain, Fact::Failed { task: number, class });
    if times > retry.retries {
        fact(domain, Fact::Held { task: number, why: Hold::Failures(class) });
        Phase::Held { was: Was::Active(Active::Due), why: Hold::Failures(class) }
    } else {
        let duration = crate::failures::backoff(times, retry, &mut domain.rng);
        Phase::Active(Active::BackingOff { until: wall_after(env, duration) })
    }
}

fn pause(domain: &mut Domain, env: &Env<Limits>, number: u64) -> Phase {
    let task = task_mut(domain, number).expect("pause names live task");
    task.record.refusals = task.record.refusals.saturating_add(1);
    let times = task.record.refusals;
    let duration = crate::failures::backoff(times, env.limits.retries.transient, &mut domain.rng);
    Phase::Active(Active::BackingOff { until: wall_after(env, duration) })
}

pub(crate) fn preparation_failed(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    if let Some(task) = record(domain, number)
        && task.phase == Phase::Active(Active::Preparing)
    {
        let phase = pause(domain, env, number);
        task_mut(domain, number).expect("prepared task exists").record.phase = phase;
        publish(domain, env, number, out);
    }
}

pub(crate) fn hold(domain: &mut Domain, env: &Env<Limits>, number: u64, why: Hold, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    let Some(task) = record(domain, number) else {
        return;
    };
    match task.phase {
        Phase::Held { .. } | Phase::Ended(_) => return,
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) => {}
    }
    if let Some(attempt) = run_attempt(&task.phase) {
        out.push(Request::Stop { task: number, attempt });
    }
    let task = task_mut(domain, number).expect("hold names live task");
    let old = core::mem::replace(&mut task.record.phase, Phase::Waiting);
    task.record.phase = Phase::Held { was: was(old), why };
    publish(domain, env, number, out);
    fact(domain, Fact::Held { task: number, why });
}
