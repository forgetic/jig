//! Connector-configured, whole-set hold admission and bounded waiting
//! (domain/tasks.md, 6.1–6.2). Names are opaque; the hub orders tasks and
//! never interprets a connector's path.
use crate::domain::{Domain, publish, record, task_mut};
use crate::{
    HoldKind, Holding, Kind, Limits, Name, New, Party, Phase, Problem, Refusal, Request, Taken, TaskRecord, Was,
};
use alloc::boxed::Box;
use skein_lib::{Env, Queue};

/// Key for one connector-defined resource kind.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct KindKey {
    pub connector: u16,
    pub kind: u16,
}

/// Install immutable connector configuration before admission.
pub(crate) fn kinds(domain: &mut Domain, limits: &Limits, connector: u16, kinds: &[Kind]) {
    assert!(
        kinds.len() <= usize::try_from(limits.hold_kinds).expect("u32 fits usize"),
        "connector hold kinds fit the configured bound"
    );
    for configured in kinds {
        assert!(configured.connector == connector, "kind belongs to connector");
        let key = KindKey { connector, kind: configured.kind };
        if let Some(old) = domain.hold_kinds.get(&key) {
            assert!(*old == configured.hold, "hold kind cannot change while tasks live");
        } else {
            domain.hold_kinds.insert(key, configured.hold).expect("configured kind capacity");
        }
    }
}

fn name(holding: &Holding) -> &Name {
    match holding {
        Holding::Write { resource, .. } => resource,
        Holding::Slot { pool, .. } => pool,
    }
}

fn kind(holding: &Holding) -> u16 {
    match holding {
        Holding::Write { kind, .. } | Holding::Slot { kind, .. } => *kind,
    }
}

fn rule(domain: &Domain, holding: &Holding) -> Option<HoldKind> {
    let key = KindKey { connector: name(holding).connector, kind: kind(holding) };
    domain.hold_kinds.get(&key).copied()
}

fn shape(limits: &Limits, name: &Name) -> bool {
    if name.path.len() > usize::try_from(limits.hold_segments).expect("u32 fits usize") {
        return false;
    }
    let mut bytes = 0_u32;
    for segment in &name.path {
        let Some(total) = bytes.checked_add(u32::try_from(segment.len()).unwrap_or(u32::MAX)) else {
            return false;
        };
        bytes = total;
    }
    bytes <= limits.hold_bytes
}

fn same(a: &Holding, b: &Holding) -> bool {
    name(a) == name(b)
}

fn holder(domain: &Domain, holding: &Holding) -> Option<u64> {
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("indexed live task");
        if row.holds_taken {
            for held in &row.holdings {
                if same(held, holding) {
                    return Some(*number);
                }
            }
        }
    }
    None
}

fn waiting_count(domain: &Domain, holding: &Holding) -> u32 {
    let mut count = 0_u32;
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("indexed live task");
        if row.phase == Phase::Waiting && row.hold_wait_since.is_some() && !row.holds_taken {
            for needed in &row.holdings {
                if same(needed, holding) {
                    count = count.saturating_add(1);
                    break;
                }
            }
        }
    }
    count
}

