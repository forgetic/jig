//! Connector reports drive named hold admission (domain/connectors.md, 3.3;
//! domain/tasks.md, 6.2). An unreported batch retains its reply and bounded
//! payload without creating tasks or reserving funding. Current project roles
//! narrow every effect and workspace check (domain/authority.md, 6). Reports retry the
//! complete batch against current state. Uncommitted requests are retried by
//! their sender after restart; this table is never restored from the store.
use crate::{Core, Event, Limits, connector};
use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_tasks as tasks;
use skein_lib::{Env, List, Queue, ReplyTo, Token};

/// An uncommitted whole batch, awaiting connector reports.
#[derive(Debug)]
pub(crate) struct Batch {
    pub reply_to: ReplyTo,
    pub kind: Kind,
    pub members: Box<[tasks::New]>,
}

/// The task admission continuation retained with a pending batch.
#[derive(Debug)]
pub(crate) enum Kind {
    /// A root, delegate or ordinary accepted proposal.
    Creation { creator: tasks::Party },
    /// An accepted result proposal belonging to a closing task.
    Result { proposer: u64, proposal: u64 },
}

impl Kind {
    fn event(self, reply_to: ReplyTo, batch: Box<[tasks::New]>) -> tasks::Event {
        match self {
            Kind::Creation { creator } => tasks::Event::Make { reply_to, creator, batch },
            Kind::Result { proposer, proposal } => {
                tasks::Event::MakeResultFollowups { reply_to, proposer, proposal, batch }
            }
        }
    }
}

fn reported(core: &Core, batch: &[tasks::New]) -> bool {
    for member in batch {
        for holding in &member.holdings {
            let name = match holding {
                tasks::Holding::Write { resource, .. } => resource,
                tasks::Holding::Slot { pool, .. } => pool,
            };
            if core.tasks.resource_hold(name).is_none() {
                return false;
            }
        }
    }
    true
}

pub(crate) fn defer(
    core: &mut Core,
    env: &Env<Limits>,
    reply_to: ReplyTo,
    kind: Kind,
    batch: Box<[tasks::New]>,
    out: &mut Queue<tasks::Request>,
) -> Option<tasks::Event> {
    let reported = reported(core, &batch);
    if !tasks::bounded_batch(&env.limits.tasks, &batch) {
        if reported {
            return Some(kind.event(reply_to, batch));
        }
        out.push(tasks::Request::Refused { reply_to, problem: tasks::Problem::new(None, tasks::Refusal::Busy) });
        return None;
    }
    for member in &batch {
        for holding in &member.holdings {
            let name = match holding {
                tasks::Holding::Write { resource, .. } => resource,
                tasks::Holding::Slot { pool, .. } => pool,
            };
            let role = core.resource_roles.get(&Key { project: member.project, name: name.clone() });
            if role == Some(&connector::ResourceRole::Unavailable) || role.is_none() && core.resource_roles_full {
                out.push(tasks::Request::Refused {
                    reply_to,
                    problem: tasks::Problem::new(Some(member.number), tasks::Refusal::ResourceUnavailable),
                });
                return None;
            }
        }
    }
    if reported {
        let mut members = List::with_capacity(env.limits.tasks.batch);
        for mut member in batch {
            let mut holdings = List::with_capacity(env.limits.tasks.holdings);
            for holding in member.holdings {
                let (name, kind) = match holding {
                    tasks::Holding::Write { resource, kind } => (resource, kind),
                    tasks::Holding::Slot { pool, kind } => (pool, kind),
                };
                match core.tasks.resource_hold(&name).expect("every resource reported") {
                    tasks::HoldKind::Shared => {}
                    tasks::HoldKind::Exclusive { .. } => {
                        holdings.push(tasks::Holding::Write { resource: name, kind }).expect("bounded holdings");
                    }
                    tasks::HoldKind::Pooled { .. } => {
                        holdings.push(tasks::Holding::Slot { pool: name, kind }).expect("bounded holdings");
                    }
                }
            }
            member.holdings = holdings.into_boxed();
            members.push(member).expect("bounded batch");
        }
        return Some(kind.event(reply_to, members.into_boxed()));
    }
    if core.awaiting_resources.len() == core.awaiting_resources.capacity() {
        out.push(tasks::Request::Refused { reply_to, problem: tasks::Problem::new(None, tasks::Refusal::Busy) });
        return None;
    }
    let token = reply_to.into_token();
    let batch = Batch { reply_to: ReplyTo::new(token), kind, members: batch };
    let _previous = core.awaiting_resources.insert(token, batch).expect("pending batch room checked");
    None
}

