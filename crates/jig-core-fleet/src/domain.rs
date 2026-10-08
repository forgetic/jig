//! The fleet's state and its entry points (programming-model.md, section 3).

use skein_lib::{Deadlines, Env, Id, Map, Queue, Slab, Time, Token};

use crate::attempt::{self, Attempt, Run};
use crate::boundary::{Event, HostKind, Request};
use crate::call::{self, PendingCall};
use crate::channel::{self, Channel};
use crate::facts::{Fact, Facts};
use crate::limits::{self, Limits};
use crate::turn::{self, Pending};

/// The most requests a step, an alarm or a resume emits under `limits`: a
/// request for each run a hello lists, up to twice the slots (a cancel, an
/// acknowledgement, or the parent told an attempt is found or listed); or an
/// adoption's three (the replaced claim's cancel and its withdrawal, and the
/// adopted attempt's placing or answer). The parent reserves this much room
/// in `out` before calling it. A turn emits at most two (busy or
/// acknowledgement, and drop); adoption and cleanup release one held turn
/// per resume, within the same bound.
#[must_use]
pub const fn max_out(limits: &Limits) -> u32 {
    let listed = limits.slots.saturating_mul(2);
    if listed > 3 { listed } else { 3 }
}

/// The fleet's state.
#[derive(Debug)]
pub struct Domain {
    /// The workers in contact, and their channels' names.
    pub(crate) channels: Slab<Channel>,
    pub(crate) tokens: Map<Token, Id<Channel>>,
    /// The attempts tracked, by their run's and their own names.
    pub(crate) attempts: Slab<Attempt>,
    pub(crate) names: Map<(Token, Token), Id<Attempt>>,
    /// The runs of the attempts tracked.
    pub(crate) runs: Map<Token, Run>,
    /// The attempts waiting for a slot, in the order they started, and the
    /// count that orders them.
    pub(crate) waiting: Map<u64, Id<Attempt>>,
    pub(crate) serial: u64,
    /// An alarm per attempt, as its state says.
    pub(crate) alarms: Deadlines<Id<Attempt>>,
    /// Relayed calls the parent has yet to answer.
    pub(crate) calls: Slab<PendingCall>,
    pub(crate) turns: Map<(Id<Attempt>, u32), Pending>,
    pub(crate) turning: bool,
    /// Something changed that may let a waiting attempt be placed: the ready
    /// list.
    pub(crate) placing: bool,
    /// The parent's attempts tracked, within `Limits::attempts`.
    pub(crate) claims: u32,
    /// The parent has loaded its claims: strays' deadlines run.
    pub(crate) loaded: bool,
    pub(crate) facts: Facts,
}

impl Domain {
    /// A domain with room for `limits`.
    #[must_use]
    pub fn new(limits: &Limits) -> Domain {
        let tracked = limits::tracked(limits).expect("worst_case accepted the limits");
        let channels_count =
            limits.workers.checked_add(u32::from(limits.engine_slots > 0)).expect("worst_case accepted the limits");
        let mut channels = Slab::with_capacity(channels_count);
        let mut tokens = Map::with_capacity(channels_count);
        if limits.engine_slots > 0 {
            let engine = Channel {
                kind: HostKind::Engine,
                token: Token::new(0),
                slots: limits.engine_slots,
                draining: false,
                hosts: skein_lib::Set::with_capacity(limits.engine_slots),
                workstreams: Map::with_capacity(0),
                uses: 0,
            };
            let id = channels.insert(engine).expect("engine slot reserved");
            tokens.insert(Token::new(0), id).expect("engine token reserved");
        }
        Domain {
            channels,
            tokens,
            attempts: Slab::with_capacity(tracked),
            names: Map::with_capacity(tracked),
            runs: Map::with_capacity(tracked),
            waiting: Map::with_capacity(tracked),
            serial: 0,
            alarms: Deadlines::with_capacity(tracked),
            calls: Slab::with_capacity(limits.calls),
            turns: Map::with_capacity(limits.turns),
            turning: false,
            placing: false,
            claims: 0,
            loaded: false,
            facts: Facts::with_capacity(limits.facts),
        }
    }

    /// Workers in contact.
    #[must_use]
    pub fn workers(&self) -> u32 {
        self.tokens.len().saturating_sub(u32::from(self.tokens.contains_key(&Token::new(0))))
    }

