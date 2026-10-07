//! Connector-owned effects and recovery (domain/connectors.md, section 4).
//!
//! The root commits each `Save` before releasing the accompanying `Make` or
//! `System` request. A restored entry is already durable, so its first action
//! is to settle an earlier attempt or prepare a new one. Entries with a common
//! resource execute in their numbered decision order.
use alloc::boxed::Box;
use skein_lib::{Env, Map, Queue, Token, Wall};

use crate::domain::{Domain, resource_spec, valid_path};
use crate::{
    ApplyResult, Attempt, Config, Description, Effect, EffectPhase, Key, KindSpec, Limits, Looked, OutboxEntry,
    Outcome, Path, Record, RecordKey, Recovery, Request, SystemRequest,
};

/// The live object this deployment owns after an effect settles.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Made {
    resources: Box<[Path]>,
    state: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Runtime {
    released: bool,
    checking: bool,
    needs_lookup: bool,
}

/// Staged effects, durable entries and made objects.
#[derive(Debug)]
pub(crate) struct Outbox {
    staged: Map<Token, Effect>,
    entries: Map<u64, OutboxEntry>,
    runtime: Map<u64, Runtime>,
    made: Map<Key, Made>,
}

impl Outbox {
    pub(crate) fn new(limits: &Limits) -> Outbox {
        Outbox {
            staged: Map::with_capacity(limits.staged),
            entries: Map::with_capacity(limits.entries),
            runtime: Map::with_capacity(limits.entries),
            made: Map::with_capacity(limits.made),
        }
    }
}

fn kind(config: &Config, number: u16) -> Option<KindSpec> {
    for spec in &config.kinds {
        if spec.kind == number {
            return Some(*spec);
        }
    }
    None
}

fn valid_effect(config: &Config, limits: &Limits, effect: &Effect) -> Option<KindSpec> {
    let spec = kind(config, effect.kind)?;
    let size = u32::try_from(effect.resources.len()).ok()?;
    if size == 0 || size > limits.resources_per_effect {
        return None;
    }
    if spec.recovery == Recovery::Conditional && effect.condition.is_none() {
        return None;
    }
    for resource in &effect.resources {
        if !resource.under(&config.prefix) || !valid_path(resource, limits) {
            return None;
        }
        let resource = resource_spec(config, resource)?;
        if !resource.writable {
            return None;
        }
    }
    Some(spec)
}

pub(crate) fn describe(domain: &mut Domain, env: &Env<Limits>, token: Token, effect: Effect, out: &mut Queue<Request>) {
    let spec = valid_effect(&domain.config, &env.limits, &effect);
    let Some(spec) = spec else {
        out.push(Request::EffectRefused { token });
        return;
    };
    if domain.outbox.staged.contains_key(&token) || domain.outbox.staged.len() >= env.limits.staged {
        out.push(Request::EffectRefused { token });
        return;
    }
    let description = Description {
        kind: effect.kind,
        resources: effect.resources.clone(),
        purpose: effect.purpose,
        condition: effect.condition.is_some(),
        form: spec.form,
        recovery: spec.recovery,
        price: spec.price,
        state: effect.state,
    };
    domain.outbox.staged.insert(token, effect).expect("stage capacity checked");
    out.push(Request::Described { token, description });
}

pub(crate) fn drop_staged(domain: &mut Domain, token: Token) {
    domain.outbox.staged.remove(&token);
}

pub(crate) fn keep(
    domain: &mut Domain,
    env: &Env<Limits>,
    token: Token,
    number: u64,
    task: u64,
    key: Key,
    out: &mut Queue<Request>,
) {
    let Some(effect) = domain.outbox.staged.remove(&token) else {
        out.push(Request::EffectRefused { token });
        return;
    };
    if key.deployment != domain.config.deployment || key.task != task || key.purpose != effect.purpose {
        out.push(Request::EffectRefused { token });
        return;
    }
    if let Some(made) = domain.outbox.made.get(&key) {
        out.push(Request::Outcome { entry: number, outcome: Outcome::Made { state: made.state, found: true } });
        return;
    }
    for (_, existing) in &domain.outbox.entries {
        if existing.key == key {
            out.push(Request::EffectRefused { token });
            return;
        }
    }
    if domain.outbox.entries.contains_key(&number)
        || domain.outbox.entries.len() >= env.limits.entries
        || domain.outbox.made.len().saturating_add(domain.outbox.entries.len()) >= env.limits.made
    {
        out.push(Request::EffectRefused { token });
        return;
    }
    let entry = OutboxEntry { number, task, key, effect, attempt: None, phase: EffectPhase::Kept };
    domain.outbox.entries.insert(number, entry.clone()).expect("outbox capacity checked");
    domain
        .outbox
        .runtime
        .insert(number, Runtime { released: false, checking: false, needs_lookup: false })
        .expect("one runtime row per outbox entry");
    out.push(Request::Save { record: Record::Outbox(entry) });
    out.push(Request::Make { entry: number });
}

