//! The attempts the fleet tracks (domain/engine.md, 4.2 and section 8): each
//! the parent's claim on a run, from its start or its adoption to the
//! parent's acknowledgement of its answer; or one a worker hosts or holds the
//! answer of that the parent has not claimed.
//!
//! A run's attempts share its workstream (the workstream is the task number, and a
//! run is named by its item), so no attempt of a run is placed while a worker
//! may host another: an attempt replaced, cancelled or found unclaimed holds
//! its run's next one back until its worker answers it, or past the grace
//! of its worker's lost channel, when the worker has cancelled it itself.
//! What a worker sends for an attempt that is not the parent's live claim is
//! dropped (attempts are fenced): only a cancelled attempt's answer still
//! ends its call.
//!
//! An answer is acknowledged to its worker only once the parent has made it
//! durable (its `Acknowledge`), or once it is for an attempt fenced off; until
//! then the worker keeps it, and its slot, and sends it again after every
//! hello, and the fleet drops what it sends again unacknowledged. A refusal
//! keeps nothing: a busy one places the attempt again, and an invalid one is
//! handed on and forgotten.
//!
//! An attempt's transition table. "On" is on the channel of a worker in
//! contact, which it takes a slot of; "adrift", its worker's channel lost,
//! kept until the grace passes. A listing is a hello's, "answered" when the
//! worker holds the attempt's answer, which follows the hello.
//!
//! ```text
//! state         event or alarm      next              emits
//! (none)        start               Waiting           (a newer claim replaces its run's claim)
//!               adopt               Adopted           (likewise)
//!               listed, its run     Fenced on         cancel, unless answered
//!                 claimed
//!               listed              Stray on          listed
//! Waiting       placed (resume)     Claimed on        assign; placed
//!               listed              Claimed on        placed (it is hosted already)
//!               cancel              (gone)            withdrawn: cancelled
//!               replaced            (gone)            withdrawn: replaced
//! Adopted       listed              Claimed on        placed
//!               cancel              Cancelled adrift
//!               replaced            Fenced adrift     withdrawn: replaced
//!               answer              Handed on         answered
//!               grace               (gone)            lost
//! Claimed       listed              Claimed on        (moved)
//!               cancel              Cancelled         cancel, if on
//!               replaced            Fenced            cancel, if on; withdrawn: replaced
//!               answer              Handed on         answered
//!               answer: invalid     (gone)            answered
//!               answer: busy        Waiting           (placed again; its worker takes no more)
//!               channel lost        Claimed adrift
//!               grace (adrift)      (gone)            lost
//! Cancelled     listed              Cancelled on      cancel, unless answered
//!               cancel              Cancelled
//!               replaced            Fenced            withdrawn: replaced
//!               answer              Handed on         answered
//!               answer: invalid     (gone)            answered
//!               answer: busy        (gone)            withdrawn: cancelled
//!               channel lost        Cancelled adrift
//!               grace (adrift)      (gone)            lost
//! Handed        acknowledge, on     (gone)            acknowledge
//!               acknowledge, adrift Acknowledged
//!               listed              Handed on         (moved)
//!               answer              Handed            drop (sent again)
//!               channel lost        Handed adrift
//! Acknowledged  listed              (gone)            acknowledge
//!               grace               (gone)
//! Stray         listed              Stray on          (moved)
//!               adopt               Claimed           placed
//!               cancel              Fenced            cancel, if on
//!               answer              Kept on
//!               channel lost        Stray adrift
//!               loaded              Stray             (its deadline: the grace from now)
//!               its deadline        Fenced            cancel, if on
//! Kept          adopt               Handed            answered
//!               cancel              (gone)            acknowledge, if on; drop the answer
//!                                   or Fenced adrift
//!               its deadline        likewise
//!               listed              Kept on           (moved)
//!               answer              Kept              drop (sent again)
//!               channel lost        Kept adrift
//!               loaded              Kept              (its deadline: the grace from now)
//! Fenced        listed              Fenced on         cancel, unless answered
//!               answer              (gone)            acknowledge; drop
//!               cancel              Fenced
//!               channel lost        Fenced adrift
//!               grace (adrift)      (gone)
//! ```
//!
//! An answer from a channel not in contact is dropped unacknowledged. One for
//! an attempt the fleet does not know (acknowledged already, or never made)
//! is acknowledged and dropped; one for an attempt waiting to be placed is
//! dropped. Strays and kept answers wait for the parent's `Loaded` before
//! their deadline runs, the grace from then or from their first listing,
//! whichever is later. Only the parent's claims (Waiting, Adopted, Claimed,
//! Cancelled) are replaced: a stray of the run is left to its deadline.
//!
//! What a state implies (the slot it takes, whether its run's next attempt
//! waits for it, its place in the queue for a slot, whether it is its run's
//! claim, and its alarm) is derived from it in one place after every
//! transition ([`follow`]).

use core::mem;

use skein_lib::{Env, Id, Queue, ReplyTo, Slab, Time, Token};

