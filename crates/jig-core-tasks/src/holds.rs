//! Connector-configured, whole-set hold admission and bounded waiting
//! (domain/tasks.md, 6.1–6.2). Names are opaque; the hub orders tasks and
//! never interprets a connector's path.
use crate::domain::{Domain, publish, record, task_mut};
use crate::{
    HoldKind, Holding, Kind, Limits, Name, New, Party, Phase, PoolSlots, Problem, Refusal, Request, Stored, Taken,
    TaskRecord, Was,
};
use alloc::boxed::Box;
use skein_lib::{Env, List, Queue};

/// Key for one connector-defined resource kind.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct KindKey {
    pub connector: u16,
    pub kind: u16,
}

/// Cache a named report. Reports may arrive before task creation. At capacity,
/// forget one report no live task names; admission always waits for a missing report.
pub(crate) fn resource(domain: &mut Domain, limits: &Limits, name: Name, hold: HoldKind) {
    if !shape(limits, &name) {
        return;
    }
    if !domain.resources.contains_key(&name) && domain.resources.len() == domain.resources.capacity() {
        let mut unused = None;
        for (known, _) in &domain.resources {
            let mut named = false;
            for (number, _) in &domain.names {
                let row = record(domain, *number).expect("live task");
                for holding in &row.holdings {
                    if self::name(holding) == known {
                        named = true;
                    }
                }
            }
            if !named {
                unused = Some(known.clone());
                break;
            }
        }
        match unused {
            Some(unused) => {
                domain.resources.remove(&unused);
            }
            None => return,
        }
    }
    let _previous = domain.resources.insert(name, hold);
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

/// Keep the latest connector count durable; a shrink changes admission but
/// leaves all current holders in place.
pub(crate) fn slots(domain: &mut Domain, limits: &Limits, pool: Name, slots: u32, out: &mut Queue<Request>) {
    if !domain.ready() || !shape(limits, &pool) {
        return;
    }
    let number = match domain.pools.get(&pool) {
        Some(row) => row.number,
        None => {
            assert!(domain.pools.len() < limits.pools, "configured pool room");
            let number = domain.next_pool.checked_add(1).expect("pool number fits");
            domain.next_pool = number;
            number
        }
    };
    let row = PoolSlots { number, pool: pool.clone(), slots };
    domain.pools.insert(pool, row.clone()).expect("pool room preflighted");
    out.push(Request::Save { record: Stored::Pool(row) });
}

pub(crate) fn restore_pool(domain: &mut Domain, limits: &Limits, row: PoolSlots) -> bool {
    if row.number == 0
        || !shape(limits, &row.pool)
        || domain.pools.contains_key(&row.pool)
        || domain.pools.len() == domain.pools.capacity()
    {
        return false;
    }
    for (_, known) in &domain.pools {
        if known.number == row.number {
            return false;
        }
    }
    domain.next_pool = domain.next_pool.max(row.number);
    domain.pools.insert(row.pool.clone(), row).is_ok()
}

/// A connector knows an individual allocation vanished even though the
/// holder's pool slot remains reserved until its task releases it.
pub(crate) fn allocation_gone(
    domain: &mut Domain,
    env: &Env<Limits>,
    pool: &Name,
    task: u64,
    out: &mut Queue<Request>,
) {
    let held = match record(domain, task) {
        Some(row) if row.holds_taken => {
            let mut held = false;
            for holding in &row.holdings {
                if let Holding::Slot { pool: name, .. } = holding
                    && name == pool
                {
                    held = true;
                    break;
                }
            }
            held
        }
        Some(_) | None => false,
    };
    if held {
        crate::run::hold(domain, env, task, crate::Hold::Drift, out);
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
    match domain.resources.get(name(holding)) {
        Some(rule) => Some(*rule),
        None => domain.hold_kinds.get(&key).copied(),
    }
}

fn matching(holding: &Holding, rule: HoldKind) -> bool {
    match holding {
        Holding::Write { .. } => match rule {
            HoldKind::Exclusive { .. } | HoldKind::Shared => true,
            HoldKind::Pooled { .. } => false,
        },
        Holding::Slot { .. } => match rule {
            HoldKind::Pooled { .. } | HoldKind::Shared => true,
            HoldKind::Exclusive { .. } => false,
        },
    }
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
                if same(held, holding) && rule(domain, held) != Some(HoldKind::Shared) {
                    return Some(*number);
                }
            }
        }
    }
    None
}

