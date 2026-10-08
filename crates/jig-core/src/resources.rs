//! Connector reports drive named hold admission (domain/connectors.md, 3.3;
//! domain/tasks.md, 6.2). An unreported batch retains its reply and bounded
//! payload without creating tasks or reserving funding. Reports retry the
//! complete batch against current state. Uncommitted requests are retried by
//! their sender after restart; this table is never restored from the store.
use crate::{Core, Event, Limits};
use alloc::boxed::Box;
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
    if reported && !tasks::bounded_batch(&env.limits.tasks, &batch) {
        return Some(kind.event(reply_to, batch));
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
    if !tasks::bounded_batch(&env.limits.tasks, &batch)
        || core.awaiting_resources.len() == core.awaiting_resources.capacity()
    {
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