use crate::boundary::{Answer, HostKind, Kinds, Refusal, Request, Withdrawal};
use crate::channel::{self, Channel};
use crate::domain::Domain;
use crate::facts::{Fact, Facts};
use crate::limits::Limits;

/// An attempt the fleet tracks.
#[derive(Debug)]
pub(crate) struct Attempt {
    /// Its run's name, and its own, as the parent and the workers give them.
    pub(crate) run: Token,
    pub(crate) token: Token,
    /// Whether a worker's listing made it, in the room kept for listings, or
    /// the parent's start or adoption, in the room for claims.
    pub(crate) listed: bool,
    /// The contiguous committed turn prefix, restored atomically on adoption.
    pub(crate) kept: u32,
    pub(crate) state: State,
}

/// What the fleet knows of a run: its claim, and how many of its attempts a
/// worker may host.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Run {
    /// The attempt the parent claims it by, whose call is open.
    pub(crate) claim: Option<Id<Attempt>>,
    /// Its attempts a worker may host. Its claim waits for a slot until none
    /// is.
    pub(crate) held: u32,
}

#[derive(Debug)]
pub(crate) enum State {
    /// Started, or refused as busy: waits for a slot, and for no worker to
    /// host another attempt of its run. `serial` is its place in the queue.
    Waiting { to: ReplyTo, workstream: u64, kinds: Kinds, serial: u64 },
    /// Adopted after a restart, no worker having listed it yet: until the
    /// grace passes.
    Adopted { to: ReplyTo, until: Time },
    /// The parent's live claim, on a worker or adrift, with its workstream to
    /// be placed again if its worker refuses it as busy (empty when
    /// adopted).
    Claimed { to: ReplyTo, at: Where, workstream: u64, kinds: Kinds },
    /// Cancelled by the parent: its answer still ends the call.
    Cancelled { to: ReplyTo, at: Where },
    /// Its answer handed to the parent, which has yet to acknowledge it: its
    /// worker keeps it, and its slot.
    Handed { at: Where },
    /// Its answer acknowledged by the parent while its worker was out of
    /// contact: acknowledged to the worker once a hello lists it, until the
    /// grace passes.
    Acknowledged { until: Time },
    /// Listed by a worker, and not claimed: it waits to be adopted until
    /// `until`, which runs once the parent has loaded its claims.
    Stray { at: Where, until: Option<Time> },
    /// A stray's answer, kept for its adoption until `until`, as a stray
    /// waits: its worker keeps it, and its slot.
    Kept { answer: Answer, payload: Token, at: Where, until: Option<Time> },
    /// Cancelled for good, replaced or not adopted in time: whatever comes
    /// for it is dropped. It holds its run's next attempt back while a
    /// worker may host it.
    Fenced { at: Where },
    /// Terminal: holds nothing.
    Closed,
}

/// Where an attempt out on a worker is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Where {
    /// On the worker of this channel, taking one of its slots.
    On(Id<Channel>),
    /// Its worker's channel was lost: kept until the grace passes.
    Adrift { until: Time },
}

/// What a state implies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Implied {
    /// The channel whose slot it takes.
    on: Option<Id<Channel>>,
    /// Whether a worker may host it, live.
    held: bool,
    /// Its place in the queue for a slot.
    queued: Option<u64>,
    claim: bool,
    /// When its alarm falls due.
    deadline: Option<Time>,
    closed: bool,
}

/// What nothing implies: an attempt not tracked yet.
const UNTRACKED: Implied = Implied { on: None, held: false, queued: None, claim: false, deadline: None, closed: false };

/// What `state` implies.
pub(crate) fn implied(state: &State) -> Implied {
    let none = UNTRACKED;
    match state {
        State::Waiting { serial, .. } => Implied { queued: Some(*serial), claim: true, ..none },
        State::Adopted { until, .. } => Implied { held: true, claim: true, deadline: Some(*until), ..none },
        State::Claimed { at, .. } | State::Cancelled { at, .. } => {
            Implied { on: on(*at), held: true, claim: true, deadline: until(*at), ..none }
        }
        State::Handed { at } => Implied { on: on(*at), ..none },
        State::Acknowledged { until } => Implied { deadline: Some(*until), ..none },
        State::Stray { at, until } => Implied { on: on(*at), held: true, deadline: *until, ..none },
        State::Kept { at, until, .. } => Implied { on: on(*at), deadline: *until, ..none },
        State::Fenced { at } => Implied { on: on(*at), held: true, deadline: until(*at), ..none },
        State::Closed => Implied { closed: true, ..none },
    }
}

fn on(at: Where) -> Option<Id<Channel>> {
    match at {
        Where::On(channel) => Some(channel),
        Where::Adrift { .. } => None,
    }
}

fn until(at: Where) -> Option<Time> {
    match at {
        Where::On(_) => None,
        Where::Adrift { until } => Some(until),
    }
}

/// The names a request about an attempt carries.
#[derive(Clone, Copy, Debug)]
struct Names {
    run: Token,
    attempt: Token,
}

// Entry points: one per event, alarm or resume that concerns an attempt.

