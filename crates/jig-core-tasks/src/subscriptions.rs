//! Standing task and timer interests with merged inbox hints (domain/tasks.md,
//! sections 7.2 and 7.4). Each subscription stays in its owner's task row.
//! Root assigns numbers and translates state/timer requests into messages in
//! the same decision; connector topics are attached in session 07.
use crate::domain::{Domain, entrance, publish, record, refused, task_mut};
use crate::{
    Limits, MessageKind, NewsClass, NoticeState, Phase, Refusal, Request, Subscription, SubscriptionKind, Word,
};
use alloc::boxed::Box;
use skein_lib::{Duration, Env, List, Queue, ReplyTo, Wall};

fn hint_of(kind: MessageKind) -> Option<u64> {
    match kind {
        MessageKind::Notice { subscription, .. }
        | MessageKind::Timer { subscription }
        | MessageKind::News { subscription, .. } => Some(subscription),
        MessageKind::Words
        | MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Amendment { .. }
        | MessageKind::Question
        | MessageKind::Answer { .. }
        | MessageKind::Result(_) => None,
    }
}

pub(crate) fn credit(task: &crate::TaskRecord, limits: &Limits) -> Option<(usize, usize)> {
    let mut count = 0_usize;
    let mut bytes = 0_usize;
    for subscription in &task.subscriptions {
        let mut waiting = false;
        for word in &task.inbox {
            if hint_of(word.kind) == Some(subscription.number) {
                waiting = true;
            }
        }
        if !waiting {
            count = count.checked_add(1)?;
            bytes = bytes.checked_add(usize::try_from(limits.result_bytes.max(limits.message_bytes)).ok()?)?;
        }
    }
    Some((count, bytes))
}

fn find(task: &crate::TaskRecord, number: u64) -> Option<Subscription> {
    for subscription in &task.subscriptions {
        if subscription.number == number {
            return Some(*subscription);
        }
    }
    None
}

fn arm(domain: &mut Domain, env: &Env<Limits>, subscription: Subscription) {
    match subscription.kind {
        SubscriptionKind::Timer { at, .. } => {
            let due = env.now.saturating_add(Duration::from_nanos(at.as_nanos().saturating_sub(env.wall.as_nanos())));
            assert!(domain.timers.arm(subscription.number, due).is_ok(), "timer subscription room admitted");
        }
        SubscriptionKind::Task { .. } | SubscriptionKind::Topic { .. } => {}
    }
}

