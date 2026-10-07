//! The workers in contact (domain/engine.md, sections 2 and 8): each known by
//! its channel, from the hello it says first on it until the channel is lost.
//! A worker that comes back dials a new channel, and is known again by what
//! its hello lists: the fleet needs no other name for it.
//!
//! A hello is the worker's entrance. It is turned away, its channel to be
//! closed, if the fleet has no room for another worker or its declared stop
//! bound is not strictly below the engine's grace. Its slots are
//! used up to the limit, and the workstreams it lists up to the limit, each
//! a task number. Each run it lists, up to the slots a worker
//! may have, is kept, cancelled again, acknowledged or found (see the
//! attempts' table); one beyond them, or beyond the room the fleet keeps for
//! listings, is cancelled and not tracked; and past twice the slots, a
//! listing is dropped and counted, to bound what a hello emits. A second
//! hello on a channel breaks the worker's contract, and is counted and
//! dropped.
//!
//! A lost channel's attempts are kept for the grace, adrift, and the worker
//! is forgotten. A worker that refuses an assignment as busy is shutting
//! down, or fuller than the fleet knew: nothing more is placed on it until
//! it frees a slot.
//!
//! Placement takes a worker in contact with a free slot that holds the
//! workstream's workspace, the first in the order of the protocol's names for
//! the channels; or, with none, the one with the most free slots. A worker
//! holds the workstreams its hello listed and those of the runs placed on it
//! since: placing one beyond the room evicts the one used longest ago.

use core::mem;

use skein_lib::{Env, Id, Map, Queue, Set, Slab, Token};

use crate::attempt::{self, Attempt};
use crate::boundary::{Hello, Hosted, Phase, Request};
use crate::domain::Domain;
use crate::facts::Fact;
use crate::limits::Limits;

/// A worker in contact.
#[derive(Debug)]
pub(crate) struct Channel {
    /// The protocol's name for its channel.
    pub(crate) token: Token,
    /// How many runs it hosts at once, as its hello said, within the limits.
    pub(crate) slots: u32,
    /// It refused an assignment as busy, and has freed no slot since:
    /// nothing is placed on it.
    pub(crate) draining: bool,
    /// The attempts it hosts or holds the answers of, each taking a slot.
    pub(crate) hosts: Set<Id<Attempt>>,
    /// The workstreams it holds workspaces for, by when each was last used,
    /// and the count that orders them.
    pub(crate) workstreams: Map<u64, u64>,
    pub(crate) uses: u64,
}

/// The protocol's name for the channel `channel`, which is in contact.
pub(crate) fn token(channels: &Slab<Channel>, channel: Id<Channel>) -> Token {
    channels.get(channel).expect("an attempt is on a worker in contact").token
}

/// Whether `channel` holds `workstream`, and when it last used it.
fn holds(channel: &Channel, workstream: u64) -> Option<u64> {
    for (&used, key) in &channel.workstreams {
        if *key == workstream {
            return Some(used);
        }
    }
    None
}

/// `channel` holds `workstream` from now on, its latest used: in
/// place of the one used longest ago if it holds as many as it may. Task number zero is not kept.
pub(crate) fn cache(channel: &mut Channel, limits: &Limits, workstream: u64) {
    if workstream == 0 || limits.workstreams == 0 {
        return;
    }
    let key = match holds(channel, workstream) {
        Some(used) => channel.workstreams.remove(&used).expect("held above"),
        None => {
            if channel.workstreams.len() >= limits.workstreams
                && let Some((&oldest, _)) = channel.workstreams.first()
            {
                channel.workstreams.remove(&oldest);
            }
            workstream
        }
    };
    let used = channel.uses;
    channel.uses = used.checked_add(1).expect("a u64 counts every use");
    let room = channel.workstreams.insert(used, key);
    assert!(room.is_ok(), "room was made above");
}