/// Start: refused at the entrance, or queued for a slot, its run's claim
/// replaced.
pub(crate) fn start(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    run: Token,
    attempt: Token,
    workstream: u64,
    kinds: Kinds,
    out: &mut Queue<Request>,
) {
    let refusal = if workstream == 0 {
        Some(Refusal::Workstream)
    } else if domain.names.contains_key(&(run, attempt)) {
        Some(Refusal::Duplicate)
    } else if !has_room(domain, env) {
        Some(Refusal::Busy)
    } else {
        None
    };
    if let Some(refusal) = refusal {
        refuse(domain, to, Names { run, attempt }, refusal, out);
        return;
    }
    replace(domain, run, out);
    let serial = next_serial(&mut domain.serial);
    insert(domain, run, attempt, false, State::Waiting { to, workstream, kinds, serial });
}

/// Adopt: a stray claimed, a kept answer handed over, or a claim adrift
/// until a worker lists it; refused if the fleet has it already, and lost if
/// it was fenced off.
pub(crate) fn adopt(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    run: Token,
    attempt: Token,
    kept: u32,
    out: &mut Queue<Request>,
) {
    let names = Names { run, attempt };
    let Some(&id) = domain.names.get(&(run, attempt)) else {
        if !has_room(domain, env) {
            refuse(domain, to, names, Refusal::Busy, out);
            return;
        }
        replace(domain, run, out);
        let until = env.now.saturating_add(env.limits.grace);
        insert(domain, run, attempt, false, State::Adopted { to, until });
        let id = *domain.names.get(&(run, attempt)).expect("inserted above");
        domain.attempts.get_mut(id).expect("inserted above").kept = kept;
        return;
    };
    let entry = domain.attempts.get(id).expect("a named attempt is tracked");
    match &entry.state {
        State::Stray { .. } | State::Kept { .. } => {}
        State::Waiting { .. }
        | State::Adopted { .. }
        | State::Claimed { .. }
        | State::Cancelled { .. }
        | State::Handed { .. }
        | State::Acknowledged { .. } => {
            refuse(domain, to, names, Refusal::Duplicate, out);
            return;
        }
        // Cancelled for good: as far as the parent can tell, lost.
        State::Fenced { .. } => {
            domain.facts.push(Fact::PresumedLost);
            out.push(Request::Lost { to, run, attempt });
            return;
        }
        State::Closed => unreachable!("a closed attempt is no longer named"),
    }
    replace(domain, run, out);
    let entry = domain.attempts.get_mut(id).expect("looked up above");
    entry.kept = kept;
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Stray { at, until: _ } => located(to, at, 0, Kinds::Workers, names, &mut domain.facts, out),
        State::Kept { answer, payload, at, until: _ } => handed(to, answer, payload, at, names, &mut domain.facts, out),
        State::Waiting { .. }
        | State::Adopted { .. }
        | State::Claimed { .. }
        | State::Cancelled { .. }
        | State::Handed { .. }
        | State::Acknowledged { .. }
        | State::Fenced { .. }
        | State::Closed => unreachable!("only a stray or a kept answer is adopted, as matched above"),
    };
    follow(domain, id, before);
}

/// Cancel, from the parent. An attempt the fleet no longer tracks, or whose
/// answer it has handed on, has answered, been lost or withdrawn, or was
/// never made: nothing to cancel.
pub(crate) fn cancel(domain: &mut Domain, run: Token, attempt: Token, out: &mut Queue<Request>) {
    let Some(&id) = domain.names.get(&(run, attempt)) else {
        return;
    };
    let names = Names { run, attempt };
    let entry = domain.attempts.get_mut(id).expect("a named attempt is tracked");
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Waiting { to, workstream: _, kinds: _, serial: _ } => withdrawn(to, Withdrawal::Cancelled, names, out),
        State::Adopted { to, until } => State::Cancelled { to, at: Where::Adrift { until } },
        State::Claimed { to, at, workstream: _, kinds: _ } => cancelled(to, at, names, &domain.channels, out),
        State::Cancelled { to, at } => State::Cancelled { to, at },
        State::Handed { at } => State::Handed { at },
        State::Acknowledged { until } => State::Acknowledged { until },
        State::Stray { at, until: _ } => fenced(at, names, &domain.channels, &mut domain.facts, out),
        State::Kept { answer: _, payload, at, until: _ } => {
            forgotten(payload, at, names, &domain.channels, &mut domain.facts, out)
        }
        State::Fenced { at } => State::Fenced { at },
        State::Closed => unreachable!("a closed attempt is no longer named"),
    };
    follow(domain, id, before);
}

/// Acknowledge, from the parent: the answer it was handed is durable, or not
/// wanted, and its worker may forget it. One for an attempt the fleet no
/// longer tracks (a refusal's, or one whose grace passed) changes nothing.
pub(crate) fn acknowledge(domain: &mut Domain, run: Token, attempt: Token, out: &mut Queue<Request>) {
    let Some(&id) = domain.names.get(&(run, attempt)) else {
        return;
    };
    let names = Names { run, attempt };
    let entry = domain.attempts.get_mut(id).expect("a named attempt is tracked");
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Handed { at } => match at {
            Where::On(channel) => acknowledged(channel, names, &domain.channels, out),
            Where::Adrift { until } => State::Acknowledged { until },
        },
        State::Waiting { .. }
        | State::Adopted { .. }
        | State::Claimed { .. }
        | State::Cancelled { .. }
        | State::Acknowledged { .. }
        | State::Stray { .. }
        | State::Kept { .. }
        | State::Fenced { .. } => unreachable!("the parent acknowledges only an answer it was handed, once"),
        State::Closed => unreachable!("a closed attempt is no longer named"),
    };
    follow(domain, id, before);
}

