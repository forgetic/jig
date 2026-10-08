//! Held person-chat decisions, independent of root authentication, policy and
//! transport history (domain/tasks.md, section 9).

use crate::domain::{Domain, activate, publish, record, task_mut};
use crate::{Active, Hold, Limits, Party, Phase, Request, Tries, Was};
use alloc::boxed::Box;
use skein_lib::{Env, Queue, ReplyTo, Time, Wall};

pub(crate) fn schedule(domain: &mut Domain, env: &Env<Limits>, number: u64) {
    let Some(task) = record(domain, number) else { return };
    match task.escalation {
        Escalation::Waiting { holder: EscalationHolder::Task(_) | EscalationHolder::Person(_), since, .. } => {
            let until = since.as_nanos().saturating_add(env.limits.escalation_stall.as_nanos());
            let remaining = until.saturating_sub(env.wall.as_nanos());
            let due = Time::from_nanos(env.now.as_nanos().saturating_add(remaining));
            assert!(domain.escalation_alarms.arm(number, due).is_ok(), "one held escalation per task");
        }
        Escalation::Waiting { holder: EscalationHolder::Role { .. }, .. }
        | Escalation::Unheld { .. }
        | Escalation::Routing { .. }
        | Escalation::Rejected { .. } => domain.escalation_alarms.cancel(number),
    }
}

pub(crate) fn rearm_all(domain: &mut Domain, env: &Env<Limits>) {
    let mut held = skein_lib::List::with_capacity(env.limits.tasks);
    for (number, _) in &domain.names {
        held.push(*number).expect("live escalation names bounded");
    }
    for &number in &held {
        schedule(domain, env, number);
    }
}

pub(crate) fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some(number) = domain.escalation_alarms.expire(env.now) else { return };
    let Some(task) = record(domain, number) else { return };
    match task.escalation {
        Escalation::Waiting { revision, holder, .. } => match holder {
            EscalationHolder::Task(_) | EscalationHolder::Person(_) => {
                out.push(Request::EscalationStalled { task: number, revision, holder });
            }
            EscalationHolder::Role { .. } => {}
        },
        Escalation::Unheld { .. } | Escalation::Routing { .. } | Escalation::Rejected { .. } => {}
    }
}

fn word(task: u64, revision: u64, entry: u64, since: Wall) -> crate::Word {
    crate::Word {
        number: entry,
        from: Party::Task(task),
        kind: crate::MessageKind::Escalation { task, revision },
        words: Box::from(&b"Held task needs a decision"[..]),
        at: since,
        hits: 1,
        eligible: true,
    }
}

pub(crate) fn waiting_for(domain: &Domain, holder: u64) -> Box<[crate::Word]> {
    let mut entries = skein_lib::List::with_capacity(domain.names.len());
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("held name live");
        match task.escalation {
            Escalation::Waiting { revision, holder: EscalationHolder::Task(target), entry, since }
                if target == holder =>
            {
                entries.push(word(*number, revision, entry, since)).expect("one escalation per held task");
            }
            Escalation::Waiting { .. }
            | Escalation::Unheld { .. }
            | Escalation::Routing { .. }
            | Escalation::Rejected { .. } => {}
        }
    }
    entries.into_boxed()
}

fn wake_holder(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let Some(task) = record(domain, number) else { return };
    let (revision, holder, entry, since) = match task.escalation {
        Escalation::Waiting { revision, holder: EscalationHolder::Task(holder), entry, since } => {
            (revision, holder, entry, since)
        }
        Escalation::Waiting { holder: EscalationHolder::Person(_) | EscalationHolder::Role { .. }, .. }
        | Escalation::Unheld { .. }
        | Escalation::Routing { .. }
        | Escalation::Rejected { .. } => return,
    };
    if record(domain, holder).is_none() {
        out.push(Request::EscalationStalled { task: number, revision, holder: EscalationHolder::Task(holder) });
    } else {
        crate::wake::after_message(domain, env, holder, None, word(number, revision, entry, since), out);
    }
}

pub(crate) fn holder_unavailable(domain: &Domain, holder: u64, out: &mut Queue<Request>) {
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("held name live");
        match task.escalation {
            Escalation::Waiting { revision, holder: EscalationHolder::Task(target), .. } if target == holder => {
                out.push(Request::EscalationStalled {
                    task: *number,
                    revision,
                    holder: EscalationHolder::Task(holder),
                });
            }
            Escalation::Waiting { .. }
            | Escalation::Unheld { .. }
            | Escalation::Routing { .. }
            | Escalation::Rejected { .. } => {}
        }
    }
}