    /// Attempts tracked, closed ones included until they are reclaimed.
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts.len()
    }

    /// Attempts waiting for a slot.
    #[must_use]
    pub fn waiting(&self) -> u32 {
        self.waiting.len()
    }

    /// Relayed calls the parent has yet to answer, answered ones included
    /// until they are reclaimed.
    #[must_use]
    pub fn calls(&self) -> u32 {
        self.calls.len()
    }

    /// Turns held for adoption or awaiting the parent's commitment.
    #[must_use]
    pub fn turns(&self) -> u32 {
        self.turns.len()
    }

    /// When the earliest alarm falls due.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Time> {
        self.alarms.next()
    }

    /// Whether an alarm is due at `now`. While one is, the loop fires the
    /// root domain, which calls [`fire`].
    #[must_use]
    pub fn is_due(&self, now: Time) -> bool {
        match self.alarms.next() {
            Some(at) => at <= now,
            None => false,
        }
    }

    /// Whether placement or a held turn may make progress. While it may,
    /// the loop resumes the root domain, which calls [`resume`], at the
    /// start of the domain's stage, before its input events.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.placing || self.turning
    }

    /// The oldest fact not yet drained. The parent drains them at its own
    /// pace; what does not fit meanwhile is dropped and counted.
    pub fn pop_fact(&mut self) -> Option<Fact> {
        self.facts.pop()
    }

    /// How many facts were dropped for want of room, since the domain was
    /// made.
    #[must_use]
    pub fn facts_lost(&self) -> u64 {
        self.facts.lost()
    }

    /// The reclaim point: frees what closed in this iteration.
    pub fn reclaim(&mut self) {
        self.channels.reclaim();
        self.attempts.reclaim();
        self.calls.reclaim();
    }
}

/// Handles one event, emitting at most [`max_out`] requests.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::Start { reply_to, run, attempt, workstream, kinds, assignment } => {
            attempt::start(domain, env, reply_to, run, attempt, workstream, kinds, assignment, out);
        }
        Event::Inbound { run, attempt, message } => call::inbound(domain, run, attempt, message, out),
        Event::Relay { channel, run, attempt, call } => {
            call::relay(domain, env, channel, run, attempt, call, out);
        }
        Event::Adopt { reply_to, run, attempt, kept, kind, worked } => match kind {
            HostKind::Engine => {
                if worked {
                    out.push(Request::Lost { to: reply_to, run, attempt });
                } else {
                    out.push(Request::NotStarted { to: reply_to, run, attempt });
                }
            }
            HostKind::Worker => attempt::adopt(domain, env, reply_to, run, attempt, kept, out),
        },
        Event::Cancel { run, attempt } => attempt::cancel(domain, run, attempt, out),
        Event::Relayed { to, answer } => call::relayed(domain, to, answer, out),
        Event::Acknowledge { run, attempt } => attempt::acknowledge(domain, run, attempt, out),
        Event::Grant { run, attempt, grant } => call::grant(domain, run, attempt, grant, out),
        Event::Rejected { channel, run, attempt, account, generation } => {
            call::credential_notice(
                domain,
                channel,
                run,
                attempt,
                Request::Rejected { run, attempt, account, generation },
                out,
            );
        }
        Event::Exhausted { channel, run, attempt, account, retry_after } => call::credential_notice(
            domain,
            channel,
            run,
            attempt,
            Request::Exhausted { run, attempt, account, retry_after },
            out,
        ),
        Event::Loaded => attempt::loaded(domain, env),
        Event::Hello { channel, hello } => channel::hello(domain, env, channel, hello, out),
        Event::Lost { channel } => channel::lost(domain, env, channel),
        Event::Answer { channel, run, attempt, answer, payload } => {
            attempt::answer(domain, channel, run, attempt, answer, payload, out);
        }
        Event::Turn { channel, run, attempt, turn, body } => {
            turn::received(domain, channel, run, attempt, turn, body, out);
        }
        Event::TurnKept { run, attempt, turn } => turn::kept(domain, run, attempt, turn, out),
        Event::TurnBusy { run, attempt, turn } => turn::busy(domain, run, attempt, turn, out),
        Event::Bounced { channel, run, attempt, name, bounce } => {
            call::bounced(domain, channel, run, attempt, name, bounce, out);
        }
        Event::Told { channel, run, attempt, fact } => call::told(domain, channel, run, attempt, fact, out),
    }
}

/// Fires the earliest alarm due at `env.now`, if there is one, emitting at
/// most [`max_out`] requests. A stage fires its alarms after its input
/// events, so a hello that arrived in the same iteration wins over a grace
/// that passed while the loop waited.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    if let Some(id) = domain.alarms.expire(env.now) {
        attempt::expire(domain, id, out);
    }
}

/// Releases one held turn or places a waiting attempt, emitting at most
/// [`max_out`] requests; with none, waits until something changes.
pub fn resume(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    if domain.turning {
        turn::resume(domain, out);
    } else if domain.placing {
        attempt::resume(domain, env, out);
    }
}