/// Placed, from the ready list: the waiting attempt `id` is assigned to the
/// worker of `channel`, which has a free slot.
pub(crate) fn place(
    domain: &mut Domain,
    env: &Env<Limits>,
    id: Id<Attempt>,
    channel: Id<Channel>,
    out: &mut Queue<Request>,
) {
    let entry = domain.attempts.get_mut(id).expect("a queued attempt is tracked");
    let names = Names { run: entry.run, attempt: entry.token };
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Waiting { to, workstream, kinds, serial: _ } => {
            assigned(to, workstream, kinds, channel, names, env, &mut domain.channels, &mut domain.facts, out)
        }
        State::Adopted { .. }
        | State::Claimed { .. }
        | State::Cancelled { .. }
        | State::Handed { .. }
        | State::Acknowledged { .. }
        | State::Stray { .. }
        | State::Kept { .. }
        | State::Fenced { .. }
        | State::Closed => unreachable!("only a waiting attempt is queued for a slot"),
    };
    follow(domain, id, before);
}

/// Listed by the hello of the worker of `channel`: what it hosts is kept,
/// cancelled again, acknowledged or found.
pub(crate) fn listed(
    domain: &mut Domain,
    env: &Env<Limits>,
    channel: Id<Channel>,
    run: Token,
    attempt: Token,
    answered: bool,
    out: &mut Queue<Request>,
) {
    let names = Names { run, attempt };
    let Some(&id) = domain.names.get(&(run, attempt)) else {
        found(domain, env, channel, names, answered, out);
        return;
    };
    let on = Where::On(channel);
    let entry = domain.attempts.get_mut(id).expect("a named attempt is tracked");
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Waiting { to, workstream, kinds, serial: _ } => {
            hosted(to, workstream, kinds, channel, names, env, &mut domain.channels, &mut domain.facts, out)
        }
        State::Adopted { to, until: _ } => located(to, on, 0, Kinds::Workers, names, &mut domain.facts, out),
        State::Claimed { to, at, workstream, kinds } => moved(to, at, workstream, kinds, channel, &mut domain.facts),
        State::Cancelled { to, at: _ } => {
            again(on, answered, names, &domain.channels, out);
            State::Cancelled { to, at: on }
        }
        State::Handed { at: _ } => State::Handed { at: on },
        State::Acknowledged { until: _ } => acknowledged(channel, names, &domain.channels, out),
        State::Stray { at: _, until } => State::Stray { at: on, until },
        State::Kept { answer, payload, at: _, until } => State::Kept { answer, payload, at: on, until },
        State::Fenced { at: _ } => {
            again(on, answered, names, &domain.channels, out);
            State::Fenced { at: on }
        }
        State::Closed => unreachable!("a closed attempt is no longer named"),
    };
    follow(domain, id, before);
}

/// An answer, on the channel `channel`. Dropped unacknowledged if the channel
/// is not in contact; otherwise handed to the parent once if the attempt is
/// its claim, kept if it is a stray, and acknowledged and dropped if it is
/// fenced off or not tracked.
pub(crate) fn answer(
    domain: &mut Domain,
    channel: Token,
    run: Token,
    attempt: Token,
    answer: Answer,
    payload: Token,
    out: &mut Queue<Request>,
) {
    let Some(&from) = domain.tokens.get(&channel) else {
        domain.facts.push(Fact::Dropped);
        out.push(Request::Drop { payload });
        return;
    };
    let names = Names { run, attempt };
    let Some(&id) = domain.names.get(&(run, attempt)) else {
        // Acknowledged already, its acknowledgement lost with a channel; or
        // never made.
        domain.facts.push(Fact::Duplicate);
        out.push(Request::Acknowledge { channel, run, attempt });
        out.push(Request::Drop { payload });
        return;
    };
    let on = Where::On(from);
    let entry = domain.attempts.get_mut(id).expect("a named attempt is tracked");
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Waiting { to, workstream, kinds, serial } => {
            resent(payload, &mut domain.facts, out);
            State::Waiting { to, workstream, kinds, serial }
        }
        State::Adopted { to, until } => match answer {
            Answer::Busy => {
                resent(payload, &mut domain.facts, out);
                State::Adopted { to, until }
            }
            Answer::Ended | Answer::Parked | Answer::Failed | Answer::Invalid => {
                handed(to, answer, payload, on, names, &mut domain.facts, out)
            }
        },
        State::Claimed { to, at: _, workstream, kinds } => match answer {
            Answer::Busy => {
                refused(payload, &mut domain.facts, out);
                let serial = next_serial(&mut domain.serial);
                State::Waiting { to, workstream, kinds, serial }
            }
            Answer::Ended | Answer::Parked | Answer::Failed | Answer::Invalid => {
                handed(to, answer, payload, on, names, &mut domain.facts, out)
            }
        },
        State::Cancelled { to, at: _ } => match answer {
            Answer::Busy => {
                refused(payload, &mut domain.facts, out);
                withdrawn(to, Withdrawal::Cancelled, names, out)
            }
            Answer::Ended | Answer::Parked | Answer::Failed | Answer::Invalid => {
                handed(to, answer, payload, on, names, &mut domain.facts, out)
            }
        },
        State::Handed { at } => {
            resent(payload, &mut domain.facts, out);
            State::Handed { at }
        }
        State::Stray { at: _, until } => State::Kept { answer, payload, at: on, until },
        State::Kept { answer: kept, payload: held, at, until } => {
            resent(payload, &mut domain.facts, out);
            State::Kept { answer: kept, payload: held, at, until }
        }
        // Acknowledged by the parent, or fenced off: its worker forgets it.
        State::Acknowledged { until: _ } | State::Fenced { at: _ } => {
            domain.facts.push(Fact::Duplicate);
            out.push(Request::Acknowledge { channel, run, attempt });
            out.push(Request::Drop { payload });
            State::Closed
        }
        State::Closed => unreachable!("a closed attempt is no longer named"),
    };
    follow(domain, id, before);
    match answer {
        // After the attempt has left its slot, which would lift the draining.
        Answer::Busy => channel::drain(domain, from),
        Answer::Ended | Answer::Parked | Answer::Failed | Answer::Invalid => {}
    }
}