/// Preflight a complete batch without granting any of its resources.
pub(crate) fn check_batch(domain: &Domain, limits: &Limits, batch: &[New]) -> Result<(), Problem> {
    for (at, new) in batch.iter().enumerate() {
        if new.holdings.len() > usize::try_from(limits.holdings).expect("u32 fits usize") {
            return Err(Problem::new(Some(new.number), Refusal::Holds));
        }
        for (index, needed) in new.holdings.iter().enumerate() {
            if !shape(limits, name(needed)) {
                return Err(Problem::new(Some(new.number), Refusal::Holds));
            }
            let Some(rule) = rule(domain, needed) else {
                return Err(Problem::new(Some(new.number), Refusal::HoldKind));
            };
            let taken = match (needed, rule) {
                (Holding::Write { .. }, HoldKind::Exclusive { taken })
                | (Holding::Slot { .. }, HoldKind::Pooled { taken }) => taken,
                (Holding::Write { .. }, HoldKind::Pooled { .. })
                | (Holding::Slot { .. }, HoldKind::Exclusive { .. }) => {
                    return Err(Problem::new(Some(new.number), Refusal::HoldKind));
                }
            };
            for earlier in new.holdings.iter().take(index) {
                if same(earlier, needed) {
                    return Err(Problem::new(Some(new.number), Refusal::Holds));
                }
            }
            let mut occupied = holder(domain, needed);
            for earlier in batch.iter().take(at) {
                for prior in &earlier.holdings {
                    if same(prior, needed) {
                        occupied = Some(earlier.number);
                    }
                }
            }
            if let Some(blocker) = occupied {
                match taken {
                    Taken::Refuses => {
                        return Err(Problem {
                            task: Some(new.number),
                            why: Refusal::HoldTaken,
                            blocked_by: Some(Box::new([blocker])),
                        });
                    }
                    Taken::Waits => {}
                }
            }
            let mut queued = waiting_count(domain, needed);
            for earlier in batch.iter().take(at) {
                for prior in &earlier.holdings {
                    if same(prior, needed) {
                        queued = queued.saturating_add(1);
                    }
                }
            }
            if queued >= limits.hold_waiters && occupied.is_some() {
                return Err(Problem::new(Some(new.number), Refusal::Holds));
            }
        }
    }
    Ok(())
}

/// True when every required hold is free; there is no partial acquisition.
pub(crate) fn free(domain: &Domain, number: u64) -> bool {
    let row = record(domain, number).expect("waiting task live");
    for needed in &row.holdings {
        if holder(domain, needed).is_some() {
            return false;
        }
    }
    true
}

fn priority(domain: &Domain, number: u64, bound: u32) -> u32 {
    let mut current = Some(number);
    for _ in 0..bound {
        let Some(at) = current else { break };
        let row = record(domain, at).expect("live ancestor");
        if let Some(priority) = row.tracked {
            return priority;
        }
        current = match row.requester {
            Party::Task(parent) => Some(parent),
            Party::Person(_) | Party::Deployment { .. } => None,
        };
    }
    0
}

fn earlier(domain: &Domain, left: u64, right: u64, limits: &Limits) -> bool {
    let lp = priority(domain, left, limits.tasks);
    let rp = priority(domain, right, limits.tasks);
    if lp != rp {
        return lp > rp;
    }
    let l = record(domain, left).expect("candidate live");
    let r = record(domain, right).expect("candidate live");
    let lt = l.hold_wait_since.unwrap_or(l.created_at);
    let rt = r.hold_wait_since.unwrap_or(r.created_at);
    lt < rt || (lt == rt && left < right)
}

/// Select the earliest eligible whole-set waiter, independent of arena order.
pub(crate) fn next_ready(domain: &Domain, limits: &Limits) -> Option<u64> {
    let mut selected = None;
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("indexed live task");
        if row.phase != Phase::Waiting
            || row.holdings.is_empty()
            || !row.waiting_on.is_empty()
            || !free(domain, *number)
        {
            continue;
        }
        selected = match selected {
            Some(old) if earlier(domain, old, *number, limits) => Some(old),
            Some(_) | None => Some(*number),
        };
    }
    selected
}

/// Persist a wait start once; the expiry is projected from the wall clock.
pub(crate) fn wait(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let row = record(domain, number).expect("waiting task live");
    if row.hold_wait_since.is_some() || row.holdings.is_empty() {
        return;
    }
    let mut blocked = None;
    for needed in &row.holdings {
        if holder(domain, needed).is_some() {
            blocked = Some(name(needed).clone());
            break;
        }
    }
    let Some(resource) = blocked else { return };
    task_mut(domain, number).expect("waiting task live").record.hold_wait_since = Some(env.wall);
    publish(domain, env, number, out);
    let mut place = 1_u32;
    for (other, _) in &domain.names {
        if *other != number {
            let row = record(domain, *other).expect("indexed live task");
            if row.phase == Phase::Waiting
                && row.hold_wait_since.is_some()
                && earlier(domain, *other, number, &env.limits)
            {
                for needed in &row.holdings {
                    if name(needed) == &resource {
                        place = place.saturating_add(1);
                        break;
                    }
                }
            }
        }
    }
    out.push(Request::Waiting { task: number, resource, place });
    let due = env.now.saturating_add(env.limits.hold_wait);
    domain.hold_alarms.arm(number, due).expect("one alarm per task");
}

