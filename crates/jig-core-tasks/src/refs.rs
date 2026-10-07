//! Live reciprocal introductions and message visibility (domain/tasks.md, section 7.5).
//! The task row keeps introduced peers; delegation links are implicit. The root
//! supplies a current run identity, and this child checks the live graph.
use crate::domain::{Domain, entrance, publish, record, refused, task_mut};
use crate::{Limits, Party, Refusal, Request};
use skein_lib::{Env, List, Queue, ReplyTo};

pub(crate) fn allows(domain: &Domain, source: u64, target: u64) -> bool {
    let Some(task) = record(domain, source) else { return false };
    task.requester == Party::Task(target)
        || crate::batch::contains(&task.delegates, target)
        || crate::batch::contains(&task.references, target)
}

pub(crate) fn introduce(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    by: u64,
    left: u64,
    right: u64,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, by) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(by), why, out),
    };
    let Some(left_row) = record(domain, left) else {
        return refused(to, Some(left), Refusal::Unknown, out);
    };
    let Some(right_row) = record(domain, right) else {
        return refused(to, Some(right), Refusal::Unknown, out);
    };
    if left == right || left_row.project != right_row.project || !allows(domain, by, left) || !allows(domain, by, right)
    {
        return refused(to, None, Refusal::Reference, out);
    }
    for (task, peer) in [(left, right), (right, left)] {
        let row = record(domain, task).expect("preflighted live peer");
        if !crate::batch::contains(&row.references, peer)
            && row.references.len() >= usize::try_from(env.limits.references).expect("u32 fits usize")
        {
            return refused(to, Some(task), Refusal::Busy, out);
        }
    }
    for (task, peer) in [(left, right), (right, left)] {
        let row = task_mut(domain, task).expect("preflighted live peer");
        let mut references = List::with_capacity(env.limits.references);
        for number in &row.record.references {
            references.push(*number).expect("references bounded");
        }
        if !crate::batch::contains(references.as_slice(), peer) {
            references.push(peer).expect("reference room preflighted");
        }
        row.record.references = references.into_boxed();
        publish(domain, env, task, out);
    }
    out.push(Request::Done { reply_to: to });
}

pub(crate) fn end(domain: &mut Domain, env: &Env<Limits>, ended: u64, out: &mut Queue<Request>) {
    let mut peers = List::with_capacity(env.limits.tasks);
    for (number, _) in &domain.names {
        if *number != ended {
            let row = record(domain, *number).expect("indexed task");
            let mut owed = false;
            for credit in &row.questions {
                if credit.answerer == ended {
                    owed = true;
                }
            }
            if crate::batch::contains(&row.references, ended) || owed {
                peers.push(*number).expect("live peer count bounded");
            }
        }
    }
    for number in peers.into_boxed() {
        let row = task_mut(domain, number).expect("live peer");
        let mut references = List::with_capacity(env.limits.references);
        for peer in &row.record.references {
            if *peer != ended {
                references.push(*peer).expect("reference subset bounded");
            }
        }
        row.record.references = references.into_boxed();
        let mut credits = List::with_capacity(env.limits.inbox_messages);
        for credit in &row.record.questions {
            if credit.answerer != ended {
                credits.push(*credit).expect("credit subset bounded");
            }
        }
        row.record.questions = credits.into_boxed();
        publish(domain, env, number, out);
    }
}
