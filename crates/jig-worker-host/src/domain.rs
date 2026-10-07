//! The host child domain's state and its entry points (section 3).

use skein_lib::{Env, Id, Map, Queue, Slab, Token};

use crate::boundary::{Event, Hosting, Reason, Request};
use crate::call::{self, Call};
use crate::facts::{Fact, Facts};
use crate::hosted::{self, Hosted};
use crate::limits::{self, Limits};

/// The most requests an entry point emits per call under `limits`: a run
/// that leaves live answers each of its relayed calls in flight, then stops
/// its agent, or, its agent gone, saves or releases its workspace and answers;
/// and an agent that starts is handed every inbound event held for it. The parent
/// reserves this much room in `out` before calling it.
#[must_use]
pub const fn max_out(limits: &Limits) -> u32 {
    let cancelling = limits.run_calls.saturating_mul(2);
    let starting = limits.held.saturating_add(limits.accounts);
    let most = if starting > cancelling { starting } else { cancelling };
    most.saturating_add(2)
}

/// The host child domain's state.
#[derive(Debug)]
pub struct Domain {
    /// The run slots.
    pub(crate) hosted: Slab<Hosted>,
    /// The runs not yet answered, by the engine's names for them.
    pub(crate) names: Map<Token, Id<Hosted>>,
    /// Host calls in flight, across runs.
    pub(crate) calls: Slab<Call>,
    /// Runs to cancel, each for its reason, one per resume.
    pub(crate) ready: Map<Id<Hosted>, Reason>,
    pub(crate) facts: Facts,
    /// The worker is shutting down: it admits no more runs.
    pub(crate) shut: bool,
    /// Answers the engine has yet to acknowledge, as the parent last said:
    /// each keeps its run's slot.
    pub(crate) unacknowledged: u32,
}

impl Domain {
    /// A domain with room for `limits`.
    #[must_use]
    pub fn new(limits: &Limits) -> Domain {
        let calls = limits::calls(limits).expect("worst_case accepted the limits");
        Domain {
            hosted: Slab::with_capacity(limits.slots),
            names: Map::with_capacity(limits.slots),
            calls: Slab::with_capacity(calls),
            ready: Map::with_capacity(limits.slots),
            facts: Facts::with_capacity(limits.facts),
            shut: false,
            unacknowledged: 0,
        }
    }

    /// Runs hosted: the slots taken, closed runs included until they are
    /// reclaimed.
    #[must_use]
    pub fn hosted(&self) -> u32 {
        self.hosted.len()
    }

    /// Runs that have not answered yet: those hosted, closed ones aside.
    #[must_use]
    pub fn unanswered(&self) -> u32 {
        self.names.len()
    }

    /// Host calls in flight, closed ones included until they are reclaimed.
    #[must_use]
    pub fn calls(&self) -> u32 {
        self.calls.len()
    }

    /// The engine's names for the hosted run `owner` (the token the host's
    /// requests for it carry) and where it is in its lifecycle, while it is
    /// hosted: for its parent to tell the engine what the run's agent says
    /// beside the host, its facts.
    #[must_use]
    pub fn hosting(&self, owner: Token) -> Option<Hosting> {
        hosted::hosting(self, owner)
    }

    /// Whether the attempt is already hosted: wire retries are deduplicated
    /// before constructing a new call and its `ReplyTo`.
    #[must_use]
    pub fn is_hosting(&self, run: Token, attempt: Token) -> bool {
        hosted::is_hosting(self, run, attempt)
    }

    /// Whether a wire answer names a pending relay of this exact attempt.
    #[must_use]
    pub fn is_relayed_for(&self, run: Token, attempt: Token, call: Token) -> bool {
        hosted::is_relayed_for(self, run, attempt, call)
    }

    /// Both the stable agent name and the current local delivery must match.
    #[must_use]
    pub fn is_relayed_named_for(&self, run: Token, attempt: Token, delivery: Token, call: Token) -> bool {
        if !self.is_relayed_for(run, attempt, delivery) {
            return false;
        }
        let Some(entry) = self.calls.get(Id::from_token(delivery)) else {
            return false;
        };
        match entry.state {
            call::State::Relayed { call: name, .. } | call::State::Settling { call: name } => name == call,
            call::State::Delivering { .. } | call::State::Closed => false,
        }
    }

    /// Whether the relayed call `call`, as the host names it, still waits for
    /// the engine's answer: neither answered, withdrawn, nor answered as
    /// unavailable as its run left live.
    #[must_use]
    pub fn is_relayed(&self, call: Token) -> bool {
        match self.calls.get(Id::from_token(call)) {
            Some(entry) => match entry.state {
                call::State::Relayed { .. } => true,
                call::State::Delivering { .. } | call::State::Settling { .. } | call::State::Closed => false,
            },
            None => false,
        }
    }

    /// Whether a run is ready to be cancelled. While one is, the loop resumes
    /// the root domain, which calls [`resume`], at the start of the
    /// domain's stage, before its input events.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.ready.is_empty()
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
        self.hosted.reclaim();
        self.calls.reclaim();
    }
}

/// Handles one event, emitting at most [`max_out`] requests.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::AssignV2 { reply_to, assignment } => hosted::assign_v2(domain, env, reply_to, assignment, out),
        Event::Turn { owner, turn } => hosted::turned(domain, env, owner, turn, out),
        Event::FinishedV2 { owner, turns, spent, finish } => {
            hosted::finished_v2(domain, env, owner, turns, spent, finish, out);
        }
        Event::Assign { reply_to, assignment } => hosted::assign(domain, env, reply_to, assignment, out),
        Event::Inbound { run, attempt, name, event } => hosted::inbound(domain, env, run, attempt, name, event, out),
        Event::Grant { run, attempt, grant } => hosted::grant(domain, run, attempt, grant, out),
        Event::Cancel { run, attempt } => hosted::cancel(domain, env, run, attempt, out),
        Event::Relayed { run, attempt, call, answer } => hosted::relayed(domain, run, attempt, call, answer, out),
        Event::RelayCancelled { call } => hosted::relay_cancelled(domain, call, out),
        Event::CancelAll { reason } => hosted::cancel_all(domain, reason),
        Event::Report => hosted::report(domain, out),
        Event::Unacknowledged { answers } => domain.unacknowledged = answers,
        Event::Prepared { owner, workspace } => hosted::prepared(domain, owner, workspace, out),
        Event::Unprepared { owner, failure, detail } => hosted::unprepared(domain, env, owner, failure, detail, out),
        Event::Started { owner, agent } => hosted::started(domain, env, owner, agent, out),
        Event::Called { owner, call, ask } => hosted::called(domain, env, owner, call, ask, out),
        Event::Withdrawn { owner, call } => hosted::withdrawn(domain, owner, call, out),
        Event::Bounced { owner, name, bounce } => hosted::bounced(domain, owner, name, bounce, out),
        Event::Yielded { owner } => hosted::yielded(domain, owner, out),
        Event::Finished { owner, finish } => hosted::finished(domain, env, owner, finish, out),
        Event::Faulted { owner, fault } => hosted::faulted(domain, env, owner, fault, out),
        Event::Gone { owner, detail } => hosted::gone(domain, env, owner, detail, out),
        Event::Delivered { owner, delivery } => hosted::delivered(domain, owner, delivery, out),
        Event::Saved { owner, at } => hosted::saved(domain, owner, at, out),
    }
}

/// Cancels a run on the ready list, if one is, emitting at most [`max_out`]
/// requests: one of those a `CancelAll` named, for its reason.
pub fn resume(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    hosted::resume(domain, env, out);
}