/// Its worker's channel was lost: the attempt `id` is kept until `until`.
pub(crate) fn adrift(domain: &mut Domain, id: Id<Attempt>, until: Time) {
    let entry = domain.attempts.get_mut(id).expect("an attempt on a worker is tracked");
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    let at = Where::Adrift { until };
    entry.state = match state {
        State::Claimed { to, at: _, workstream, kinds } => State::Claimed { to, at, workstream, kinds },
        State::Cancelled { to, at: _ } => State::Cancelled { to, at },
        State::Handed { at: _ } => State::Handed { at },
        State::Stray { at: _, until } => State::Stray { at, until },
        State::Kept { answer, payload, at: _, until } => State::Kept { answer, payload, at, until },
        State::Fenced { at: _ } => State::Fenced { at },
        State::Waiting { .. } | State::Adopted { .. } | State::Acknowledged { .. } | State::Closed => {
            unreachable!("only an attempt on a worker takes its slot")
        }
    };
    follow(domain, id, before);
}

/// The alarm of the attempt `id`: the grace of its lost channel, or of its
/// adoption, passed; or a stray's or a kept answer's deadline.
pub(crate) fn expire(domain: &mut Domain, id: Id<Attempt>, out: &mut Queue<Request>) {
    let entry = domain.attempts.get_mut(id).expect("an alarm names a tracked attempt");
    let names = Names { run: entry.run, attempt: entry.token };
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Adopted { to, until: _ }
        | State::Claimed { to, at: Where::Adrift { .. }, .. }
        | State::Cancelled { to, at: Where::Adrift { .. } } => presumed(to, names, &mut domain.facts, out),
        State::Stray { at, until: Some(_) } => fenced(at, names, &domain.channels, &mut domain.facts, out),
        State::Kept { answer: _, payload, at, until: Some(_) } => {
            forgotten(payload, at, names, &domain.channels, &mut domain.facts, out)
        }
        State::Acknowledged { until: _ } | State::Fenced { at: Where::Adrift { .. } } => State::Closed,
        State::Waiting { .. }
        | State::Claimed { at: Where::On(_), .. }
        | State::Cancelled { at: Where::On(_), .. }
        | State::Handed { .. }
        | State::Stray { until: None, .. }
        | State::Kept { until: None, .. }
        | State::Fenced { at: Where::On(_) }
        | State::Closed => unreachable!("no alarm runs in this state"),
    };
    follow(domain, id, before);
}

/// Loaded: the deadline of every stray and kept answer waiting for it runs
/// from now. Only their deadlines change, so their alarms are armed here.
pub(crate) fn loaded(domain: &mut Domain, env: &Env<Limits>) {
    domain.loaded = true;
    let at = env.now.saturating_add(env.limits.grace);
    for (_, &id) in &domain.names {
        let entry = domain.attempts.get_mut(id).expect("a named attempt is tracked");
        let waits = match &entry.state {
            State::Stray { until: None, .. } | State::Kept { until: None, .. } => true,
            State::Waiting { .. }
            | State::Adopted { .. }
            | State::Claimed { .. }
            | State::Cancelled { .. }
            | State::Handed { .. }
            | State::Acknowledged { .. }
            | State::Stray { until: Some(_), .. }
            | State::Kept { until: Some(_), .. }
            | State::Fenced { .. }
            | State::Closed => false,
        };
        if !waits {
            continue;
        }
        let state = mem::replace(&mut entry.state, State::Closed);
        entry.state = match state {
            State::Stray { at: on, until: _ } => State::Stray { at: on, until: Some(at) },
            State::Kept { answer, payload, at: on, until: _ } => {
                State::Kept { answer, payload, at: on, until: Some(at) }
            }
            State::Waiting { .. }
            | State::Adopted { .. }
            | State::Claimed { .. }
            | State::Cancelled { .. }
            | State::Handed { .. }
            | State::Acknowledged { .. }
            | State::Fenced { .. }
            | State::Closed => unreachable!("only a stray or a kept answer waits for the load, as matched above"),
        };
        let room = domain.alarms.arm(id, at);
        assert!(room.is_ok(), "an alarm per attempt");
    }
}