pub(crate) fn make(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let Some(runtime) = domain.outbox.runtime.get_mut(&number) else {
        return;
    };
    runtime.released = true;
    progress(domain, env, number, out);
}

pub(crate) fn withdraw(domain: &mut Domain, number: u64, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.get(&number) else {
        return;
    };
    if entry.attempt.is_some() {
        return;
    }
    let key = entry.key;
    domain.outbox.entries.remove(&number);
    domain.outbox.runtime.remove(&number);
    out.push(Request::Erase { key: RecordKey::Outbox(key) });
    out.push(Request::Outcome { entry: number, outcome: Outcome::Withdrawn });
}

pub(crate) fn restore_entry(domain: &mut Domain, mut entry: OutboxEntry) {
    match entry.phase {
        EffectPhase::Sent => entry.phase = EffectPhase::Uncertain,
        EffectPhase::Kept | EffectPhase::Uncertain | EffectPhase::Retry | EffectPhase::Held => {}
    }
    let number = entry.number;
    let needs_lookup = entry.phase == EffectPhase::Uncertain;
    domain.outbox.entries.insert(number, entry).expect("restored outbox entry fits");
    domain
        .outbox
        .runtime
        .insert(number, Runtime { released: true, checking: false, needs_lookup })
        .expect("restored runtime entry fits");
}

pub(crate) fn restore_made(domain: &mut Domain, key: Key, resources: Box<[Path]>, state: u64) {
    domain.outbox.made.insert(key, Made { resources, state }).expect("restored made object fits");
}

fn overlap(left: &[Path], right: &[Path]) -> bool {
    for a in left {
        for b in right {
            if a == b {
                return true;
            }
        }
    }
    false
}

fn first_on_resources(domain: &Domain, number: u64, resources: &[Path]) -> bool {
    for (earlier, entry) in &domain.outbox.entries {
        if *earlier < number && overlap(&entry.effect.resources, resources) {
            return false;
        }
    }
    true
}

fn deadline(env: &Env<Limits>) -> Wall {
    Wall::from_nanos(
        env.wall
            .as_nanos()
            .saturating_add(env.limits.write_lifetime.as_nanos())
            .saturating_add(env.limits.clock_margin.as_nanos()),
    )
}

fn attempt_due(attempt: Option<Attempt>, now: Wall) -> bool {
    match attempt {
        Some(attempt) => attempt.deadline <= now,
        None => false,
    }
}

fn start_attempt(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.get_mut(&number) else {
        return;
    };
    let next = match entry.attempt {
        Some(attempt) => attempt.number.checked_add(1),
        None => Some(1),
    };
    let Some(next) = next else {
        hold_attempt_limit(entry, number, out);
        return;
    };
    if next > env.limits.max_attempts {
        hold_attempt_limit(entry, number, out);
        return;
    }
    let attempt = Attempt { number: next, sent: env.wall, deadline: deadline(env) };
    entry.attempt = Some(attempt);
    entry.phase = EffectPhase::Sent;
    let call = SystemRequest::Apply {
        entry: number,
        attempt: next,
        key: entry.key,
        effect: entry.effect.clone(),
        form: kind(&domain.config, entry.effect.kind).expect("kept effect kind remains configured").form,
        recovery: kind(&domain.config, entry.effect.kind).expect("kept effect kind remains configured").recovery,
    };
    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
    out.push(Request::System(call));
}

fn hold_attempt_limit(entry: &mut OutboxEntry, number: u64, out: &mut Queue<Request>) {
    let outcome = match entry.phase {
        EffectPhase::Retry => Outcome::Failed,
        EffectPhase::Kept | EffectPhase::Sent | EffectPhase::Uncertain | EffectPhase::Held => Outcome::Uncertain,
    };
    entry.phase = EffectPhase::Held;
    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
    out.push(Request::Outcome { entry: number, outcome });
}

