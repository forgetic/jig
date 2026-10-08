//! Bounded task messages and atomic turn reads (domain/tasks.md, section 8.2).
use crate::domain::{Domain, publish, record, refused, task_mut};
use crate::{Limits, MessageKind, NewsClass, Party, Phase, QuestionCredit, Refusal, Request, Word};
use alloc::boxed::Box;
use skein_lib::{Env, List, Queue, ReplyTo};

/// The three ways a whole message enters a bounded inbox (domain/tasks.md, 8.2).
#[derive(Clone, Copy)]
pub(crate) enum Admission {
    /// The sender may be refused before its decision is committed.
    Refusable,
    /// Its sender already kept one slot and its maximum bytes.
    RoomKept,
    /// A subscription or amendment replaces its earlier unread hint.
    Merged,
}

pub(crate) struct Prepared {
    pub previous: Option<u64>,
    pub word: Word,
    pub inbox: Box<[Word]>,
}

fn merge_key(kind: MessageKind) -> Option<u64> {
    match kind {
        MessageKind::Notice { subscription, .. }
        | MessageKind::Timer { subscription }
        | MessageKind::News { subscription, .. } => Some(subscription),
        MessageKind::Amendment { .. } => Some(0),
        MessageKind::Words
        | MessageKind::Question
        | MessageKind::Answer { .. }
        | MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Result(_) => None,
    }
}

/// Build one admitted inbox update before its owner commits any state. A
/// reserved or merged message is never refused after its sender committed.
pub(crate) fn prepare(
    domain: &Domain,
    limits: &Limits,
    number: u64,
    mut word: Word,
    admission: Admission,
) -> Option<Prepared> {
    let task = record(domain, number)?;
    if word.number == 0 || word.number <= task.last_message {
        return None;
    }
    match admission {
        Admission::Refusable => {
            if !room(domain, limits, number, 1, word.words.len()) {
                return None;
            }
        }
        Admission::RoomKept | Admission::Merged => {}
    }
    let previous = if task.last_message == 0 { None } else { Some(task.last_message) };
    let key = match admission {
        Admission::Merged => merge_key(word.kind),
        Admission::Refusable | Admission::RoomKept => None,
    };
    let mut inbox = List::with_capacity(limits.inbox_messages.saturating_add(1));
    for old in &task.inbox {
        if key.is_some() && key == merge_key(old.kind) {
            word.at = old.at;
            word.hits = old.hits.saturating_add(1);
            word.eligible = old.eligible;
            // A later quiet hint cannot erase an earlier unread wake.
            match old.kind {
                MessageKind::News { subscription, class: NewsClass::Wakes } => {
                    word.kind = MessageKind::News { subscription, class: NewsClass::Wakes };
                }
                MessageKind::Words
                | MessageKind::Question
                | MessageKind::Answer { .. }
                | MessageKind::Proposal { .. }
                | MessageKind::Escalation { .. }
                | MessageKind::ProposalDecision { .. }
                | MessageKind::Result(_)
                | MessageKind::Notice { .. }
                | MessageKind::Timer { .. }
                | MessageKind::Amendment { .. }
                | MessageKind::News { class: NewsClass::Kept | NewsClass::Dropped, .. } => {}
            }
        } else {
            inbox.push(old.clone()).expect("admitted inbox subset");
        }
    }
    inbox.push(word.clone()).expect("admitted inbox room");
    Some(Prepared { previous, word, inbox: inbox.into_boxed() })
}

/// Account for messages already waiting and the result credit reserved for
/// every live direct delegate (domain/tasks.md, section 8.2).
pub(crate) fn room(domain: &Domain, limits: &Limits, task: u64, count: u32, bytes: usize) -> bool {
    let Some(record) = record(domain, task) else {
        return false;
    };
    let Some(reserved) = record.delegates.len().checked_add(record.questions.len()) else { return false };
    let Some(reserved) = reserved.checked_add(usize::from(record.proposal.is_some())) else { return false };
    let Some((subscription_count, subscription_bytes)) = crate::subscriptions::credit(record, limits) else {
        return false;
    };
    let Some(total_count) = record.inbox.len().checked_add(reserved) else {
        return false;
    };
    let Some(total_count) = total_count.checked_add(subscription_count) else { return false };
    let Some(total_count) = total_count.checked_add(usize::try_from(count).expect("u32 fits usize")) else {
        return false;
    };
    let mut total_bytes = bytes;
    for message in &record.inbox {
        let Some(sum) = total_bytes.checked_add(message.words.len()) else {
            return false;
        };
        total_bytes = sum;
    }
    let Some(result_bytes) =
        record.delegates.len().checked_mul(usize::try_from(limits.result_bytes).expect("u32 fits usize"))
    else {
        return false;
    };
    let Some(answer_bytes) =
        record.questions.len().checked_mul(usize::try_from(limits.message_bytes).expect("u32 fits usize"))
    else {
        return false;
    };
    let Some(total_bytes) = total_bytes.checked_add(result_bytes) else { return false };
    let Some(total_bytes) = total_bytes.checked_add(answer_bytes) else {
        return false;
    };
    let proposal_bytes =
        if record.proposal.is_some() { usize::try_from(limits.message_bytes).expect("u32 fits usize") } else { 0 };
    let Some(total_bytes) = total_bytes.checked_add(proposal_bytes) else { return false };
    let Some(total_bytes) = total_bytes.checked_add(subscription_bytes) else { return false };
    total_count <= usize::try_from(limits.inbox_messages).expect("u32 fits usize")
        && total_bytes <= usize::try_from(limits.inbox_bytes).expect("u32 fits usize")
}