pub(crate) fn wake_restored(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let mut held = skein_lib::List::with_capacity(domain.names.len());
    for (number, _) in &domain.names {
        held.push(*number).expect("live names bounded");
    }
    for &number in &held {
        wake_holder(domain, env, number, out);
    }
}

/// Semantic recipient of one held chat decision. Root verifies current membership
/// and coverage; tasks keeps only this bounded identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EscalationHolder {
    /// Nearest live ancestor task with authority to decide this hold.
    Task(u64),
    /// The chat's person requester, eligible under current policy.
    Person(
        /// Positive requester identity, supplied by root.
        u64,
    ),
    /// Final project policy role; any authenticated current holder may decide
    /// first, and passing further refuses.
    Role {
        /// Project of the held chat, checked by tasks.
        project: u32,
        /// Policy role selected and checked by root, bounded by authority roles.
        role: u32,
    },
}

/// One semantic state per task; no receipt table, inbox or historical archive.
/// Root archives decisions; rejected chats remain held.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Escalation {
    /// No current held decision; retain the last revision to prevent reuse
    Unheld {
        /// Zero initially; checked revision advances on each hold/pass
        revision: u64,
    },
    /// Root owes an eligible requester or final policy role in this same atomic
    /// decision; not a durable unresolved route.
    Routing {
        /// Positive per-task semantic revision.
        revision: u64,
    },
    /// One durable pending decision; no task activation can escape the hold
    Waiting {
        /// Positive revision echoed by reads and decisions.
        revision: u64,
        /// Root-resolved current recipient; membership stays root's concern
        holder: EscalationHolder,
        /// Root-numbered virtual decision entry delivered to the holder.
        entry: u64,
        /// Wall-clock start of this holder's bounded decision wait.
        since: Wall,
    },
    /// One completed rejection; retain reason while held without reopening or
    /// rerouting. Historical transport outcome stays root-owned.
    Rejected {
        revision: u64,
        by: u64,
        /// Rejection words, at most task `result_bytes`.
        reason: Box<[u8]>,
    },
}

impl Escalation {
    /// Pure projection of this task's checked semantic revision; no allocation,
    /// mutation or root-issued identity is involved.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        match self {
            Escalation::Unheld { revision }
            | Escalation::Routing { revision }
            | Escalation::Waiting { revision, .. }
            | Escalation::Rejected { revision, .. } => *revision,
        }
    }
}

/// Root-authorized semantic choice for the exact waiting revision. No authority
/// widening or generic release surface is implied.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum EscalationDecision {
    /// Lift any hold, reset tries, and rejudge readiness.
    Release,
    /// Decide once while leaving the task held.
    Reject {
        /// At most `result_bytes`, checked before mutation.
        reason: Box<[u8]>,
    },
    /// Persist the root-verified final policy role and advance revision; a role
    /// holder cannot pass further.
    Pass {
        /// Final policy role, checked by root and structurally by tasks
        holder: EscalationHolder,
    },
}

/// Semantic terminal for one root decision call. Root commits accepted state
/// and its own archive with people's keyed answer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EscalationOutcome {
    /// Hold lifted, with normal activation consequences.
    Released,
    /// Reason retained; no activation or automatic reroute.
    Rejected,
    /// Waiting moved to the final policy role.
    Passed {
        /// New checked pending revision.
        revision: u64,
    },
    /// No exact live pending revision; root may read its own historical outcome
    Stale,
    /// A policy-role decision cannot pass further.
    NoFurther,
    /// Input/revision capacity failed before mutation.
    Limit,
}

/// Task-to-root held view; not a second task/funding ledger. Root
/// authenticates reads and decisions against this exact context.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EscalationContext {
    /// Positive held task identity.
    pub task: u64,
    /// Policy project checked by root.
    pub project: u32,
    pub requester: u64,
    /// Immediate requester, used to seek the nearest covering ancestor.
    pub immediate: Party,
    /// Preserved semantic hold reason.
    pub why: Hold,
    /// Exactly one bounded current revision/status; rejection owns at most
    /// `result_bytes`.
    pub escalation: Escalation,
}