pub(crate) fn subscribe(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    subscription: Subscription,
    connector_topic: bool,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let task = record(domain, number).expect("subscriber live");
    match task.phase {
        Phase::Closing(_) | Phase::Held { was: crate::Was::Closing(_), .. } | Phase::Ended(_) => {
            return refused(to, Some(number), Refusal::State, out);
        }
        Phase::Waiting | Phase::Active(_) | Phase::Held { .. } => {}
    }
    if subscription.number == 0 || find(task, subscription.number).is_some() {
        return refused(to, Some(number), Refusal::Subscription, out);
    }
    if task.subscriptions.len() >= usize::try_from(env.limits.subscriptions).expect("u32 fits usize") {
        return refused(to, Some(number), Refusal::Busy, out);
    }
    match subscription.kind {
        SubscriptionKind::Task { target, held, result } => {
            if !crate::refs::allows(domain, number, target) || (!held && !result) {
                return refused(to, Some(number), Refusal::Reference, out);
            }
        }
        SubscriptionKind::Timer { period, .. } => {
            if period == Some(Duration::ZERO) {
                return refused(to, Some(number), Refusal::Subscription, out);
            }
        }
        SubscriptionKind::Topic { .. } if !connector_topic => {
            return refused(to, Some(number), Refusal::Subscription, out);
        }
        SubscriptionKind::Topic { .. } => {}
    }
    let reserve = usize::try_from(env.limits.result_bytes.max(env.limits.message_bytes)).expect("u32 fits usize");
    if !crate::inbox::room(domain, &env.limits, number, 1, reserve) {
        return refused(to, Some(number), Refusal::Busy, out);
    }
    let row = task_mut(domain, number).expect("subscriber live");
    let mut next = List::with_capacity(env.limits.subscriptions);
    for old in &row.record.subscriptions {
        next.push(*old).expect("bounded subscriptions");
    }
    next.push(subscription).expect("subscription room preflighted");
    row.record.subscriptions = next.into_boxed();
    publish(domain, env, number, out);
    arm(domain, env, subscription);
    match subscription.kind {
        SubscriptionKind::Task { target, held: true, .. } => {
            let watched = record(domain, target).expect("referenced target live");
            match watched.phase {
                Phase::Held { .. } => out.push(Request::Notify {
                    task: number,
                    subscription: subscription.number,
                    target,
                    state: NoticeState::Held,
                    words: Box::new([]),
                }),
                Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => {}
            }
        }
        SubscriptionKind::Task { .. } | SubscriptionKind::Timer { .. } | SubscriptionKind::Topic { .. } => {}
    }
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn unsubscribe(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    subscription: u64,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    let task = record(domain, number).expect("subscriber live");
    if find(task, subscription).is_none() {
        return refused(to, Some(number), Refusal::Subscription, out);
    }
    let row = task_mut(domain, number).expect("subscriber live");
    let mut next = List::with_capacity(env.limits.subscriptions);
    for old in &row.record.subscriptions {
        if old.number != subscription {
            next.push(*old).expect("subscription subset bounded");
        }
    }
    row.record.subscriptions = next.into_boxed();
    let mut inbox = List::with_capacity(env.limits.inbox_messages);
    for word in &row.record.inbox {
        if hint_of(word.kind) != Some(subscription) {
            inbox.push(word.clone()).expect("inbox subset bounded");
        }
    }
    row.record.inbox = inbox.into_boxed();
    domain.timers.cancel(subscription);
    publish(domain, env, number, out);
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn notice(domain: &mut Domain, env: &Env<Limits>, number: u64, word: Word, out: &mut Queue<Request>) {
    let Some(task) = record(domain, number) else { return };
    let Some(subscription_number) = hint_of(word.kind) else { return };
    let Some(subscription) = find(task, subscription_number) else { return };
    let matching = match word.kind {
        MessageKind::Notice { target, .. } => match subscription.kind {
            SubscriptionKind::Task { target: watched, .. } => target == watched,
            SubscriptionKind::Timer { .. } | SubscriptionKind::Topic { .. } => false,
        },
        MessageKind::Timer { .. } => match subscription.kind {
            SubscriptionKind::Timer { .. } => true,
            SubscriptionKind::Task { .. } | SubscriptionKind::Topic { .. } => false,
        },
        MessageKind::News { class, .. } => match subscription.kind {
            SubscriptionKind::Topic { .. } => class != NewsClass::Dropped,
            SubscriptionKind::Task { .. } | SubscriptionKind::Timer { .. } => false,
        },
        MessageKind::Words
        | MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Amendment { .. }
        | MessageKind::Question
        | MessageKind::Answer { .. }
        | MessageKind::Result(_) => false,
    };
    if !matching
        || word.number == 0
        || word.number <= task.last_message
        || word.words.len()
            > usize::try_from(env.limits.result_bytes.max(env.limits.message_bytes)).expect("u32 fits usize")
    {
        return;
    }
    let prepared = crate::inbox::prepare(domain, &env.limits, number, word, crate::inbox::Admission::Merged)
        .expect("subscribed hint has a merged inbox slot");
    let row = task_mut(domain, number).expect("subscriber live");
    row.record.last_message = prepared.word.number;
    row.record.inbox = prepared.inbox;
    publish(domain, env, number, out);
    crate::wake::after_message(domain, env, number, prepared.previous, prepared.word.clone(), out);
    match subscription.kind {
        SubscriptionKind::Timer { period: Some(period), .. } => {
            let at = Wall::from_nanos(env.wall.as_nanos().saturating_add(period.as_nanos()));
            let row = task_mut(domain, number).expect("subscriber live");
            let mut next = List::with_capacity(env.limits.subscriptions);
            for old in &row.record.subscriptions {
                if old.number == subscription_number {
                    next.push(Subscription {
                        number: old.number,
                        kind: SubscriptionKind::Timer { at, period: Some(period) },
                    })
                    .expect("bounded subscriptions");
                } else {
                    next.push(*old).expect("bounded subscriptions");
                }
            }
            row.record.subscriptions = next.into_boxed();
            publish(domain, env, number, out);
            arm(
                domain,
                env,
                Subscription {
                    number: subscription_number,
                    kind: SubscriptionKind::Timer { at, period: Some(period) },
                },
            );
        }
        SubscriptionKind::Timer { period: None, .. } => {
            let row = task_mut(domain, number).expect("subscriber live");
            let mut next = List::with_capacity(env.limits.subscriptions);
            for old in &row.record.subscriptions {
                if old.number != subscription_number {
                    next.push(*old).expect("bounded subscriptions");
                }
            }
            row.record.subscriptions = next.into_boxed();
            publish(domain, env, number, out);
        }
        SubscriptionKind::Task { .. } | SubscriptionKind::Topic { .. } => {}
    }
}

pub(crate) fn notify_state(
    domain: &Domain,
    target: u64,
    state: NoticeState,
    words: &[u8],
    out: &mut Queue<Request>,
) -> bool {
    let mut emitted = false;
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("indexed subscriber");
        for subscription in &task.subscriptions {
            match subscription.kind {
                SubscriptionKind::Task { target: watched, held, result } if watched == target => {
                    let interested = match state {
                        NoticeState::Held => held,
                        NoticeState::Done | NoticeState::Failed | NoticeState::Cancelled => result,
                    };
                    if interested {
                        out.push(Request::Notify {
                            task: *number,
                            subscription: subscription.number,
                            target,
                            state,
                            words: words.into(),
                        });
                        emitted = true;
                    }
                }
                SubscriptionKind::Task { .. } | SubscriptionKind::Timer { .. } | SubscriptionKind::Topic { .. } => {}
            }
        }
    }
    emitted
}

pub(crate) fn timer_due(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some(subscription) = domain.timers.expire(env.now) else { return };
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("indexed task");
        if let Some(found) = find(task, subscription) {
            match found.kind {
                SubscriptionKind::Timer { .. } => out.push(Request::Timer { task: *number, subscription }),
                SubscriptionKind::Task { .. } | SubscriptionKind::Topic { .. } => {}
            }
            return;
        }
    }
}

pub(crate) fn restore(domain: &mut Domain, env: &Env<Limits>) {
    let numbers = crate::domain::snapshot(domain, env.limits.tasks);
    for number in numbers.into_boxed() {
        let task = record(domain, number).expect("restored task live");
        for subscription in task.subscriptions.clone() {
            arm(domain, env, subscription);
        }
    }
}