fn look(domain: &mut Domain, number: u64, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.get(&number) else {
        return;
    };
    let spec = kind(&domain.config, entry.effect.kind).expect("kept effect kind remains configured");
    let Some(runtime) = domain.outbox.runtime.get_mut(&number) else {
        return;
    };
    if runtime.checking {
        return;
    }
    runtime.checking = true;
    runtime.needs_lookup = false;
    out.push(Request::System(SystemRequest::Look {
        entry: number,
        key: entry.key,
        effect: entry.effect.clone(),
        recovery: spec.recovery,
    }));
}

fn progress(domain: &mut Domain, env: &Env<Limits>, number: u64, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.get(&number) else {
        return;
    };
    let Some(runtime) = domain.outbox.runtime.get(&number) else {
        return;
    };
    if !runtime.released || runtime.checking || !first_on_resources(domain, number, &entry.effect.resources) {
        return;
    }
    let phase = entry.phase;
    let attempt = entry.attempt;
    let recovery = kind(&domain.config, entry.effect.kind).expect("kept effect kind remains configured").recovery;
    let needs_lookup = runtime.needs_lookup;
    match phase {
        EffectPhase::Kept => start_attempt(domain, env, number, out),
        EffectPhase::Sent => {
            let Some(attempt) = attempt else {
                return;
            };
            if env.wall < attempt.deadline {
                return;
            }
            uncertain(domain, number, recovery, out);
        }
        EffectPhase::Retry => {
            let due = match attempt {
                Some(attempt) => attempt.deadline <= env.wall,
                None => true,
            };
            if due {
                start_attempt(domain, env, number, out);
            }
        }
        EffectPhase::Uncertain => match recovery {
            Recovery::Keyed | Recovery::Conditional => {
                if needs_lookup || attempt_due(attempt, env.wall) {
                    look(domain, number, out);
                }
            }
            Recovery::Idempotent => {
                if attempt_due(attempt, env.wall) {
                    start_attempt(domain, env, number, out);
                }
            }
            Recovery::Unrecoverable => hold_uncertain(domain, number, out),
        },
        EffectPhase::Held => {}
    }
}

fn uncertain(domain: &mut Domain, number: u64, recovery: Recovery, out: &mut Queue<Request>) {
    match recovery {
        Recovery::Unrecoverable => hold_uncertain(domain, number, out),
        Recovery::Keyed | Recovery::Conditional | Recovery::Idempotent => {
            let entry = domain.outbox.entries.get_mut(&number).expect("an unsettled entry remains");
            entry.phase = EffectPhase::Uncertain;
            out.push(Request::Save { record: Record::Outbox(entry.clone()) });
            out.push(Request::Outcome { entry: number, outcome: Outcome::Uncertain });
            match recovery {
                Recovery::Keyed | Recovery::Conditional => look(domain, number, out),
                Recovery::Idempotent | Recovery::Unrecoverable => {}
            }
        }
    }
}

fn hold_uncertain(domain: &mut Domain, number: u64, out: &mut Queue<Request>) {
    let entry = domain.outbox.entries.get_mut(&number).expect("an unsettled entry remains");
    if entry.phase == EffectPhase::Held {
        return;
    }
    entry.phase = EffectPhase::Held;
    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
    out.push(Request::Outcome { entry: number, outcome: Outcome::Uncertain });
}

fn settle_made(domain: &mut Domain, number: u64, state: u64, found: bool, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.remove(&number) else {
        return;
    };
    domain.outbox.runtime.remove(&number);
    let made = Made { resources: entry.effect.resources.clone(), state };
    domain.outbox.made.insert(entry.key, made).expect("ownership reserved at keep");
    out.push(Request::Erase { key: RecordKey::Outbox(entry.key) });
    out.push(Request::Save { record: Record::Made { key: entry.key, resources: entry.effect.resources, state } });
    out.push(Request::Outcome { entry: number, outcome: Outcome::Made { state, found } });
}

fn settle_failed(domain: &mut Domain, number: u64, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.remove(&number) else {
        return;
    };
    domain.outbox.runtime.remove(&number);
    out.push(Request::Erase { key: RecordKey::Outbox(entry.key) });
    out.push(Request::Outcome { entry: number, outcome: Outcome::Failed });
}