/// Take every resource in the same commit as activation.
pub(crate) fn take(domain: &mut Domain, _env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let holdings = record(domain, number).expect("waiting task live").holdings.clone();
    let row = task_mut(domain, number).expect("waiting task live");
    row.record.holds_taken = true;
    row.record.hold_wait_since = None;
    domain.hold_alarms.cancel(number);
    if !holdings.is_empty() {
        out.push(Request::Taken { task: number, holdings });
    }
}

/// Hold a task whose configured wait bound elapsed, retaining no resource.
pub(crate) fn expire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    if let Some(number) = domain.hold_alarms.expire(env.now) {
        let waiting = match record(domain, number) {
            Some(row) => row.phase == Phase::Waiting && row.hold_wait_since.is_some() && !row.holds_taken,
            None => false,
        };
        if waiting {
            crate::run::hold(domain, env, number, crate::Hold::HoldsWaited, out);
        }
    }
}

/// Forget a settled task's whole hold set before the next readiness pass.
pub(crate) fn release(domain: &mut Domain, number: u64) {
    domain.hold_alarms.cancel(number);
}

/// Validate one live row against the configured kinds and prior restored rows.
pub(crate) fn valid_record(domain: &Domain, limits: &Limits, row: &TaskRecord) -> bool {
    if row.holdings.len() > usize::try_from(limits.holdings).expect("u32 fits usize") {
        return false;
    }
    for (at, holding) in row.holdings.iter().enumerate() {
        if !shape(limits, name(holding)) {
            return false;
        }
        let valid_kind = match (holding, rule(domain, holding)) {
            (Holding::Write { .. }, Some(HoldKind::Exclusive { .. }))
            | (Holding::Slot { .. }, Some(HoldKind::Pooled { .. })) => true,
            (Holding::Write { .. }, Some(HoldKind::Pooled { .. }) | None)
            | (Holding::Slot { .. }, Some(HoldKind::Exclusive { .. }) | None) => false,
        };
        if !valid_kind {
            return false;
        }
        for earlier in row.holdings.iter().take(at) {
            if same(earlier, holding) {
                return false;
            }
        }
        if row.holds_taken && holder(domain, holding).is_some() {
            return false;
        }
    }
    let (must_take, must_free) = match row.phase {
        Phase::Waiting | Phase::Held { was: Was::Waiting, .. } => (false, true),
        Phase::Active(_) | Phase::Held { was: Was::Active(_), .. } => (true, false),
        Phase::Closing(_) | Phase::Held { was: Was::Closing(_), .. } => (false, false),
        Phase::Ended(_) => return false,
    };
    if (must_take && !row.holds_taken) || (must_free && row.holds_taken) {
        return false;
    }
    if row.hold_wait_since.is_some() && (row.holds_taken || row.holdings.is_empty()) {
        return false;
    }
    true
}

/// Rebuild the wall-projected wait timers after all live rows were restored.
pub(crate) fn restore(domain: &mut Domain, env: &Env<Limits>) {
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("restored live row");
        if row.phase == Phase::Waiting
            && let Some(since) = row.hold_wait_since
        {
            let until = since.as_nanos().saturating_add(env.limits.hold_wait.as_nanos());
            let remaining = until.saturating_sub(env.wall.as_nanos());
            let due = env.now.saturating_add(skein_lib::Duration::from_nanos(remaining));
            domain.hold_alarms.arm(*number, due).expect("one restored alarm per waiting task");
        }
    }
}
