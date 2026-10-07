//! Ephemeral reads, brief sections and workspace items (domain/connectors.md,
//! sections 9 and 10). A token owns exactly one value until handover or drop.
use alloc::boxed::Box;
use skein_lib::{Env, List, Queue, Token};

use crate::domain::{Domain, resource_spec};
use crate::{Item, Limits, Path, Read, Record, Request};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Payload {
    Section(Box<[u8]>),
    Items(Box<[Item]>),
}

fn encoded(domain: &Domain, resource: &Path, task: u64, size: u32) -> Box<[u8]> {
    let state = match crate::requirements::current(domain, resource) {
        Some(fact) => fact.state.unwrap_or(0),
        None => 0,
    };
    let result = domain.results.get(&task).copied().unwrap_or(0);
    let mut bytes: List<u8> = List::with_capacity(18);
    for byte in task.to_be_bytes() {
        bytes.push(byte).expect("fixed encoding fits");
    }
    for byte in state.to_be_bytes() {
        bytes.push(byte).expect("fixed encoding fits");
    }
    for byte in result.to_be_bytes() {
        bytes.push(byte).expect("fixed encoding fits");
    }
    let bytes = bytes.into_boxed();
    let len = usize::try_from(size).unwrap_or(usize::MAX).min(bytes.len());
    Box::from(bytes.get(..len).expect("length capped by encoding"))
}

pub(crate) fn read(domain: &Domain, env: &Env<Limits>, token: Token, read: Read, out: &mut Queue<Request>) {
    if resource_spec(&domain.config, &read.resource).is_none() || read.size > env.limits.value_bytes {
        out.push(Request::Answer { token, bytes: Box::from([]) });
    } else {
        out.push(Request::Answer { token, bytes: encoded(domain, &read.resource, 0, read.size) });
    }
}

pub(crate) fn gather(
    domain: &mut Domain,
    env: &Env<Limits>,
    token: Token,
    task: u64,
    budget: u32,
    out: &mut Queue<Request>,
) {
    let Some(Record::Task { resources, .. }) = domain.tasks.get(&task) else {
        out.push(Request::Ready { token, size: 0 });
        return;
    };
    let Some(resource) = resources.first() else {
        out.push(Request::Ready { token, size: 0 });
        return;
    };
    if domain.values.len() >= env.limits.values && !domain.values.contains_key(&token) {
        out.push(Request::Ready { token, size: 0 });
        return;
    }
    let bytes = encoded(domain, resource, task, budget.min(env.limits.value_bytes));
    let size = u32::try_from(bytes.len()).expect("encoded section is at most 18 bytes");
    domain.values.insert(token, Payload::Section(bytes)).expect("value capacity checked");
    out.push(Request::Ready { token, size });
}

pub(crate) fn cut(domain: &mut Domain, token: Token, size: u32, out: &mut Queue<Request>) {
    let Some(Payload::Section(bytes)) = domain.values.get_mut(&token) else {
        out.push(Request::Ready { token, size: 0 });
        return;
    };
    let length = usize::try_from(size).unwrap_or(usize::MAX).min(bytes.len());
    *bytes = Box::from(bytes.get(..length).expect("length capped by section"));
    out.push(Request::Ready { token, size: u32::try_from(length).expect("section was bounded") });
}

pub(crate) fn items(domain: &mut Domain, env: &Env<Limits>, token: Token, task: u64, out: &mut Queue<Request>) {
    let Some(Record::Task { resources, project, .. }) = domain.tasks.get(&task) else {
        out.push(Request::Ready { token, size: 0 });
        return;
    };
    if domain.values.len() >= env.limits.values && !domain.values.contains_key(&token) {
        out.push(Request::Ready { token, size: 0 });
        return;
    }
    let mut listed: List<Item> = List::with_capacity(env.limits.resources_per_task);
    for resource in resources {
        let Some(spec) = resource_spec(&domain.config, resource) else {
            continue;
        };
        let role = domain.adoptions.get(&(*project, resource.clone()));
        let writable = spec.writable
            && match role {
                Some(role) => *role != crate::ResourceRole::Context,
                None => false,
            };
        let state = match crate::requirements::current(domain, resource) {
            Some(fact) => fact.state,
            None => None,
        };
        listed.push(Item { resource: resource.clone(), writable, state }).expect("task names were bounded");
    }
    let size = listed.len();
    domain.values.insert(token, Payload::Items(listed.into_boxed())).expect("value capacity checked");
    out.push(Request::Ready { token, size });
}

pub(crate) fn hand_over(domain: &mut Domain, token: Token, out: &mut Queue<Request>) {
    match domain.values.remove(&token) {
        Some(Payload::Section(bytes)) => out.push(Request::Section { token, bytes }),
        Some(Payload::Items(items)) => out.push(Request::Workspace { token, items }),
        None => {}
    }
}

pub(crate) fn drop_value(domain: &mut Domain, token: Token) {
    domain.values.remove(&token);
}
