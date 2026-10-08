//! Durable, exclusive writer slots for held resources (domain/tasks.md, 6.3).
//! The hub knows only opaque names and fences; connectors own fresh reads.
use crate::domain::{Domain, record, refused};
use crate::{Limits, Name, Party, Refusal, Request, Stored, Writer, WriterSlot};
use skein_lib::{Env, Queue, ReplyTo};

fn covers(domain: &Domain, task: u64, resource: &Name, limit: u32) -> bool {
    let Some(holder) = crate::holds::writer_holder(domain, resource) else { return false };
    below(domain, task, holder, limit)
}

fn below(domain: &Domain, task: u64, ancestor: u64, limit: u32) -> bool {
    let mut current = Some(task);
    for _ in 0..limit {
        let Some(at) = current else { break };
        if at == ancestor {
            return true;
        }
        current = match record(domain, at) {
            Some(row) => match row.requester {
                Party::Task(parent) => Some(parent),
                Party::Person(_) | Party::Deployment { .. } => None,
            },
            None => None,
        };
    }
    false
}

fn take(domain: &mut Domain, resource: Name, writer: Writer, out: &mut Queue<Request>) {
    let number = domain.next_writer.checked_add(1).expect("writer number fits");
    domain.next_writer = number;
    let slot = WriterSlot { number, resource: resource.clone(), writer, lost: false };
    assert!(domain.writers.insert(resource, slot.clone()).is_ok(), "preflighted writer capacity");
    out.push(Request::Save { record: Stored::Writer(slot) });
}

fn free(domain: &mut Domain, env: &Env<Limits>, resource: &Name, out: &mut Queue<Request>) {
    if let Some(slot) = domain.writers.remove(resource) {
        out.push(Request::Erase { key: crate::Key::Writer(slot.number) });
        // A hold handed downward can wait for its ancestor's run writer.
        // Ordinary delegate results and effect settlements have their own wake
        // routes, after the connector has received their outcome.
        if let Some(holder) = crate::holds::writer_holder(domain, resource) {
            match slot.writer {
                Writer::Run { task, .. } => {
                    if holder != task && below(domain, holder, task, env.limits.depth.saturating_add(1)) {
                        crate::domain::wake_procedure(domain, env, holder, out);
                    }
                }
                Writer::Effect { .. } => {}
            }
        }
    }
}

pub(crate) fn claim(
    domain: &mut Domain,
    limits: &Limits,
    task: u64,
    attempt: u64,
    writes: &[Name],
    out: &mut Queue<Request>,
) -> Result<(), Option<Name>> {
    if writes.len() > usize::try_from(limits.holdings).expect("u32 fits usize") {
        return Err(None);
    }
    for (index, resource) in writes.iter().enumerate() {
        let mut duplicate = false;
        for prior in writes.iter().take(index) {
            if prior == resource {
                duplicate = true;
                break;
            }
        }
        if !crate::holds::valid_name(limits, resource)
            || !covers(domain, task, resource, limits.depth.saturating_add(1))
            || duplicate
        {
            return Err(None);
        }
        if domain.writers.contains_key(resource) {
            return Err(Some(resource.clone()));
        }
    }
    if domain.writers.len().saturating_add(u32::try_from(writes.len()).unwrap_or(u32::MAX)) > domain.writers.capacity()
    {
        return Err(None);
    }
    for resource in writes {
        take(domain, resource.clone(), Writer::Run { task, attempt }, out);
    }
    Ok(())
}

pub(crate) fn answered(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    attempt: u64,
    lost: bool,
    out: &mut Queue<Request>,
) {
    let mut resources = skein_lib::List::with_capacity(domain.writers.capacity());
    for (resource, slot) in &domain.writers {
        if slot.writer == (Writer::Run { task, attempt }) {
            resources.push(resource.clone()).expect("bounded writer names");
        }
    }
    for resource in &resources {
        if lost {
            let slot = domain.writers.get_mut(resource).expect("collected writer");
            slot.lost = true;
            out.push(Request::Save { record: Stored::Writer(slot.clone()) });
        } else {
            free(domain, env, resource, out);
        }
    }
}

pub(crate) fn read_afresh(domain: &mut Domain, env: &Env<Limits>, resource: &Name, out: &mut Queue<Request>) {
    if match domain.writers.get(resource) {
        Some(slot) => slot.lost,
        None => false,
    } {
        free(domain, env, resource, out);
    }
}

/// Pure whole-set preflight; a repeated procedure entry keeps its own slot.
pub(crate) fn effect_ready(domain: &Domain, limits: &Limits, task: u64, resource: &Name, entry: Option<u64>) -> bool {
    if !crate::holds::valid_name(limits, resource) || !covers(domain, task, resource, limits.depth.saturating_add(1)) {
        return false;
    }
    match domain.writers.get(resource) {
        Some(slot) => match slot.writer {
            Writer::Effect { entry: current } => entry == Some(current),
            Writer::Run { .. } => false,
        },
        None => domain.writers.len() < domain.writers.capacity(),
    }
}

pub(crate) fn effect_in_flight(
    domain: &mut Domain,
    limits: &Limits,
    to: ReplyTo,
    task: u64,
    resource: Name,
    entry: u64,
    out: &mut Queue<Request>,
) {
    if !crate::holds::valid_name(limits, &resource)
        || !covers(domain, task, &resource, limits.depth.saturating_add(1))
        || entry == 0
    {
        refused(to, Some(task), Refusal::Holds, out);
    } else if let Some(slot) = domain.writers.get(&resource) {
        if slot.writer == (Writer::Effect { entry }) {
            out.push(Request::Done { reply_to: to });
        } else {
            out.push(Request::WriterWaiting { reply_to: to, task: Some(task), resource });
        }
    } else if domain.writers.len() == domain.writers.capacity() {
        refused(to, Some(task), Refusal::Holds, out);
    } else {
        take(domain, resource, Writer::Effect { entry }, out);
        out.push(Request::Done { reply_to: to });
    }
}

pub(crate) fn effect_settled(
    domain: &mut Domain,
    env: &Env<Limits>,
    resource: &Name,
    entry: u64,
    out: &mut Queue<Request>,
) {
    if match domain.writers.get(resource) {
        Some(slot) => slot.writer == Writer::Effect { entry },
        None => false,
    } {
        free(domain, env, resource, out);
    }
}

pub(crate) fn restore(domain: &mut Domain, limits: &Limits, slot: WriterSlot) -> bool {
    if slot.number == 0
        || !crate::holds::valid_name(limits, &slot.resource)
        || domain.writers.contains_key(&slot.resource)
        || domain.writers.len() == domain.writers.capacity()
    {
        return false;
    }
    domain.next_writer = domain.next_writer.max(slot.number);
    domain.writers.insert(slot.resource.clone(), slot).is_ok()
}

pub(crate) fn valid_restored(domain: &Domain, limit: u32) -> bool {
    for (resource, slot) in &domain.writers {
        if slot.resource != *resource {
            return false;
        }
        match slot.writer {
            Writer::Run { task, attempt } => {
                if attempt == 0 || !covers(domain, task, resource, limit) {
                    return false;
                }
                let Some(row) = record(domain, task) else { return false };
                if row.attempt != attempt || (row.last_answer == Some(attempt)) != slot.lost {
                    return false;
                }
            }
            Writer::Effect { entry } => {
                if entry == 0 || slot.lost || crate::holds::writer_holder(domain, resource).is_none() {
                    return false;
                }
            }
        }
    }
    true
}
