//! What the fleet relays between the parent and the runs (domain/engine.md,
//! section 8; domain/hosts.md, 4.2): inbound events down, a run's host calls
//! up and their answers back, its bounces and its facts up. Each goes through
//! only while its attempt is the parent's live claim; what comes for an
//! attempt cancelled, replaced, lost or answered is dropped (attempts are
//! fenced), and an inbound event that reaches no worker goes back to the
//! parent, which keeps it.
//! Worker inputs also require the live claim's current hosting channel;
//! foreign channels and channels lost or replaced cannot originate them.
//!
//! A relayed call is the parent's to answer, exactly once: the fleet keeps it
//! until the parent does, whatever becomes of its attempt meanwhile, and
//! passes the answer down only if the attempt is still the live claim, on a
//! worker in contact. A call beyond the room for calls is dropped, and its
//! run withdraws it past its own deadline.

use alloc::boxed::Box;
use core::mem;

use skein_lib::{Env, Id, Queue, ReplyTo, Token};

use crate::attempt::{Attempt, State, Where};
use crate::boundary::{Bounce, Call, Message, Request, Undelivered};
use crate::channel;
use crate::domain::Domain;
use crate::facts::Fact;

/// A relayed call the parent has yet to answer.
#[derive(PartialEq, Eq, Hash, Debug)]
pub(crate) struct PendingCall {
    attempt: Id<Attempt>,
    /// The worker's name for it, kept until the parent's answer.
    name: Box<[u8]>,
}

/// Where the parent's live claim is, as far as relaying goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Live {
    /// On the worker of this channel, in contact.
    On(Token),
    /// Its worker's channel was lost.
    Adrift,
    /// The attempt is not the parent's live claim.
    Not,
}

/// Where the attempt `id` is, if it is the parent's live claim.
fn live(domain: &Domain, id: Id<Attempt>) -> Live {
    let Some(entry) = domain.attempts.get(id) else {
        return Live::Not;
    };
    match &entry.state {
        State::Claimed { at: Where::On(channel), .. } => Live::On(channel::token(&domain.channels, *channel)),
        State::Claimed { at: Where::Adrift { .. }, .. } => Live::Adrift,
        State::Waiting { .. }
        | State::Adopted { .. }
        | State::Cancelled { .. }
        | State::Handed { .. }
        | State::Acknowledged { .. }
        | State::Stray { .. }
        | State::Kept { .. }
        | State::Fenced { .. }
        | State::Closed => Live::Not,
    }
}

/// Whether the attempt `attempt` of `run` is the parent's live claim, and
/// which it is.
fn named(domain: &Domain, run: Token, attempt: Token) -> Option<Id<Attempt>> {
    let &id = domain.names.get(&(run, attempt))?;
    match live(domain, id) {
        Live::On(_) | Live::Adrift => Some(id),
        Live::Not => None,
    }
}

/// Only the worker currently hosting the live claim may originate inputs.
fn owned(domain: &Domain, channel: Token, run: Token, attempt: Token) -> Option<Id<Attempt>> {
    let id = named(domain, run, attempt)?;
    match live(domain, id) {
        Live::On(held) if held == channel => Some(id),
        Live::On(_) | Live::Adrift | Live::Not => None,
    }
}

/// A typed call is passed through whole, with only its bounded name retained
/// for the answer. A rejected call is returned whole to its parent.
pub(crate) fn relay(
    domain: &mut Domain,
    env: &Env<crate::limits::Limits>,
    channel: Token,
    run: Token,
    attempt: Token,
    call: Call,
    out: &mut Queue<Request>,
) {
    let Some(id) = owned(domain, channel, run, attempt) else {
        domain.facts.push(Fact::Dropped);
        out.push(Request::DropCall { call });
        return;
    };
    if u64::try_from(call.name.len()).expect("a length fits") > env.limits.call_name_bytes {
        domain.facts.push(Fact::Dropped);
        out.push(Request::DropCall { call });
        return;
    }
    let name = call.name.clone();
    match domain.calls.insert(PendingCall { attempt: id, name }) {
        Ok(id) => out.push(Request::Relay { reply_to: ReplyTo::new(id.token()), run, attempt, call }),
        Err(_) => {
            domain.facts.push(Fact::Dropped);
            out.push(Request::DropCall { call });
        }
    }
}