#[expect(clippy::too_many_lines, reason = "one inbox entrance preflights message and answer credits before mutation")]
pub(crate) fn message(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    project: u32,
    number: u64,
    word: Word,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Some(number), Refusal::NotReady, out);
    }
    let Some(task) = record(domain, number) else {
        return refused(to, Some(number), Refusal::Unknown, out);
    };
    let requester = match word.from {
        Party::Person(person) => task.requester == Party::Person(person),
        Party::Task(source) => crate::refs::allows(domain, source, number),
        Party::Deployment { .. } => false,
    };
    let sendable = match word.kind {
        MessageKind::Words | MessageKind::Question | MessageKind::Answer { .. } => true,
        MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. }
        | MessageKind::Amendment { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Result(_)
        | MessageKind::Notice { .. }
        | MessageKind::Timer { .. }
        | MessageKind::News { .. } => false,
    };
    if task.project != project || !requester || !sendable {
        return refused(to, Some(number), Refusal::State, out);
    }
    let available = match task.phase {
        Phase::Active(_) | Phase::Waiting | Phase::Held { was: crate::Was::Waiting | crate::Was::Active(_), .. } => {
            true
        }
        Phase::Closing(_) | Phase::Held { was: crate::Was::Closing(_), .. } | Phase::Ended(_) => false,
    };
    let person_waiting = match word.from {
        Party::Person(_) => match task.phase {
            Phase::Active(_) => false,
            Phase::Waiting | Phase::Closing(_) | Phase::Held { .. } | Phase::Ended(_) => true,
        },
        Party::Task(_) | Party::Deployment { .. } => false,
    };
    if !available || person_waiting {
        return refused(to, Some(number), Refusal::State, out);
    }
    if word.number == 0
        || word.number <= task.last_message
        || word.words.is_empty()
        || word.words.len() > usize::try_from(env.limits.message_bytes).expect("u32 fits usize")
    {
        return refused(to, Some(number), Refusal::Read, out);
    }
    let answer = match word.kind {
        MessageKind::Words | MessageKind::Question => None,
        MessageKind::Answer { question } => {
            let valid = match word.from {
                Party::Task(answerer) => {
                    let mut found = false;
                    for credit in &task.questions {
                        if credit.number == question && answerer == credit.answerer {
                            found = true;
                        }
                    }
                    found
                }
                Party::Person(person) => {
                    let mut found = false;
                    if task.requester == Party::Person(person) {
                        for prior in &task.inbox {
                            if prior.number == question && prior.kind == MessageKind::Question {
                                found = true;
                            }
                            if prior.kind == (MessageKind::Answer { question }) {
                                found = false;
                            }
                        }
                    }
                    found
                }
                Party::Deployment { .. } => false,
            };
            if !valid {
                return refused(to, Some(number), Refusal::Reference, out);
            }
            match word.from {
                Party::Task(_) => Some(question),
                Party::Person(_) | Party::Deployment { .. } => None,
            }
        }
        MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. }
        | MessageKind::Amendment { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Result(_)
        | MessageKind::Notice { .. }
        | MessageKind::Timer { .. }
        | MessageKind::News { .. } => {
            unreachable!("root hints have reserved entrances")
        }
    };
    let admission = match answer {
        Some(_) => Admission::RoomKept,
        None => Admission::Refusable,
    };
    let Some(prepared) = prepare(domain, &env.limits, number, word, admission) else {
        return refused(to, Some(number), Refusal::Busy, out);
    };
    if prepared.word.kind == MessageKind::Question {
        let source = match prepared.word.from {
            Party::Task(source) => source,
            Party::Person(_) | Party::Deployment { .. } => return refused(to, Some(number), Refusal::Reference, out),
        };
        if !room(domain, &env.limits, source, 1, usize::try_from(env.limits.message_bytes).expect("u32 fits usize")) {
            return refused(to, Some(source), Refusal::Busy, out);
        }
    }
    let task = task_mut(domain, number).expect("admitted task still live");
    task.record.last_message = prepared.word.number;
    task.record.inbox = prepared.inbox;
    if let Some(question) = answer {
        let mut credits = List::with_capacity(env.limits.inbox_messages);
        for credit in &task.record.questions {
            if credit.number != question {
                credits.push(*credit).expect("credit subset bounded");
            }
        }
        task.record.questions = credits.into_boxed();
    }
    publish(domain, env, number, out);
    if prepared.word.kind == MessageKind::Question {
        let source = match prepared.word.from {
            Party::Task(source) => source,
            Party::Person(_) | Party::Deployment { .. } => unreachable!("question source checked"),
        };
        let source_row = task_mut(domain, source).expect("referenced questioner remains live");
        let mut credits = List::with_capacity(env.limits.inbox_messages);
        for credit in &source_row.record.questions {
            credits.push(*credit).expect("question credits bounded");
        }
        credits.push(QuestionCredit { number: prepared.word.number, answerer: number }).expect("answer room reserved");
        source_row.record.questions = credits.into_boxed();
        publish(domain, env, source, out);
    }
    crate::wake::after_message(domain, env, number, prepared.previous, prepared.word.clone(), out);
    out.push(Request::Sent { reply_to: to, task: number, word: prepared.word });
}