pub(crate) fn context(domain: &Domain, number: u64) -> Option<Box<EscalationContext>> {
    let task = record(domain, number)?;
    let immediate = task.requester;
    let mut above = immediate;
    let mut requester = None;
    for _ in 0..=task.depth {
        match above {
            Party::Person(person) => {
                requester = Some(person);
                break;
            }
            Party::Deployment { .. } => {
                requester = Some(0);
                break;
            }
            Party::Task(parent) => above = record(domain, parent)?.requester,
        }
    }
    let requester = requester?;
    let why = match &task.phase {
        Phase::Held { why, .. } => *why,
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => return None,
    };
    Some(Box::new(EscalationContext {
        task: number,
        project: task.project,
        requester,
        immediate,
        why,
        escalation: task.escalation.clone(),
    }))
}

pub(crate) fn begin(record: &mut crate::TaskRecord) -> bool {
    match record.phase {
        Phase::Held { .. } => {}
        Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => {
            // Ending/cancellation retires current semantic decision state; root owns history.
            record.escalation = Escalation::Unheld { revision: record.escalation.revision() };
            return false;
        }
    }
    let revision = match record.escalation {
        Escalation::Unheld { revision } => revision,
        Escalation::Routing { .. } | Escalation::Waiting { .. } | Escalation::Rejected { .. } => return false,
    };
    record.escalation = Escalation::Routing {
        revision: revision.checked_add(1).expect("release and restore reserve the next revision"),
    };
    true
}

pub(crate) fn routed(
    domain: &mut Domain,
    env: &Env<Limits>,
    number: u64,
    revision: u64,
    holder: EscalationHolder,
    entry: u64,
    out: &mut Queue<Request>,
) {
    let Some(task) = record(domain, number) else { return };
    let old = match task.escalation {
        Escalation::Routing { revision: current } if current == revision => None,
        Escalation::Waiting { revision: current, holder, .. } if current == revision => Some(holder),
        Escalation::Unheld { .. }
        | Escalation::Routing { .. }
        | Escalation::Waiting { .. }
        | Escalation::Rejected { .. } => return,
    };
    if old == Some(holder) {
        return;
    }
    let revision = match old {
        Some(_) => match revision.checked_add(1) {
            Some(revision) => revision,
            None => return,
        },
        None => revision,
    };
    let valid = match holder {
        EscalationHolder::Task(parent) => parent != number && parent != 0,
        EscalationHolder::Person(person) => {
            person != 0
                && match context(domain, number) {
                    Some(view) => view.requester == person,
                    None => false,
                }
        }
        EscalationHolder::Role { project, .. } => project == task.project,
    };
    if !valid || entry == 0 {
        return;
    }
    task_mut(domain, number).expect("routed task exists").record.escalation =
        Escalation::Waiting { revision, holder, entry, since: env.wall };
    publish(domain, env, number, out);
    wake_holder(domain, env, number, out);
}

