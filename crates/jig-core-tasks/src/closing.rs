//! Bounded dependency and closing cascades (domain/tasks.md, sections 5.1 and 5.6). Tasks owns live topology and actual financial settlement;
//! root completes Close obligations. No historical result credit or inbox
//! remains here, and ending a task does not replay its result after restart.
use crate::domain::{Domain, activate, fact, publish, record, snapshot, task_mut};
use crate::{
    Active, Closing, Ending, Fact, Hold, Key, Limits, Party, Phase, Request, Stage, Status, Stored, TaskResult, Was,
};
use alloc::boxed::Box;
use skein_lib::{Env, List, Queue};

pub(crate) fn named_by_live(domain: &Domain, ended: u64) -> bool {
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("indexed live task");
        if crate::batch::contains(&row.dependencies, ended)
            || crate::batch::contains(&row.spec.inputs, ended)
            || crate::batch::contains(&row.references, ended)
        {
            return true;
        }
    }
    false
}

pub(crate) fn remember_stub(domain: &mut Domain, stub: crate::Stub, out: &mut Queue<Request>) {
    if !domain.ready() || stub.task == 0 || stub.result.raw() != stub.task || !named_by_live(domain, stub.task) {
        return;
    }
    if domain.stubs.contains_key(&stub.task) {
        return;
    }
    domain.stubs.insert(stub.task, stub).expect("new historical input fits bounded live names");
    out.push(Request::Save { record: Stored::Stub(stub) });
}

fn prune_stubs(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let mut unused = List::with_capacity(crate::limits::stub_capacity(&env.limits).expect("valid stub bound"));
    for (number, _) in &domain.stubs {
        if !named_by_live(domain, *number) {
            unused.push(*number).expect("stub count bounded");
        }
    }
    for number in unused.into_boxed() {
        domain.stubs.remove(&number);
        out.push(Request::Erase { key: Key::Stub(number) });
    }
}

pub(crate) fn status(ending: &Ending) -> Status {
    match ending {
        Ending::Done(_) => Status::Done,
        Ending::Failed { .. } => Status::Failed,
        Ending::Cancelled { .. } => Status::Cancelled,
    }
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
                Party::Task(parent) => Some(parent),
                Party::Person(_) | Party::Deployment { .. } => None,
            },
            None => None,
        };
    }
    false
}

pub(crate) fn cancel_delegates(
    domain: &mut Domain,
    env: &Env<Limits>,
    number: u64,
    reason: &[u8],
    out: &mut Queue<Request>,
) {
    let mut delegates = List::with_capacity(env.limits.delegates);
    for delegate in &record(domain, number).expect("finishing task live").delegates {
        delegates.push(*delegate).expect("live delegates bounded");
    }
    for delegate in delegates.into_boxed() {
        cancel_tree(domain, env, delegate, reason, out);
    }
}

fn previous_result(phase: &Phase) -> Option<TaskResult> {
    match phase {
        Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. } => match &closing.ending {
            Ending::Done(result) => Some(result.clone()),
            Ending::Failed { reason } => Some(TaskResult::Failure { reason: reason.clone() }),
            Ending::Cancelled { result, .. } => result.clone(),
        },
        Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => None,
        Phase::Ended(_) => unreachable!("cancel targets live task"),
    }
}