/// Root's same-decision result handoff after a delegate settled. Its reserved
/// inbox credit cannot be consumed by ordinary words.
pub(crate) fn delegate_result(
    domain: &mut Domain,
    env: &Env<Limits>,
    number: u64,
    word: Word,
    out: &mut Queue<Request>,
) {
    let Some(task) = record(domain, number) else {
        return;
    };
    match word.kind {
        MessageKind::Result(_) => {}
        MessageKind::Words
        | MessageKind::Proposal { .. }
        | MessageKind::Escalation { .. }
        | MessageKind::ProposalDecision { .. }
        | MessageKind::Amendment { .. }
        | MessageKind::Question
        | MessageKind::Answer { .. }
        | MessageKind::Notice { .. }
        | MessageKind::Timer { .. }
        | MessageKind::News { .. } => return,
    }
    if !match word.from {
        Party::Task(_) => true,
        Party::Person(_) | Party::Deployment { .. } => false,
    } || word.number == 0
        || word.number <= task.last_message
        || word.words.len() > usize::try_from(env.limits.result_bytes).expect("u32 fits usize")
        || !room(domain, &env.limits, number, 1, word.words.len())
    {
        return;
    }
    let prepared = prepare(domain, &env.limits, number, word, Admission::RoomKept)
        .expect("committed delegate result keeps a slot");
    let task = task_mut(domain, number).expect("requester remains live");
    task.record.last_message = prepared.word.number;
    task.record.inbox = prepared.inbox;
    publish(domain, env, number, out);
    crate::wake::after_message(domain, env, number, prepared.previous, prepared.word, out);
}

/// Root-authenticated proposal decision consumes the pending proposal's
/// reserved inbox slot and wakes or relays the proposer after commitment.
pub(crate) fn proposal_decision(
    domain: &mut Domain,
    env: &Env<Limits>,
    number: u64,
    word: Word,
    out: &mut Queue<Request>,
) {
    let old = record(domain, number).expect("proposal's live proposer");
    assert!(old.proposal.is_none() && word.number > old.last_message, "decision follows proposal and is fresh");
    assert!(room(domain, &env.limits, number, 1, word.words.len()), "reserved proposal slot remains");
    let prepared = prepare(domain, &env.limits, number, word, Admission::RoomKept)
        .expect("committed proposal decision keeps a slot");
    let task = task_mut(domain, number).expect("proposal's live proposer");
    task.record.last_message = prepared.word.number;
    task.record.inbox = prepared.inbox;
    publish(domain, env, number, out);
    crate::wake::after_message(domain, env, number, prepared.previous, prepared.word, out);
}

pub(crate) fn readable(domain: &Domain, task: u64, read: Option<u64>) -> bool {
    match read {
        None => true,
        Some(number) => {
            let task_record = record(domain, task).expect("readable task live");
            for word in &task_record.inbox {
                if word.number == number {
                    return true;
                }
            }
            for word in crate::proposals::waiting_for(domain, task) {
                if word.number == number {
                    return true;
                }
            }
            for word in crate::escalation::waiting_for(domain, task) {
                if word.number == number {
                    return true;
                }
            }
            false
        }
    }
}

pub(crate) fn take(task: &mut crate::TaskRecord, read: Option<u64>) {
    if let Some(number) = read {
        let mut kept = List::with_capacity(u32::try_from(task.inbox.len()).expect("bounded inbox count"));
        for word in &task.inbox {
            if word.number > number {
                kept.push(word.clone()).expect("retained subset fits");
            }
        }
        task.inbox = kept.into_boxed();
    }
}