#[expect(
    clippy::too_many_arguments,
    reason = "one explicit semantic event carries its identity, revision, actor, choice and terminal destination"
)]
pub(crate) fn decide(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    revision: u64,
    by: u64,
    entry: Option<u64>,
    decision: EscalationDecision,
    out: &mut Queue<Request>,
) {
    let mut outcome = EscalationOutcome::Stale;
    if domain.ready()
        && by != 0
        && let Some(task) = record(domain, number)
    {
        match task.escalation {
            Escalation::Waiting { revision: current, holder, .. } if current == revision => {
                let standing = match holder {
                    EscalationHolder::Task(task) => task == by,
                    EscalationHolder::Person(person) => person == by,
                    EscalationHolder::Role { .. } => true,
                };
                if standing {
                    outcome = validate(task, &decision, holder, entry, &env.limits);
                }
            }
            Escalation::Unheld { .. }
            | Escalation::Routing { .. }
            | Escalation::Waiting { .. }
            | Escalation::Rejected { .. } => {}
        }
    }
    let changed = match outcome {
        EscalationOutcome::Released | EscalationOutcome::Rejected | EscalationOutcome::Passed { .. } => true,
        EscalationOutcome::Stale | EscalationOutcome::NoFurther | EscalationOutcome::Limit => false,
    };
    if changed {
        let task = task_mut(domain, number).expect("decision validated live task");
        match decision {
            EscalationDecision::Release => {
                task.record.escalation = Escalation::Unheld { revision };
                task.record.tries = Tries::NONE;
                task.record.refusals = 0;
                let was = match core::mem::replace(&mut task.record.phase, Phase::Waiting) {
                    Phase::Held { was, .. } => was,
                    Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => {
                        unreachable!("release validated a held task")
                    }
                };
                task.record.phase = match was {
                    Was::Waiting => {
                        task.record.hold_wait_since = None;
                        Phase::Waiting
                    }
                    Was::Active(active) => Phase::Active(active),
                    Was::Closing(closing) => Phase::Closing(closing),
                };
            }
            EscalationDecision::Reject { reason } => {
                task.record.escalation = Escalation::Rejected { revision, by, reason }
            }
            EscalationDecision::Pass { holder } => {
                let entry = entry.expect("root-numbered next holder entry");
                task.record.escalation = Escalation::Waiting {
                    revision: revision.checked_add(1).expect("pass preflight"),
                    holder,
                    entry,
                    since: env.wall,
                }
            }
        }
        publish(domain, env, number, out);
        let due = match record(domain, number) {
            Some(task) => task.phase == Phase::Active(Active::Due),
            None => false,
        };
        if outcome == EscalationOutcome::Released && due {
            activate(domain, number, out);
        }
        match outcome {
            EscalationOutcome::Passed { .. } => wake_holder(domain, env, number, out),
            EscalationOutcome::Released
            | EscalationOutcome::Rejected
            | EscalationOutcome::Stale
            | EscalationOutcome::NoFurther
            | EscalationOutcome::Limit => {}
        }
    }
    out.push(Request::EscalationDecided { reply_to: to, task: number, revision, outcome });
}

fn validate(
    task: &crate::TaskRecord,
    decision: &EscalationDecision,
    holder: EscalationHolder,
    entry: Option<u64>,
    limits: &Limits,
) -> EscalationOutcome {
    match decision {
        EscalationDecision::Release => {
            if task.escalation.revision() == u64::MAX {
                return EscalationOutcome::Limit;
            }
            match &task.phase {
                Phase::Held { .. } => EscalationOutcome::Released,
                Phase::Waiting | Phase::Active(_) | Phase::Closing(_) | Phase::Ended(_) => EscalationOutcome::Stale,
            }
        }
        EscalationDecision::Reject { reason } => {
            if reason.len() <= usize::try_from(limits.result_bytes).expect("u32 fits usize") {
                EscalationOutcome::Rejected
            } else {
                EscalationOutcome::Limit
            }
        }
        EscalationDecision::Pass { holder: next } => {
            if !match entry {
                Some(entry) => entry != 0,
                None => false,
            } {
                return EscalationOutcome::Limit;
            }
            match holder {
                EscalationHolder::Role { .. } => return EscalationOutcome::NoFurther,
                EscalationHolder::Task(_) | EscalationHolder::Person(_) => {}
            }
            match next {
                EscalationHolder::Role { project, .. } if *project != task.project => {
                    return EscalationOutcome::NoFurther;
                }
                EscalationHolder::Task(_) | EscalationHolder::Person(_) | EscalationHolder::Role { .. } => {}
            }
            match task.escalation.revision().checked_add(1) {
                Some(revision) => EscalationOutcome::Passed { revision },
                None => EscalationOutcome::Limit,
            }
        }
    }
}

/// Read-only root preflight/recheck snapshot, at most one Waiting context per
/// live task, with explicit readiness terminal.
pub(crate) fn project_contexts(
    domain: &Domain,
    limits: &Limits,
    project: u32,
) -> Result<Box<[EscalationContext]>, crate::Refusal> {
    if !domain.ready() {
        return Err(crate::Refusal::NotReady);
    }
    let mut contexts = skein_lib::List::with_capacity(limits.tasks);
    for (number, _) in &domain.names {
        let task = record(domain, *number).expect("live name");
        let waiting = match task.escalation {
            Escalation::Waiting { .. } => true,
            Escalation::Unheld { .. } | Escalation::Routing { .. } | Escalation::Rejected { .. } => false,
        };
        if task.project == project
            && waiting
            && let Some(context) = context(domain, *number)
        {
            contexts.push(*context).expect("one context per bounded live task");
        }
    }
    Ok(contexts.into_boxed())
}