fn holders(domain: &Domain, holding: &Holding) -> u32 {
    let mut count = 0_u32;
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("indexed live task");
        if row.holds_taken {
            for held in &row.holdings {
                if same(held, holding) {
                    count = count.saturating_add(1);
                    break;
                }
            }
        }
    }
    count
}

fn held_by(domain: &Domain, task: u64, holding: &Holding) -> bool {
    let Some(row) = record(domain, task) else { return false };
    if !row.holds_taken {
        return false;
    }
    for held in &row.holdings {
        if same(held, holding) {
            return true;
        }
    }
    false
}

fn available(domain: &Domain, holding: &Holding, extra: u32) -> bool {
    match rule(domain, holding) {
        Some(HoldKind::Shared) => true,
        Some(HoldKind::Exclusive { .. }) => holder(domain, holding).is_none() && extra == 0,
        Some(HoldKind::Pooled { .. }) => {
            let slots = match domain.pools.get(name(holding)) {
                Some(row) => row.slots,
                None => 0,
            };
            holders(domain, holding).saturating_add(extra) < slots
        }
        None => false,
    }
}

/// Current task that owns an exclusive resource, including a handed-down one.
pub(crate) fn writer_holder(domain: &Domain, resource: &Name) -> Option<u64> {
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("indexed live task");
        if row.holds_taken {
            for held in &row.holdings {
                if name(held) == resource
                    && match rule(domain, held) {
                        Some(HoldKind::Exclusive { .. }) => true,
                        Some(HoldKind::Pooled { .. } | HoldKind::Shared) | None => false,
                    }
                {
                    return Some(*number);
                }
            }
        }
    }
    None
}