pub(crate) fn resume(core: &mut Core, work: &mut Queue<Event>) {
    // Resume one whole batch per report; later decisions check readiness again.
    let mut ready: Option<Token> = None;
    for (token, batch) in &core.awaiting_resources {
        if reported(core, &batch.members) {
            ready = Some(*token);
            break;
        }
    }
    if let Some(token) = ready {
        let batch = core.awaiting_resources.remove(&token).expect("ready pending batch");
        work.push(Event::Tasks(batch.kind.event(batch.reply_to, batch.members)));
    }
}

/// A role belongs to one project, even when projects name the same object.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Key {
    pub project: u32,
    pub name: tasks::Name,
}

/// Remember current adoption facts. At capacity, unknown names fail closed;
/// a cached narrowing is never discarded to make room for another name.
pub(crate) fn role(core: &mut Core, limits: &Limits, project: u32, name: tasks::Name, role: connector::ResourceRole) {
    if !tasks::valid_name(&limits.tasks, &name) {
        return;
    }
    let key = Key { project, name };
    if role == connector::ResourceRole::Owned && !core.resource_roles_full {
        core.resource_roles.remove(&key);
        return;
    }
    if core.resource_roles.insert(key, role).is_err() {
        core.resource_roles_full = true;
    }
}

fn access(
    core: &Core,
    connector: u16,
    project: u32,
    name: &authority::Name,
    described: authority::EffectAccess,
) -> authority::EffectAccess {
    let key = Key { project, name: tasks::Name { connector, path: name.segments.clone() } };
    match core.resource_roles.get(&key) {
        Some(connector::ResourceRole::Participant) => match described {
            authority::EffectAccess::Owned | authority::EffectAccess::Participant => {
                authority::EffectAccess::Participant
            }
            authority::EffectAccess::Context | authority::EffectAccess::Unavailable => described,
        },
        Some(connector::ResourceRole::Context) => match described {
            authority::EffectAccess::Unavailable => described,
            authority::EffectAccess::Owned
            | authority::EffectAccess::Participant
            | authority::EffectAccess::Context => authority::EffectAccess::Context,
        },
        Some(connector::ResourceRole::Unavailable) => authority::EffectAccess::Unavailable,
        None if core.resource_roles_full => authority::EffectAccess::Unavailable,
        Some(connector::ResourceRole::Owned) | None => described,
    }
}

/// Apply current adoption without widening the connector's own fresh report.
pub(crate) fn effect(core: &Core, project: u32, mut effect: authority::Effect) -> authority::Effect {
    effect.access = access(core, effect.connector, project, &effect.name, effect.access);
    for resource in &mut effect.additional {
        resource.access = access(core, effect.connector, project, &resource.name, resource.access);
    }
    effect
}

/// Translate adoption's access to the task child's hold and writer checks.
pub(crate) fn task_access(role: connector::ResourceRole) -> tasks::ResourceAccess {
    match role {
        connector::ResourceRole::Owned | connector::ResourceRole::Participant => tasks::ResourceAccess::Writable,
        connector::ResourceRole::Context => tasks::ResourceAccess::Context,
        connector::ResourceRole::Unavailable => tasks::ResourceAccess::Unavailable,
    }
}

impl Core {
    /// Restore a connector's durable adoption before tasks open. This restores
    /// report state only; it cannot decide a hold or stop a live activation.
    #[must_use]
    pub fn restore_resource_role(
        &mut self,
        env: &Env<Limits>,
        project: u32,
        name: tasks::Name,
        role: connector::ResourceRole,
    ) -> bool {
        match self.restart_step() {
            Some(crate::RestartStep::RestoreConnector { connector }) if connector == name.connector => {}
            Some(
                crate::RestartStep::LoadCore
                | crate::RestartStep::RestoreConnector { .. }
                | crate::RestartStep::AdoptRuns
                | crate::RestartStep::ReadAfresh { .. }
                | crate::RestartStep::SettleOutbox { .. }
                | crate::RestartStep::Open,
            )
            | None => return false,
        }
        if !tasks::valid_name(&env.limits.tasks, &name) {
            return false;
        }
        self::role(self, &env.limits, project, name.clone(), role);
        let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
        tasks::step(
            &mut self.tasks,
            &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
            tasks::Event::ResourceAccess { project, name, access: task_access(role) },
            &mut out,
        );
        out.is_empty() && !self.resource_roles_full
    }
}