pub(crate) fn cancel_tree(
    domain: &mut Domain,
    env: &Env<Limits>,
    ancestor: u64,
    reason: &[u8],
    out: &mut Queue<Request>,
) {
    let mut selected = List::with_capacity(env.limits.tasks);
    for (number, _) in &domain.names {
        if below(domain, *number, ancestor, env.limits.tasks) {
            selected.push(*number).expect("selected subtree bounded by live set");
        }
    }
    // Reverse depth traversal gives every descendant its cancellation before
    // the parent can advance. No recursion and no mutable graph iterator.
    let mut deepest = 0_u32;
    for number in &selected {
        deepest = deepest.max(record(domain, *number).expect("selected task live").depth);
    }
    for depth in (0..=deepest).rev() {
        for number in &selected {
            let old = record(domain, *number).expect("selected task live");
            if old.depth != depth {
                continue;
            }
            let attempt = crate::run::run_attempt(&old.phase);
            let partial = previous_result(&old.phase);
            let stage = match &old.phase {
                Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. } => closing.stage,
                Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => {
                    match attempt {
                        Some(attempt) => Stage::Run { attempt },
                        None => Stage::Delegates,
                    }
                }
                Phase::Ended(_) => unreachable!("selected task live"),
            };
            // Reuse the root's already-open closing gate; replaying Close is
            // required to associate the final cancelled result with it.
            let stage = match stage {
                Stage::Settled => Stage::Delegates,
                Stage::Run { .. } | Stage::Delegates | Stage::Effects | Stage::Releases => stage,
            };
            task_mut(domain, *number).expect("selected task live").record.phase =
                Phase::Closing(Closing { stage, ending: Ending::Cancelled { reason: reason.into(), result: partial } });
            publish(domain, env, *number, out);
            if let Some(attempt) = attempt {
                out.push(Request::Stop { task: *number, attempt });
            }
            if stage == Stage::Effects {
                let ending = match &record(domain, *number).expect("selected task live").phase {
                    Phase::Closing(closing) => closing.ending.clone(),
                    Phase::Waiting | Phase::Active(_) | Phase::Held { .. } | Phase::Ended(_) => {
                        unreachable!("cancel installed closing")
                    }
                };
                out.push(Request::Close { task: *number, ending });
            }
            if stage == Stage::Releases {
                let ending = match &record(domain, *number).expect("selected task live").phase {
                    Phase::Closing(closing) => closing.ending.clone(),
                    Phase::Waiting | Phase::Active(_) | Phase::Held { .. } | Phase::Ended(_) => {
                        unreachable!("cancel installed closing")
                    }
                };
                out.push(Request::Release { task: *number, ending });
            }
        }
    }
}

pub(crate) fn effects_settled(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    let Some(task) = task_mut(domain, number) else { return };
    match &mut task.record.phase {
        Phase::Closing(closing) if closing.stage == Stage::Effects => {
            closing.stage = Stage::Releases;
            let ending = closing.ending.clone();
            publish(domain, env, number, out);
            out.push(Request::Release { task: number, ending });
        }
        Phase::Held { was: Was::Closing(closing), .. } if closing.stage == Stage::Effects => {
            closing.stage = Stage::Releases;
            publish(domain, env, number, out);
        }
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Held { .. } | Phase::Ended(_) => {}
    }
}

pub(crate) fn settled(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    let Some(task) = task_mut(domain, number) else {
        return;
    };
    match &mut task.record.phase {
        Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. }
            if closing.stage == Stage::Releases =>
        {
            closing.stage = Stage::Settled;
            publish(domain, env, number, out);
        }
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Held { .. } | Phase::Ended(_) => {}
    }
}