/// Resumes the ready list: the first attempt in the queue whose run no
/// worker hosts another of, for which a worker has a free slot, is placed.
/// With none, placement waits for something to change.
pub(crate) fn resume(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let mut chosen = None;
    for (_, &id) in &domain.waiting {
        let entry = domain.attempts.get(id).expect("a queued attempt is tracked");
        let held = match domain.runs.get(&entry.run) {
            Some(run) => run.held,
            None => unreachable!("a queued attempt is its run's claim"),
        };
        if held > 0 {
            continue;
        }
        let workstream = match &entry.state {
            State::Waiting { workstream, .. } => workstream,
            State::Adopted { .. }
            | State::Claimed { .. }
            | State::Cancelled { .. }
            | State::Handed { .. }
            | State::Acknowledged { .. }
            | State::Stray { .. }
            | State::Kept { .. }
            | State::Fenced { .. }
            | State::Closed => unreachable!("only a waiting attempt is queued for a slot"),
        };
        let kinds = match &entry.state {
            State::Waiting { kinds, .. } => *kinds,
            _ => unreachable!("only a waiting attempt is queued"),
        };
        if let Some(channel) = channel::choose(domain, *workstream, kinds) {
            chosen = Some((id, channel));
            break;
        }
    }
    match chosen {
        Some((id, channel)) => place(domain, env, id, channel, out),
        None => domain.placing = false,
    }
}

// Cell handlers: each takes the source state's data by value and returns the
// target state.

/// Whether there is room for another of the parent's attempts: within its
/// share, and in the table.
fn has_room(domain: &Domain, env: &Env<Limits>) -> bool {
    domain.claims < env.limits.attempts && !domain.attempts.is_full()
}

/// The next place in the queue for a slot.
fn next_serial(serial: &mut u64) -> u64 {
    let this = *serial;
    *serial = this.checked_add(1).expect("a u64 counts every placing");
    this
}

/// Refused at the entrance.
fn refuse(domain: &mut Domain, to: ReplyTo, names: Names, refusal: Refusal, out: &mut Queue<Request>) {
    domain.facts.push(Fact::Refused { refusal });
    out.push(Request::Refused { to, run: names.run, attempt: names.attempt, refusal });
}

/// A newer claim of `run`: its claim, if it has one, is withdrawn, and fenced
/// if a worker may host it.
fn replace(domain: &mut Domain, run: Token, out: &mut Queue<Request>) {
    let claim = match domain.runs.get(&run) {
        Some(entry) => entry.claim,
        None => None,
    };
    let Some(id) = claim else {
        return;
    };
    let entry = domain.attempts.get_mut(id).expect("a run's claim is tracked");
    let names = Names { run, attempt: entry.token };
    let before = implied(&entry.state);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Waiting { to, workstream: _, kinds: _, serial: _ } => withdrawn(to, Withdrawal::Replaced, names, out),
        State::Adopted { to, until } => superseded(to, Where::Adrift { until }, names, out),
        State::Claimed { to, at, workstream: _, kinds: _ } => {
            again(at, false, names, &domain.channels, out);
            superseded(to, at, names, out)
        }
        State::Cancelled { to, at } => superseded(to, at, names, out),
        State::Handed { .. }
        | State::Acknowledged { .. }
        | State::Stray { .. }
        | State::Kept { .. }
        | State::Fenced { .. }
        | State::Closed => unreachable!("a run's claim is the parent's"),
    };
    follow(domain, id, before);
}

/// Waiting, cancelled or replaced: its call ends, and nothing of it is left.
fn withdrawn(to: ReplyTo, withdrawal: Withdrawal, names: Names, out: &mut Queue<Request>) -> State {
    out.push(Request::Withdrawn { to, run: names.run, attempt: names.attempt, withdrawal });
    State::Closed
}

/// A claim on a worker, or maybe on one, replaced: its call ends, and it is
/// fenced off while a worker may host it.
fn superseded(to: ReplyTo, at: Where, names: Names, out: &mut Queue<Request>) -> State {
    out.push(Request::Withdrawn { to, run: names.run, attempt: names.attempt, withdrawal: Withdrawal::Replaced });
    State::Fenced { at }
}

/// Claimed, cancel: the cancel goes to its worker now, or once a hello
/// lists it.
fn cancelled(to: ReplyTo, at: Where, names: Names, channels: &Slab<Channel>, out: &mut Queue<Request>) -> State {
    again(at, false, names, channels, out);
    State::Cancelled { to, at }
}