/// Whether an opaque resource name fits the configured hold bounds.
#[must_use]
pub fn valid_name(limits: &Limits, resource: &Name) -> bool {
    shape(limits, resource)
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
#[expect(clippy::too_many_lines, reason = "whole-batch hold admission checks every resource before mutation")]
pub(crate) fn check_batch(domain: &Domain, limits: &Limits, creator: Party, batch: &[New]) -> Result<(), Problem> {
    for (at, new) in batch.iter().enumerate() {
        if new.holdings.len() > usize::try_from(limits.holdings).expect("u32 fits usize") {
            return Err(Problem::new(Some(new.number), Refusal::Holds));
        }
        for (index, needed) in new.holdings.iter().enumerate() {
            if !shape(limits, name(needed)) {
                return Err(Problem::new(Some(new.number), Refusal::Holds));
            }
            if available_access(domain, new.project, name(needed)) == crate::ResourceAccess::Unavailable {
                return Err(Problem::new(Some(new.number), Refusal::ResourceUnavailable));
            }
            let Some(rule) = rule(domain, needed) else {
                return Err(Problem::new(Some(new.number), Refusal::HoldKind));
            };
            let taken = match rule {
                HoldKind::Shared => Taken::Waits,
                HoldKind::Exclusive { taken } | HoldKind::Pooled { taken } => taken,
            };
            if !domain.resources.contains_key(name(needed)) && !matching(needed, rule) {
                return Err(Problem::new(Some(new.number), Refusal::HoldKind));
            }
            for earlier in new.holdings.iter().take(index) {
                if same(earlier, needed) {
                    return Err(Problem::new(Some(new.number), Refusal::Holds));
                }
            }
            let mut occupied = holder(domain, needed);
            let mut earlier_count = 0_u32;
            for earlier in batch.iter().take(at) {
                for prior in &earlier.holdings {
                    if same(prior, needed) {
                        occupied = Some(earlier.number);
                        earlier_count = earlier_count.saturating_add(1);
                    }
                }
            }
            let handed_down = match creator {
                Party::Task(parent) => earlier_count == 0 && held_by(domain, parent, needed),
                Party::Person(_) | Party::Deployment { .. } => false,
            };
            if handed_down && !new.dependencies.is_empty() {
                return Err(Problem::new(Some(new.number), Refusal::Holds));
            }
            let blocked = !handed_down && !available(domain, needed, earlier_count);
            if blocked {
                match taken {
                    Taken::Refuses => {
                        return Err(Problem {
                            task: Some(new.number),
                            why: Refusal::HoldTaken,
                            blocked_by: match occupied {
                                Some(owner) => Some(Box::new([owner])),
                                None => None,
                            },
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
            if queued >= limits.hold_waiters && blocked {
                return Err(Problem::new(Some(new.number), Refusal::Holds));
            }
        }
        let mut handing_down = false;
        if let Party::Task(parent) = creator {
            for needed in &new.holdings {
                if held_by(domain, parent, needed) {
                    handing_down = true;
                    break;
                }
            }
        }
        if handing_down {
            for needed in &new.holdings {
                let mut earlier_holder = false;
                for earlier in batch.iter().take(at) {
                    for prior in &earlier.holdings {
                        if same(prior, needed) {
                            earlier_holder = true;
                            break;
                        }
                    }
                }
                if earlier_holder {
                    return Err(Problem::new(Some(new.number), Refusal::HoldTaken));
                }
                let transferred = match creator {
                    Party::Task(parent) => held_by(domain, parent, needed),
                    Party::Person(_) | Party::Deployment { .. } => false,
                };
                if !transferred && !available(domain, needed, 0) {
                    return Err(Problem::new(Some(new.number), Refusal::HoldTaken));
                }
            }
        }
    }
    Ok(())
}

/// True when every required hold is free; there is no partial acquisition.
pub(crate) fn free(domain: &Domain, number: u64) -> bool {
    let row = record(domain, number).expect("waiting task live");
    for needed in &row.holdings {
        if available_access(domain, row.project, name(needed)) == crate::ResourceAccess::Unavailable
            || !available(domain, needed, 0)
        {
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
        if !available(domain, needed, 0) {
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
    let mut taken = List::with_capacity(u32::try_from(holdings.len()).expect("bounded task holdings"));
    for holding in holdings {
        if rule(domain, &holding) != Some(HoldKind::Shared) {
            taken.push(holding).expect("bounded task holdings");
        }
    }
    if !taken.is_empty() {
        out.push(Request::Taken { task: number, holdings: taken.into_boxed() });
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

/// A connector's failed-resource cleanup can leave its resource for the tree root.
/// Transfer ownership before the failed task ends, so admission still sees a holder.
pub(crate) fn retained(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    root: u64,
    holding: &Holding,
    out: &mut Queue<Request>,
) {
    if !domain.ready() || task == root || !valid_name(&env.limits, name(holding)) {
        return;
    }
    let Some(child) = record(domain, task) else { return };
    if child.root != root || !child.holds_taken {
        return;
    }
    let closing = match &child.phase {
        Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. } => closing,
        Phase::Waiting | Phase::Active(_) | Phase::Held { .. } | Phase::Ended(_) => return,
    };
    if closing.stage != crate::Stage::Releases {
        return;
    }
    match closing.ending {
        crate::Ending::Failed { .. } => {}
        crate::Ending::Done(_) | crate::Ending::Cancelled { .. } => return,
    }
    let Some(parent) = record(domain, root) else { return };
    if parent.root != root || !parent.holds_taken {
        return;
    }
    for held in &parent.holdings {
        if same(held, holding) {
            return;
        }
    }
    let capacity = env.limits.tasks.checked_mul(env.limits.holdings).expect("bounded live holds");
    if parent.holdings.len() >= usize::try_from(capacity).expect("u32 fits usize") {
        return;
    }
    let mut kept = List::with_capacity(capacity);
    let mut found = false;
    for held in &child.holdings {
        if same(held, holding) {
            found = true;
        } else {
            kept.push(held.clone()).expect("child holds bounded");
        }
    }
    if !found {
        return;
    }
    let mut owned = List::with_capacity(capacity);
    for held in &parent.holdings {
        owned.push(held.clone()).expect("root holds bounded");
    }
    owned.push(holding.clone()).expect("tree holds bounded");
    task_mut(domain, task).expect("retained child live").record.holdings = kept.into_boxed();
    task_mut(domain, root).expect("retaining root live").record.holdings = owned.into_boxed();
    publish(domain, env, task, out);
    publish(domain, env, root, out);
    out.push(Request::Taken { task: root, holdings: Box::new([holding.clone()]) });
}

/// Validate one live row against the configured kinds and prior restored rows.
pub(crate) fn valid_record(domain: &Domain, limits: &Limits, row: &TaskRecord) -> bool {
    let bound = if row.root == row.number {
        limits.tasks.checked_mul(limits.holdings).expect("validated live hold bound")
    } else {
        limits.holdings
    };
    if row.holdings.len() > usize::try_from(bound).expect("u32 fits usize") {
        return false;
    }
    for (at, holding) in row.holdings.iter().enumerate() {
        if !shape(limits, name(holding)) {
            return false;
        }
        let valid_kind = match rule(domain, holding) {
            Some(rule) => matching(holding, rule) || domain.resources.contains_key(name(holding)),
            None => false,
        };
        if !valid_kind {
            return false;
        }
        for earlier in row.holdings.iter().take(at) {
            if same(earlier, holding) {
                return false;
            }
        }
        if row.holds_taken
            && let Holding::Write { .. } = holding
            && holder(domain, holding).is_some()
        {
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

/// Project-local access key; holds themselves remain global by name.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct ResourceKey {
    project: u32,
    name: Name,
}

pub(crate) fn available_access(domain: &Domain, project: u32, name: &Name) -> crate::ResourceAccess {
    match domain.resource_access.get(&ResourceKey { project, name: name.clone() }) {
        Some(access) => *access,
        None if domain.resource_access_full => crate::ResourceAccess::Unavailable,
        None => crate::ResourceAccess::Writable,
    }
}

pub(crate) fn access(
    domain: &mut Domain,
    env: &Env<Limits>,
    project: u32,
    name: Name,
    access: crate::ResourceAccess,
    out: &mut Queue<Request>,
) {
    if !shape(&env.limits, &name) {
        return;
    }
    let key = ResourceKey { project, name: name.clone() };
    if access == crate::ResourceAccess::Writable && !domain.resource_access_full {
        domain.resource_access.remove(&key);
    } else if domain.resource_access.insert(key, access).is_err() {
        domain.resource_access_full = true;
    }
    let mut stopped = List::with_capacity(env.limits.tasks);
    for (number, _) in &domain.names {
        let row = record(domain, *number).expect("live task");
        if row.project != project {
            continue;
        }
        let mut unavailable = false;
        if access == crate::ResourceAccess::Unavailable {
            for holding in &row.holdings {
                unavailable |= self::name(holding) == &name;
            }
        }
        let mut forbidden = false;
        if access != crate::ResourceAccess::Writable
            && let Some(slot) = domain.writers.get(&name)
        {
            match slot.writer {
                crate::Writer::Run { task, .. } => forbidden = task == *number,
                crate::Writer::Effect { .. } => {}
            }
        }
        if unavailable || forbidden {
            stopped
                .push((
                    *number,
                    if unavailable { crate::Hold::ResourceUnavailable } else { crate::Hold::ResourceAccess },
                ))
                .expect("bounded live tasks");
        }
    }
    for (number, why) in stopped.into_boxed() {
        crate::run::hold(domain, env, number, why, out);
    }
}