pub(crate) fn applied(domain: &mut Domain, number: u64, attempt: u32, result: ApplyResult, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.get(&number) else {
        return;
    };
    let Some(last) = entry.attempt else {
        return;
    };
    if attempt > last.number {
        return;
    }
    let recovery = kind(&domain.config, entry.effect.kind).expect("kept effect kind remains configured").recovery;
    let target = entry.effect.target;
    match result {
        ApplyResult::Made { state } => {
            if state == target {
                settle_made(domain, number, state, attempt < last.number, out);
            } else {
                settle_failed(domain, number, out);
            }
        }
        ApplyResult::Conflict => settle_failed(domain, number, out),
        ApplyResult::Transient => {
            if attempt != last.number || entry.phase == EffectPhase::Held {
                return;
            }
            let entry = domain.outbox.entries.get_mut(&number).expect("entry remains unsettled");
            entry.phase = EffectPhase::Retry;
            out.push(Request::Save { record: Record::Outbox(entry.clone()) });
        }
        ApplyResult::Uncertain => {
            if attempt != last.number || entry.phase == EffectPhase::Held {
                return;
            }
            uncertain(domain, number, recovery, out);
        }
    }
}

pub(crate) fn looked(domain: &mut Domain, env: &Env<Limits>, number: u64, looked: Looked, out: &mut Queue<Request>) {
    let Some(entry) = domain.outbox.entries.get(&number) else {
        return;
    };
    if entry.phase != EffectPhase::Uncertain {
        return;
    }
    let Some(runtime) = domain.outbox.runtime.get_mut(&number) else {
        return;
    };
    if !runtime.checking {
        return;
    }
    runtime.checking = false;
    let recovery = kind(&domain.config, entry.effect.kind).expect("kept effect kind remains configured").recovery;
    let key = entry.key;
    let target = entry.effect.target;
    let condition = entry.effect.condition;
    let due = attempt_due(entry.attempt, env.wall);
    match recovery {
        Recovery::Keyed => {
            if looked.found_key && looked.state == Some(target) {
                settle_made(domain, number, target, true, out);
            } else if looked.found_key {
                settle_failed(domain, number, out);
            } else if due {
                start_attempt(domain, env, number, out);
            }
        }
        Recovery::Conditional => {
            if looked.owner == Some(key) && looked.state == Some(target) {
                settle_made(domain, number, target, true, out);
            } else if looked.state == Some(target) || looked.state != condition {
                settle_failed(domain, number, out);
            } else if due {
                start_attempt(domain, env, number, out);
            }
        }
        Recovery::Idempotent | Recovery::Unrecoverable => {}
    }
}

pub(crate) fn next_deadline(domain: &Domain) -> Option<Wall> {
    let mut earliest: Option<Wall> = None;
    for (number, entry) in &domain.outbox.entries {
        let Some(runtime) = domain.outbox.runtime.get(number) else {
            continue;
        };
        if !runtime.released || runtime.checking || !first_on_resources(domain, *number, &entry.effect.resources) {
            continue;
        }
        let due = match entry.phase {
            EffectPhase::Kept => Some(Wall::EPOCH),
            EffectPhase::Sent | EffectPhase::Retry | EffectPhase::Uncertain => {
                if runtime.needs_lookup {
                    Some(Wall::EPOCH)
                } else {
                    match entry.attempt {
                        Some(attempt) => Some(attempt.deadline),
                        None => Some(Wall::EPOCH),
                    }
                }
            }
            EffectPhase::Held => None,
        };
        if let Some(due) = due {
            earliest = Some(match earliest {
                Some(old) => old.min(due),
                None => due,
            });
        }
    }
    earliest
}

pub(crate) fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let mut chosen = None;
    for (number, entry) in &domain.outbox.entries {
        let Some(runtime) = domain.outbox.runtime.get(number) else {
            continue;
        };
        if !runtime.released || runtime.checking || !first_on_resources(domain, *number, &entry.effect.resources) {
            continue;
        }
        let due = match entry.phase {
            EffectPhase::Kept => true,
            EffectPhase::Sent | EffectPhase::Retry | EffectPhase::Uncertain => {
                runtime.needs_lookup
                    || match entry.attempt {
                        Some(attempt) => attempt.deadline <= env.wall,
                        None => true,
                    }
            }
            EffectPhase::Held => false,
        };
        if due {
            chosen = Some(*number);
            break;
        }
    }
    if let Some(number) = chosen {
        progress(domain, env, number, out);
    }
}