pub(crate) fn progress(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    // Every pass must advance a phase or end a task to need another pass;
    // immutable dependency DAGs and delegation depth bound the cascade.
    for _ in 0..env.limits.tasks.saturating_add(1) {
        let mut changed = false;
        let numbers = snapshot(domain, env.limits.tasks);
        for number in numbers.into_boxed() {
            let Some(task) = record(domain, number) else {
                continue;
            };
            match &task.phase {
                Phase::Waiting => {
                    let mut ready = true;
                    for dependency in &task.waiting_on {
                        if domain.names.contains_key(dependency) {
                            ready = false;
                        } else {
                            unreachable!("unfinished dependency must remain live");
                        }
                    }
                    if ready {
                        if task.holdings.is_empty() {
                            crate::holds::take(domain, env, number, out);
                            task_mut(domain, number).expect("waiting task live").record.phase =
                                Phase::Active(Active::Due);
                            publish(domain, env, number, out);
                            activate(domain, number, out);
                            changed = true;
                        } else {
                            crate::holds::wait(domain, env, number, out);
                        }
                    }
                }
                Phase::Closing(closing) => match closing.stage {
                    Stage::Delegates
                        if task.delegates.is_empty()
                            && !task.result_proposal
                            && !crate::funders::funded_live(domain, number) =>
                    {
                        let ending = closing.ending.clone();
                        let task = task_mut(domain, number).expect("closing task live");
                        task.record.phase = Phase::Closing(Closing { stage: Stage::Effects, ending: ending.clone() });
                        publish(domain, env, number, out);
                        out.push(Request::Close { task: number, ending });
                        changed = true;
                    }
                    Stage::Run { .. } | Stage::Effects | Stage::Releases | Stage::Delegates => {}
                    Stage::Settled => {
                        let requester = task.requester;
                        end_task(domain, env, number, out);
                        if match requester {
                            Party::Task(_) => true,
                            Party::Person(_) | Party::Deployment { .. } => false,
                        } {
                            // Root must insert the delegate's result before its
                            // requester can advance its own closing gate.
                            return;
                        }
                        changed = true;
                    }
                },
                Phase::Active(_) | Phase::Held { .. } | Phase::Ended(_) => {}
            }
        }
        if let Some(number) = crate::holds::next_ready(domain, &env.limits) {
            crate::holds::take(domain, env, number, out);
            task_mut(domain, number).expect("waiting task live").record.phase = Phase::Active(Active::Due);
            publish(domain, env, number, out);
            activate(domain, number, out);
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

fn end_task(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let task = record(domain, number).expect("settled task live");
    let ending = match &task.phase {
        Phase::Closing(closing) => match closing.stage {
            Stage::Settled => &closing.ending,
            Stage::Run { .. } | Stage::Delegates | Stage::Effects | Stage::Releases => {
                unreachable!("ending follows settlement")
            }
        },
        Phase::Waiting | Phase::Active(_) | Phase::Held { .. } | Phase::Ended(_) => {
            unreachable!("ending follows settlement")
        }
    };
    assert!(task.delegates.is_empty(), "all delegates ended before requester");
    let ending = ending.clone();
    let requester = task.requester;
    let status = status(&ending);
    let subscriptions = task.subscriptions.clone();
    let mut ended = task.clone();
    for subscription in &subscriptions {
        domain.timers.cancel(subscription.number);
        match subscription.kind {
            crate::SubscriptionKind::Topic { connector, .. } => {
                out.push(Request::EndTopic { task: number, subscription: subscription.number, connector });
            }
            crate::SubscriptionKind::Task { .. } | crate::SubscriptionKind::Timer { .. } => {}
        }
    }
    crate::funders::end(domain, env, number, out);
    ended.phase = Phase::Ended(ending.clone());
    ended.ended_at = Some(env.wall);
    let id = domain.names.remove(&number).expect("ending name exists");
    crate::holds::release(domain, number);
    crate::refs::end(domain, env, number, out);
    domain.tasks.retire(id);
    domain.alarms.cancel(number);
    out.push(Request::Erase { key: Key::Live(number) });
    out.push(Request::Save { record: Stored::Ended(Box::new(ended)) });
    let dependents = snapshot(domain, env.limits.tasks);
    for dependent in dependents.into_boxed() {
        let task = task_mut(domain, dependent).expect("dependent snapshot is live");
        if !crate::batch::contains(&task.record.waiting_on, number) {
            continue;
        }
        let mut remaining = List::with_capacity(env.limits.dependencies);
        for &waiting in &task.record.waiting_on {
            if waiting != number {
                remaining.push(waiting).expect("dependency subset bounded");
            }
        }
        task.record.waiting_on = remaining.into_boxed();
        match status {
            Status::Done => {}
            Status::Failed | Status::Cancelled => match &task.record.phase {
                Phase::Closing(_) | Phase::Held { was: Was::Closing(_), .. } | Phase::Ended(_) => {}
                Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => {
                    task.record.phase = Phase::Held {
                        was: crate::run::was(core::mem::replace(&mut task.record.phase, Phase::Waiting)),
                        why: Hold::Dependency(number),
                    };
                }
            },
        }
        publish(domain, env, dependent, out);
    }
    match requester {
        Party::Task(parent) => {
            let mut remaining = List::with_capacity(env.limits.delegates);
            let task = task_mut(domain, parent).expect("requester remains live until all delegates end");
            for delegate in &task.record.delegates {
                if *delegate != number {
                    remaining.push(*delegate).expect("remaining delegates bounded");
                }
            }
            task.record.delegates = remaining.into_boxed();
            publish(domain, env, parent, out);
        }
        Party::Person(_) | Party::Deployment { .. } => {}
    }
    if named_by_live(domain, number) {
        let stub = crate::Stub { task: number, phase: status, result: skein_lib::Token::new(number) };
        domain.stubs.insert(number, stub).expect("one stub for a bounded live reference");
        out.push(Request::Save { record: Stored::Stub(stub) });
    }
    prune_stubs(domain, env, out);
    let state = match status {
        Status::Done => crate::NoticeState::Done,
        Status::Failed => crate::NoticeState::Failed,
        Status::Cancelled => crate::NoticeState::Cancelled,
    };
    let words = match &ending {
        Ending::Done(
            TaskResult::Report { words } | TaskResult::Verdict { words, .. } | TaskResult::Change { words, .. },
        ) => words.as_ref(),
        Ending::Done(TaskResult::Failure { reason }) | Ending::Failed { reason } | Ending::Cancelled { reason, .. } => {
            reason.as_ref()
        }
    };
    crate::subscriptions::notify_state(domain, number, state, words, out);
    out.push(Request::Ended { task: number, requester, ending });
    fact(domain, Fact::Ended { task: number, status });
}