/// Hello, on a new channel: the worker is in contact, or turned away.
pub(crate) fn hello(domain: &mut Domain, env: &Env<Limits>, channel: Token, hello: Hello, out: &mut Queue<Request>) {
    let limits = &env.limits;
    if domain.tokens.contains_key(&channel) {
        domain.facts.push(Fact::Dropped);
        return;
    }
    let too_long = match hello.graces {
        Some(graces) => graces >= limits.grace,
        None => false,
    };
    if domain.channels.is_full() || too_long {
        domain.facts.push(Fact::TurnedAway);
        out.push(Request::Refuse { channel });
        return;
    }
    let Hello { slots, workstreams, hosting, graces: _ } = hello;
    let mut worker = Channel {
        token: channel,
        slots: slots.min(limits.slots),
        draining: false,
        hosts: Set::with_capacity(limits.slots),
        workstreams: Map::with_capacity(limits.workstreams),
        uses: 0,
    };
    for workstream in &workstreams {
        cache(&mut worker, limits, *workstream);
    }
    let Ok(id) = domain.channels.insert(worker) else {
        unreachable!("checked for room above");
    };
    let named = domain.tokens.insert(channel, id);
    assert!(named == Ok(None), "a channel is named once, with room for every worker");
    let most = limits.slots.saturating_mul(2);
    let mut listed: u32 = 0;
    for Hosted { run, attempt, phase } in hosting {
        let answered = match phase {
            Phase::Answered => true,
            Phase::Preparing | Phase::Starting | Phase::Active | Phase::Waiting | Phase::Ending => false,
        };
        if listed >= most {
            domain.facts.push(Fact::Dropped);
            continue;
        }
        if listed >= limits.slots {
            // More than a worker may host: cancelled, and not tracked.
            domain.facts.push(Fact::Fenced);
            if !answered {
                out.push(Request::Cancel { channel, run, attempt });
            }
        } else {
            attempt::listed(domain, env, id, run, attempt, answered, out);
        }
        listed = listed.saturating_add(1);
    }
    domain.facts.push(Fact::Hello { listed });
    domain.placing = true;
}

/// Lost: the worker's attempts are kept for the grace, adrift. A channel
/// the fleet does not know never said hello, or was turned away.
pub(crate) fn lost(domain: &mut Domain, env: &Env<Limits>, channel: Token) {
    let Some(id) = domain.tokens.remove(&channel) else {
        return;
    };
    let entry = domain.channels.get_mut(id).expect("a named channel is in contact");
    let hosts = mem::replace(&mut entry.hosts, Set::with_capacity(0));
    let until = env.now.saturating_add(env.limits.grace);
    for &attempt in &hosts {
        attempt::adrift(domain, attempt, until);
    }
    domain.channels.retire(id);
    domain.facts.push(Fact::Lost { adrift: hosts.len() });
}

/// The worker of `channel` refused an assignment as busy: nothing more is
/// placed on it until it frees a slot.
pub(crate) fn drain(domain: &mut Domain, channel: Id<Channel>) {
    domain.channels.get_mut(channel).expect("an answer's channel is in contact").draining = true;
}

/// The worker to place an attempt of `workstream` on: one with a free slot
/// that holds its workspace, or else the one with the most free slots.
pub(crate) fn choose(domain: &Domain, workstream: u64) -> Option<Id<Channel>> {
    let mut best: Option<(Id<Channel>, u32)> = None;
    for (_, &id) in &domain.tokens {
        let channel = domain.channels.get(id).expect("a named channel is in contact");
        let free = channel.slots.saturating_sub(channel.hosts.len());
        if channel.draining || free == 0 {
            continue;
        }
        if holds(channel, workstream).is_some() {
            return Some(id);
        }
        best = match best {
            Some((other, most)) if most >= free => Some((other, most)),
            Some(_) | None => Some((id, free)),
        };
    }
    let (id, _) = best?;
    Some(id)
}
