//! Root-authorized changes to live tasks (domain/tasks.md, sections 5.5 and 7).
//! The child keeps only current state; root owns durable call decisions and
//! commits every emitted save before any worker sees a control outcome.
use crate::domain::{Domain, entrance, publish, record, refused, task_mut};
use crate::{
    Active, Authority, Limits, MessageKind, Party, Phase, Refusal, Request, Spec, Stored, Tries, WakePolicy, Was, Word,
};
use alloc::boxed::Box;
use skein_lib::{Env, Queue, ReplyTo};

/// One tool change to a live delegate, authenticated by root's current claim.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Control {
    /// Close a whole subtree with a common bounded reason.
    Cancel { reason: Box<[u8]> },
    /// Lift a hold and reset the failure counts it exhausted.
    Release,
}

/// Whole optional task changes; root checks any widening against its holder.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Amendment {
    pub spec: Option<Spec>,
    pub wake: Option<WakePolicy>,
    pub dependencies: Option<Box<[u64]>>,
    pub authority: Option<Authority>,
    pub reason: Box<[u8]>,
}

/// A committed plan revision, kept outside the live arena for later reads.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct History {
    pub task: u64,
    pub revision: u64,
    pub by: Party,
    pub change: Change,
    pub reason: Box<[u8]>,
    pub proposal: Option<Box<crate::Proposal>>,
}

/// One kind of durable task-tree change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Change {
    Cancelled,
    Released,
    Amended,
    Prioritised { priority: u32 },
    Moved,
    Proposed,
    ProposalPassed,
    ProposalAccepted,
    ProposalRejected,
    ProposalWithdrawn,
}