/// The parent's answer to a relayed call: passed down to the run's worker,
/// if its attempt is still the live claim and on a worker in contact.
pub(crate) fn relayed(domain: &mut Domain, to: ReplyTo, answer: Token, out: &mut Queue<Request>) {
    let id = Id::<PendingCall>::from_token(to.into_token());
    let entry = domain.calls.get_mut(id).expect("the parent answers a call in flight, which is kept until it does");
    let owner = entry.attempt;
    let name = mem::replace(&mut entry.name, Box::new([]));
    domain.calls.retire(id);
    match live(domain, owner) {
        Live::On(channel) => {
            let entry = domain.attempts.get(owner).expect("a live claim is tracked");
            out.push(Request::Relayed { channel, run: entry.run, attempt: entry.token, call: name, answer });
        }
        Live::Adrift | Live::Not => {
            domain.facts.push(Fact::Dropped);
            out.push(Request::Drop { payload: answer });
        }
    }
}

/// A typed message follows the same host fence as an opaque inbound event.
pub(crate) fn inbound(domain: &mut Domain, run: Token, attempt: Token, message: Message, out: &mut Queue<Request>) {
    let undelivered = match domain.names.get(&(run, attempt)) {
        Some(&id) => match &domain.attempts.get(id).expect("a named attempt is tracked").state {
            State::Claimed { at: Where::On(channel), .. } => {
                let channel = channel::token(&domain.channels, *channel);
                out.push(Request::Inbound { channel, run, attempt, message });
                return;
            }
            State::Waiting { .. } => Undelivered::Unplaced,
            State::Adopted { .. } | State::Claimed { at: Where::Adrift { .. }, .. } => Undelivered::Adrift,
            State::Cancelled { .. }
            | State::Handed { .. }
            | State::Acknowledged { .. }
            | State::Stray { .. }
            | State::Kept { .. }
            | State::Fenced { .. } => Undelivered::Gone,
            State::Closed => unreachable!("a closed attempt is no longer named"),
        },
        None => Undelivered::Gone,
    };
    out.push(Request::Undelivered { run, attempt, message, undelivered });
}

/// A bounce, from a worker: up to the parent, if its attempt is the live
/// claim.
pub(crate) fn bounced(
    domain: &mut Domain,
    channel: Token,
    run: Token,
    attempt: Token,
    name: Token,
    bounce: Bounce,
    out: &mut Queue<Request>,
) {
    if owned(domain, channel, run, attempt).is_some() {
        out.push(Request::Bounced { run, attempt, name, bounce });
    } else {
        domain.facts.push(Fact::Dropped);
    }
}

/// A fact a run told, from its worker: up to the parent, if its attempt is
/// the live claim.
pub(crate) fn told(
    domain: &mut Domain,
    channel: Token,
    run: Token,
    attempt: Token,
    fact: Token,
    out: &mut Queue<Request>,
) {
    if owned(domain, channel, run, attempt).is_some() {
        out.push(Request::Told { run, attempt, fact });
    } else {
        domain.facts.push(Fact::Dropped);
        out.push(Request::Drop { payload: fact });
    }
}

/// A grant follows only the live claimed attempt's worker.
pub(crate) fn grant(domain: &mut Domain, run: Token, attempt: Token, grant: crate::Grant, out: &mut Queue<Request>) {
    let Some(id) = named(domain, run, attempt) else {
        return;
    };
    match live(domain, id) {
        Live::On(channel) => out.push(Request::Grant { channel, run, attempt, grant }),
        Live::Adrift | Live::Not => {}
    }
}

/// Credential notices require the live claim's current hosting channel.
pub(crate) fn credential_notice(
    domain: &mut Domain,
    channel: Token,
    run: Token,
    attempt: Token,
    notice: Request,
    out: &mut Queue<Request>,
) {
    if owned(domain, channel, run, attempt).is_some() {
        out.push(notice);
    } else {
        domain.facts.push(Fact::Dropped);
    }
}
