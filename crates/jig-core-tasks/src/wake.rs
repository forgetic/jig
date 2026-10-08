//! Closed wake rules and bounded batch timers (domain/tasks.md, section 8.3).
//! Policies and unread messages are durable in task rows. Monotonic deadlines
//! are derived from injected wall and monotonic time after restore. Procedures
//! batch news by their policy and step immediately on other messages.
use crate::domain::{Domain, activate, publish, record, task_mut};
use crate::{Active, Limits, MessageKind, NewsClass, Party, Phase, Request, ResultsWake, WakePolicy, WakeRule, Word};
use skein_lib::{Duration, Env, Queue, Wall};

pub(crate) fn valid(policy: &WakePolicy) -> bool {
    for rule in [policy.words, policy.notices, policy.news] {
        match rule {
            WakeRule::Never | WakeRule::Immediate => {}
            WakeRule::Batch { count, age } => {
                if count == 0 || age == Duration::ZERO {
                    return false;
                }
            }
        }
    }
    true
}

fn category(kind: MessageKind) -> u8 {
    match kind {
        MessageKind::Words => 0,
        MessageKind::Amendment { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. } => 7,
        MessageKind::Question => 1,
        MessageKind::Answer { .. } => 2,
        MessageKind::Result(_) => 3,
        MessageKind::Notice { .. } => 4,
        MessageKind::Timer { .. } => 5,
        MessageKind::News { .. } => 6,
    }
}

fn rule(domain: &Domain, number: u64, word: &Word) -> WakeRule {
    let task = record(domain, number).expect("inbox task live");
    match word.kind {
        MessageKind::Words => match word.from {
            Party::Person(_) => WakeRule::Immediate,
            Party::Task(_) | Party::Deployment { .. } => task.wake.words,
        },
        MessageKind::Amendment { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. } => WakeRule::Immediate,
        MessageKind::Question => {
            if task.wake.questions {
                WakeRule::Immediate
            } else {
                WakeRule::Never
            }
        }
        MessageKind::Answer { .. } => {
            if task.wake.answers {
                WakeRule::Immediate
            } else {
                WakeRule::Never
            }
        }
        MessageKind::Result(result) => match task.wake.results {
            ResultsWake::Never => WakeRule::Never,
            ResultsWake::Each => WakeRule::Immediate,
            ResultsWake::LastOrFailure => match result {
                crate::ResultKind::Failed | crate::ResultKind::Cancelled => WakeRule::Immediate,
                crate::ResultKind::Report | crate::ResultKind::Verdict { .. } | crate::ResultKind::Change { .. } => {
                    if task.delegates.is_empty() {
                        WakeRule::Immediate
                    } else {
                        WakeRule::Never
                    }
                }
            },
        },
        MessageKind::Notice { .. } => task.wake.notices,
        MessageKind::Timer { .. } => {
            if task.wake.timers {
                WakeRule::Immediate
            } else {
                WakeRule::Never
            }
        }
        MessageKind::News { class, .. } => match class {
            NewsClass::Wakes => task.wake.news,
            NewsClass::Kept | NewsClass::Dropped => WakeRule::Never,
        },
    }
}

fn batch_state(domain: &Domain, number: u64, word: &Word) -> (u32, Wall, bool) {
    let task = record(domain, number).expect("inbox task live");
    let mut hits = 0_u32;
    let mut oldest = word.at;
    let mut latched = false;
    for item in &task.inbox {
        if category(item.kind) == category(word.kind) {
            hits = hits.saturating_add(item.hits);
            oldest = oldest.min(item.at);
            latched |= item.eligible;
        }
    }
    (hits, oldest, latched)
}