pub(crate) fn history(
    domain: &mut Domain,
    number: u64,
    by: Party,
    change: Change,
    reason: &[u8],
    out: &mut Queue<Request>,
) {
    let task = task_mut(domain, number).expect("history task live");
    task.record.revision = task.record.revision.checked_add(1).expect("revision preflighted");
    out.push(Request::Save {
        record: Stored::History(History {
            task: number,
            revision: task.record.revision,
            by,
            change,
            reason: reason.into(),
            proposal: None,
        }),
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "one root-fenced amendment carries task, caller, message and stop decision"
)]
#[expect(clippy::too_many_lines, reason = "whole amendment preflight and mutation remain together")]
pub(crate) fn amend(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    by: Party,
    number: u64,
    message: u64,
    stop_run: bool,
    amendment: Amendment,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    if !allowed(domain, number, by, env.limits.tasks) {
        return refused(to, Some(number), Refusal::Reference, out);
    }
    let old = record(domain, number).expect("entrance found task");
    let mutable = match old.phase {
        Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => true,
        Phase::Closing(_) | Phase::Held { was: Was::Closing(_), .. } | Phase::Ended(_) => false,
    };
    if !mutable {
        return refused(to, Some(number), Refusal::State, out);
    }
    if old.revision == u64::MAX || message == 0 || message <= old.last_message {
        return refused(to, Some(number), Refusal::Read, out);
    }
    if amendment.reason.len() > usize::try_from(env.limits.message_bytes).expect("u32 fits usize") {
        return refused(to, Some(number), Refusal::Read, out);
    }
    if let Some(spec) = &amendment.spec
        && (!crate::batch::valid_spec(&env.limits, spec) || !spec.inputs.is_empty())
    {
        return refused(to, Some(number), Refusal::Spec, out);
    }
    if let Some(wake) = &amendment.wake
        && !crate::wake::valid(wake)
    {
        return refused(to, Some(number), Refusal::Read, out);
    }
    if let Some(dependencies) = &amendment.dependencies {
        let waiting = match old.phase {
            Phase::Waiting | Phase::Held { was: Was::Waiting, .. } => true,
            Phase::Active(_)
            | Phase::Closing(_)
            | Phase::Held { was: Was::Active(_) | Was::Closing(_), .. }
            | Phase::Ended(_) => false,
        };
        if !waiting || dependencies.len() > usize::try_from(env.limits.dependencies).expect("u32 fits usize") {
            return refused(to, Some(number), Refusal::Dependencies, out);
        }
        for (at, dependency) in dependencies.iter().enumerate() {
            if !crate::batch::contains(&old.dependencies, *dependency) {
                return refused(to, Some(number), Refusal::Dependencies, out);
            }
            for earlier in dependencies.iter().take(at) {
                if earlier == dependency {
                    return refused(to, Some(number), Refusal::Dependencies, out);
                }
            }
        }
    }
    if let Some(authority) = &amendment.authority {
        if !crate::batch::valid_authority(&env.limits, authority) {
            return refused(to, Some(number), Refusal::AuthorityShape, out);
        }
        if !crate::funders::can_resize(domain, number, authority.budget.spend) {
            return refused(to, Some(number), Refusal::Funding, out);
        }
    } else if stop_run {
        return refused(to, Some(number), Refusal::State, out);
    }
    let attempt = if stop_run { crate::run::run_attempt(&old.phase) } else { None };
    let next_revision = old.revision.checked_add(1).expect("revision preflighted");
    let word = Word {
        number: message,
        from: by,
        kind: MessageKind::Amendment { revision: next_revision },
        words: amendment.reason.clone(),
        at: env.wall,
        hits: 1,
        eligible: true,
    };
    let prepared = crate::inbox::prepare(domain, &env.limits, number, word, crate::inbox::Admission::Merged)
        .expect("authenticated amendment has a merged inbox slot");
    if let Some(authority) = &amendment.authority
        && authority.budget.spend != old.numbers.budget
    {
        crate::funders::resize(domain, env, number, authority.budget.spend, out);
    }
    let task = task_mut(domain, number).expect("amendment task live");
    if let Some(spec) = amendment.spec {
        task.record.spec = spec;
    }
    if let Some(wake) = amendment.wake {
        task.record.wake = wake;
    }
    if let Some(dependencies) = amendment.dependencies {
        let mut waiting = skein_lib::List::with_capacity(env.limits.dependencies);
        for dependency in &task.record.waiting_on {
            if crate::batch::contains(&dependencies, *dependency) {
                waiting.push(*dependency).expect("dependency subset bound");
            }
        }
        task.record.waiting_on = waiting.into_boxed();
        task.record.dependencies = dependencies;
    }
    if let Some(authority) = amendment.authority {
        task.record.authority = authority;
    }
    task.record.last_message = message;
    task.record.inbox = prepared.inbox;
    if attempt.is_some() {
        task.record.narrowing = true;
    }
    history(domain, number, by, Change::Amended, &amendment.reason, out);
    publish(domain, env, number, out);
    if let Some(attempt) = attempt {
        out.push(Request::Stop { task: number, attempt });
    } else {
        crate::wake::after_message(domain, env, number, prepared.previous, prepared.word, out);
    }
    out.push(Request::Done { reply_to: to });
}

/// Apply one bounded whole-project priority edit after the root checked the person's role.
pub(crate) fn prioritise(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    project: u32,
    by: Party,
    goals: &[(u64, u32)],
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, None, Refusal::NotReady, out);
    }
    let person = match by {
        Party::Person(person) if person != 0 => person,
        Party::Person(_) | Party::Task(_) | Party::Deployment { .. } => {
            return refused(to, None, Refusal::Reference, out);
        }
    };
    if project == 0 || goals.is_empty() || goals.len() > usize::try_from(env.limits.tasks).expect("u32 fits usize") {
        return refused(to, None, Refusal::Read, out);
    }
    for (at, (number, _)) in goals.iter().enumerate() {
        let Some(task) = record(domain, *number) else { return refused(to, Some(*number), Refusal::Unknown, out) };
        let mutable = match task.phase {
            Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => true,
            Phase::Closing(_) | Phase::Held { was: Was::Closing(_), .. } | Phase::Ended(_) => false,
        };
        if task.project != project || task.tracked.is_none() || task.revision == u64::MAX || !mutable {
            return refused(to, Some(*number), Refusal::State, out);
        }
        for (earlier, _) in goals.iter().take(at) {
            if earlier == number {
                return refused(to, Some(*number), Refusal::Read, out);
            }
        }
    }
    for (number, priority) in goals {
        let task = task_mut(domain, *number).expect("priority preflight found task");
        task.record.tracked = Some(*priority);
        history(domain, *number, Party::Person(person), Change::Prioritised { priority: *priority }, &[], out);
        publish(domain, env, *number, out);
    }
    out.push(Request::Done { reply_to: to });
}