/// A stray cancelled, or not adopted in time: fenced off, and cancelled on
/// its worker.
fn fenced(at: Where, names: Names, channels: &Slab<Channel>, facts: &mut Facts, out: &mut Queue<Request>) -> State {
    facts.push(Fact::Fenced);
    again(at, false, names, channels, out);
    State::Fenced { at }
}

/// Sends the attempt's cancel to its worker, if it is on one and has not
/// answered.
fn again(at: Where, answered: bool, names: Names, channels: &Slab<Channel>, out: &mut Queue<Request>) {
    match at {
        Where::On(channel) if !answered => {
            let channel = channel::token(channels, channel);
            out.push(Request::Cancel { channel, run: names.run, attempt: names.attempt });
        }
        Where::On(_) | Where::Adrift { .. } => {}
    }
}

/// The parent has the answer durably: its worker forgets it.
fn acknowledged(channel: Id<Channel>, names: Names, channels: &Slab<Channel>, out: &mut Queue<Request>) -> State {
    let channel = channel::token(channels, channel);
    out.push(Request::Acknowledge { channel, run: names.run, attempt: names.attempt });
    State::Closed
}

/// A kept answer no one adopts: the parent forgets it, and its worker too
/// if it is in contact; otherwise once a hello lists it, as for any attempt
/// fenced off.
fn forgotten(
    payload: Token,
    at: Where,
    names: Names,
    channels: &Slab<Channel>,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> State {
    facts.push(Fact::Forgotten);
    out.push(Request::Drop { payload });
    match at {
        Where::On(channel) => acknowledged(channel, names, channels, out),
        Where::Adrift { .. } => State::Fenced { at },
    }
}

/// An answer sent again while the fleet keeps or has handed on the first, or
/// for an attempt waiting to be placed: dropped, not acknowledged.
fn resent(payload: Token, facts: &mut Facts, out: &mut Queue<Request>) {
    facts.push(Fact::Duplicate);
    out.push(Request::Drop { payload });
}

/// A busy refusal: the parent forgets its payload, and the attempt goes on.
fn refused(payload: Token, facts: &mut Facts, out: &mut Queue<Request>) {
    facts.push(Fact::Busy);
    out.push(Request::Drop { payload });
}

/// The answer goes to the parent, ending its call; its worker keeps it until
/// the parent acknowledges it, unless it is a refusal, which keeps nothing.
fn handed(
    to: ReplyTo,
    answer: Answer,
    payload: Token,
    at: Where,
    names: Names,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> State {
    facts.push(Fact::Answered { answer });
    out.push(Request::Answered { to, run: names.run, attempt: names.attempt, answer, payload });
    match answer {
        Answer::Invalid => State::Closed,
        Answer::Ended | Answer::Parked | Answer::Failed => State::Handed { at },
        Answer::Busy => unreachable!("a busy refusal is placed again, not handed on"),
    }
}

/// Past the grace, no worker hosting it in contact: presumed lost.
fn presumed(to: ReplyTo, names: Names, facts: &mut Facts, out: &mut Queue<Request>) -> State {
    facts.push(Fact::PresumedLost);
    out.push(Request::Lost { to, run: names.run, attempt: names.attempt });
    State::Closed
}

/// An adopted attempt found, or a stray adopted: the parent's claim from now
/// on, which the parent hears is on a worker.
fn located(
    to: ReplyTo,
    at: Where,
    workstream: u64,
    kinds: Kinds,
    names: Names,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> State {
    facts.push(Fact::Found);
    out.push(Request::Placed { run: names.run, attempt: names.attempt });
    State::Claimed { to, at, workstream, kinds }
}

/// A claim listed again: on its worker's channel from now on.
fn moved(to: ReplyTo, at: Where, workstream: u64, kinds: Kinds, channel: Id<Channel>, facts: &mut Facts) -> State {
    match at {
        Where::On(_) => {}
        Where::Adrift { .. } => facts.push(Fact::Found),
    }
    State::Claimed { to, at: Where::On(channel), workstream, kinds }
}

/// Waiting, placed: assigned to the worker of `channel`, which holds its
/// workstream from now on.
#[expect(clippy::too_many_arguments, reason = "a cell handler takes the fields it touches")]
fn assigned(
    to: ReplyTo,
    workstream: u64,
    kinds: Kinds,
    channel: Id<Channel>,
    names: Names,
    env: &Env<Limits>,
    channels: &mut Slab<Channel>,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> State {
    let entry = channels.get_mut(channel).expect("a worker chosen is in contact");
    if entry.kind == HostKind::Worker {
        channel::cache(entry, &env.limits, workstream);
    }
    out.push(Request::Assign { channel: entry.token, kind: entry.kind, run: names.run, attempt: names.attempt });
    out.push(Request::Placed { run: names.run, attempt: names.attempt });
    facts.push(Fact::Placed);
    State::Claimed { to, at: Where::On(channel), workstream, kinds }
}

/// Waiting, listed: a worker hosts it already, so it is not assigned again.
#[expect(clippy::too_many_arguments, reason = "a cell handler takes the fields it touches")]
fn hosted(
    to: ReplyTo,
    workstream: u64,
    kinds: Kinds,
    channel: Id<Channel>,
    names: Names,
    env: &Env<Limits>,
    channels: &mut Slab<Channel>,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> State {
    let entry = channels.get_mut(channel).expect("a worker saying hello is in contact");
    channel::cache(entry, &env.limits, workstream);
    located(to, Where::On(channel), workstream, kinds, names, facts, out)
}

/// Listed, and not tracked: fenced off at once if its run is claimed by
/// another attempt, and otherwise a stray that waits to be adopted, which
/// the parent hears of. Beyond the room kept for listings, it is cancelled
/// and not tracked.
fn found(
    domain: &mut Domain,
    env: &Env<Limits>,
    channel: Id<Channel>,
    names: Names,
    answered: bool,
    out: &mut Queue<Request>,
) {
    let at = Where::On(channel);
    if domain.attempts.is_full() {
        domain.facts.push(Fact::Fenced);
        again(at, answered, names, &domain.channels, out);
        return;
    }
    let claimed = match domain.runs.get(&names.run) {
        Some(run) => run.claim.is_some(),
        None => false,
    };
    let state = if claimed {
        domain.facts.push(Fact::Fenced);
        again(at, answered, names, &domain.channels, out);
        State::Fenced { at }
    } else {
        domain.facts.push(Fact::Stray);
        out.push(Request::Listed { run: names.run, attempt: names.attempt });
        let until = if domain.loaded { Some(env.now.saturating_add(env.limits.grace)) } else { None };
        State::Stray { at, until }
    };
    insert(domain, names.run, names.attempt, true, state);
}

// What a state implies, in one place.

/// Tracks a new attempt in `state`. The entrance checked there is room.
fn insert(domain: &mut Domain, run: Token, attempt: Token, listed: bool, state: State) {
    let Ok(id) = domain.attempts.insert(Attempt { run, token: attempt, listed, kept: 0, state }) else {
        unreachable!("the entrance checks there is room for an attempt");
    };
    let named = domain.names.insert((run, attempt), id);
    assert!(named == Ok(None), "an attempt tracked is named once, with room for every one");
    if !listed {
        domain.claims = domain.claims.checked_add(1).expect("no more claims than tracked");
    }
    follow(domain, id, UNTRACKED);
}

/// Derives what the new state of the attempt `id` implies, given what its
/// state `before` did: the slot it takes (a worker that frees one takes
/// assignments again), its run's claim and how many of its attempts a worker
/// may host, its place in the queue, its alarm; and retires it once it has
/// closed.
fn follow(domain: &mut Domain, id: Id<Attempt>, before: Implied) {
    domain.turning = !domain.turns.is_empty();
    let entry = domain.attempts.get(id).expect("an attempt lives until it is reclaimed");
    let after = implied(&entry.state);
    let run = entry.run;
    let attempt = entry.token;
    let listed = entry.listed;
    if before.on != after.on {
        if let Some(channel) = before.on {
            // Lost already, if it went adrift with its channel.
            if let Some(entry) = domain.channels.get_mut(channel) {
                entry.hosts.remove(&id);
                entry.draining = false;
            }
            domain.placing = true;
        }
        if let Some(channel) = after.on {
            let entry = domain.channels.get_mut(channel).expect("an attempt is on a worker in contact");
            let room = entry.hosts.insert(id);
            assert!(room.is_ok(), "a worker hosts no more attempts than its listings or its slots take");
        }
    }
    if before.queued != after.queued {
        if let Some(serial) = before.queued {
            domain.waiting.remove(&serial);
        }
        if let Some(serial) = after.queued {
            let room = domain.waiting.insert(serial, id);
            assert!(room.is_ok(), "the queue has room for every attempt");
        }
        domain.placing = true;
    }
    let mut entry = match domain.runs.get(&run) {
        Some(entry) => *entry,
        None => Run { claim: None, held: 0 },
    };
    if before.held != after.held {
        entry.held = if after.held {
            entry.held.checked_add(1).expect("no more attempts held than tracked")
        } else {
            domain.placing = true;
            entry.held.checked_sub(1).expect("an attempt held was counted")
        };
    }
    if after.claim {
        entry.claim = Some(id);
    } else if before.claim && entry.claim == Some(id) {
        entry.claim = None;
    }
    if entry.claim.is_none() && entry.held == 0 {
        domain.runs.remove(&run);
    } else {
        let room = domain.runs.insert(run, entry);
        assert!(room.is_ok(), "a run tracked has an attempt tracked");
    }
    match after.deadline {
        Some(at) => {
            let room = domain.alarms.arm(id, at);
            assert!(room.is_ok(), "an alarm per attempt");
        }
        None => domain.alarms.cancel(id),
    }
    if after.closed {
        domain.names.remove(&(run, attempt));
        domain.attempts.retire(id);
        if !listed {
            domain.claims = domain.claims.checked_sub(1).expect("a claim tracked was counted");
        }
    }
}