pub(crate) fn after_message(
    domain: &mut Domain,
    env: &Env<Limits>,
    number: u64,
    previous: Option<u64>,
    word: Word,
    out: &mut Queue<Request>,
) {
    let decision = match record(domain, number).expect("message task live").executor {
        crate::Executor::Procedure { .. } => match word.kind {
            MessageKind::News { .. } => rule(domain, number, &word),
            MessageKind::Words
            | MessageKind::Amendment { .. }
            | MessageKind::ProposalDecision { .. }
            | MessageKind::Proposal { .. }
            | MessageKind::Escalation { .. }
            | MessageKind::Question
            | MessageKind::Answer { .. }
            | MessageKind::Result(_)
            | MessageKind::Notice { .. }
            | MessageKind::Timer { .. } => WakeRule::Immediate,
        },
        crate::Executor::Agent { .. } => rule(domain, number, &word),
        crate::Executor::Person(_) => WakeRule::Never,
    };
    let ready = match decision {
        WakeRule::Never => false,
        WakeRule::Immediate => true,
        WakeRule::Batch { count, age } => {
            let (hits, oldest, latched) = batch_state(domain, number, &word);
            let at = oldest.as_nanos().saturating_add(age.as_nanos());
            if latched || hits >= count || env.wall.as_nanos() >= at {
                let task = task_mut(domain, number).expect("batch task live");
                for item in &mut task.record.inbox {
                    if category(item.kind) == category(word.kind) {
                        item.eligible = true;
                    }
                }
                publish(domain, env, number, out);
                true
            } else {
                false
            }
        }
    };
    schedule(domain, env, number);
    if !ready {
        return;
    }
    let phase = record(domain, number).expect("batch task live").phase.clone();
    match phase {
        Phase::Active(Active::Idle) => {
            task_mut(domain, number).expect("idle task live").record.phase = Phase::Active(Active::Due);
            publish(domain, env, number, out);
            activate(domain, number, out);
        }
        Phase::Active(Active::Claimed { attempt } | Active::Running { attempt }) => {
            out.push(Request::Relay { task: number, attempt, previous, word });
        }
        Phase::Waiting
        | Phase::Active(Active::Due | Active::Preparing | Active::BackingOff { .. })
        | Phase::Closing(_)
        | Phase::Held { .. }
        | Phase::Ended(_) => {}
    }
}

/// News and results arriving after the offered prefix keep their own wake policy.
pub(crate) fn after_step(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let inbox = record(domain, number).expect("stepped procedure live").inbox.clone();
    schedule(domain, env, number);
    for word in inbox {
        // One activation offers the whole remaining inbox. Stop once it is
        // due so the route's output bound does not grow with its message count.
        if record(domain, number).expect("stepped procedure live").phase != Phase::Active(Active::Idle) {
            return;
        }
        after_message(domain, env, number, None, word, out);
    }
}

pub(crate) fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some(number) = domain.wakes.expire(env.now) else { return };
    let Some(task) = record(domain, number) else { return };
    let mut candidate = None;
    for word in &task.inbox {
        match rule(domain, number, word) {
            WakeRule::Batch { age, .. } => {
                let at = word.at.as_nanos().saturating_add(age.as_nanos());
                if !word.eligible && env.wall.as_nanos() >= at {
                    candidate = Some(word.clone());
                    break;
                }
            }
            WakeRule::Never | WakeRule::Immediate => {}
        }
    }
    if let Some(word) = candidate {
        after_message(domain, env, number, None, word, out);
    } else {
        schedule(domain, env, number);
    }
}

fn schedule(domain: &mut Domain, env: &Env<Limits>, number: u64) {
    let task = record(domain, number).expect("batch task live");
    let mut oldest: Option<u64> = None;
    for word in &task.inbox {
        match rule(domain, number, word) {
            WakeRule::Batch { age, .. } if !word.eligible => {
                let at = word.at.as_nanos().saturating_add(age.as_nanos());
                oldest = Some(match oldest {
                    Some(previous) => previous.min(at),
                    None => at,
                });
            }
            WakeRule::Never | WakeRule::Immediate | WakeRule::Batch { .. } => {}
        }
    }
    domain.wakes.cancel(number);
    if let Some(at) = oldest {
        let due = env.now.saturating_add(Duration::from_nanos(at.saturating_sub(env.wall.as_nanos())));
        assert!(domain.wakes.arm(number, due).is_ok(), "batch alarm room");
    }
}

pub(crate) fn restore(domain: &mut Domain, env: &Env<Limits>) {
    let numbers = crate::domain::snapshot(domain, env.limits.tasks);
    for number in numbers.into_boxed() {
        schedule(domain, env, number);
    }
}