fn below(domain: &Domain, number: u64, ancestor: u64, bound: u32) -> bool {
    let mut at = Some(number);
    for _ in 0..bound {
        let Some(number) = at else { return false };
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

fn allowed(domain: &Domain, number: u64, by: Party, bound: u32) -> bool {
    match by {
        Party::Task(task) => task != number && below(domain, number, task, bound),
        Party::Person(person) => person != 0,
        Party::Deployment { .. } => false,
    }
}

pub(crate) fn apply(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    by: Party,
    number: u64,
    control: Control,
    out: &mut Queue<Request>,
) {
    let to = match entrance(domain, to, number) {
        Ok(to) => to,
        Err((to, why)) => return refused(to, Some(number), why, out),
    };
    if !allowed(domain, number, by, env.limits.tasks) {
        return refused(to, Some(number), Refusal::Reference, out);
    }
    match control {
        Control::Cancel { reason } => {
            if reason.len() > usize::try_from(env.limits.result_bytes).expect("u32 fits usize") {
                return refused(to, Some(number), Refusal::Read, out);
            }
            for (child, _) in &domain.names {
                if below(domain, *child, number, env.limits.tasks)
                    && record(domain, *child).expect("live child").revision == u64::MAX
                {
                    return refused(to, Some(*child), Refusal::State, out);
                }
            }
            let mut selected = skein_lib::List::with_capacity(env.limits.tasks);
            for (child, _) in &domain.names {
                if below(domain, *child, number, env.limits.tasks) {
                    selected.push(*child).expect("live child bound");
                }
            }
            for child in selected.into_boxed() {
                history(domain, child, by, Change::Cancelled, &reason, out);
            }
            crate::closing::cancel_tree(domain, env, number, &reason, out);
            out.push(Request::Done { reply_to: to });
        }
        Control::Release => {
            let task = record(domain, number).expect("entrance found live task");
            if task.revision == u64::MAX {
                return refused(to, Some(number), Refusal::State, out);
            }
            if crate::run::run_attempt(&task.phase).is_some() {
                return refused(to, Some(number), Refusal::State, out);
            }
            let next = match &task.phase {
                Phase::Held { was: Was::Waiting, .. } => Phase::Waiting,
                Phase::Held {
                    was: Was::Active(Active::Idle | Active::Due | Active::Preparing | Active::BackingOff { .. }),
                    ..
                } => Phase::Active(Active::Due),
                Phase::Held { was: Was::Closing(closing), .. } => Phase::Closing(closing.clone()),
                Phase::Held { was: Was::Active(Active::Claimed { .. } | Active::Running { .. }), .. } => {
                    unreachable!("live run refused above")
                }
                Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => {
                    return refused(to, Some(number), Refusal::State, out);
                }
            };
            let task = task_mut(domain, number).expect("entrance found live task");
            task.record.phase = next;
            if task.record.phase == Phase::Waiting {
                task.record.hold_wait_since = None;
            }
            task.record.tries = Tries::NONE;
            task.record.refusals = 0;
            history(domain, number, by, Change::Released, &[], out);
            publish(domain, env, number, out);
            if record(domain, number).expect("released task live").phase == Phase::Active(Active::Due) {
                crate::domain::activate(domain, number, out);
            }
            if let Phase::Closing(closing) = &record(domain, number).expect("released task live").phase {
                match closing.stage {
                    crate::Stage::Effects => out.push(Request::Close { task: number, ending: closing.ending.clone() }),
                    crate::Stage::Releases => {
                        out.push(Request::Release { task: number, ending: closing.ending.clone() });
                    }
                    crate::Stage::Run { .. } | crate::Stage::Delegates | crate::Stage::Settled => {}
                }
            }
            out.push(Request::Done { reply_to: to });
        }
    }
}
