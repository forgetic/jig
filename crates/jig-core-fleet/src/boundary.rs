//! The records that cross the boundary with the fleet's parent, the engine's
//! root domain (programming-model.md, 4.5). The fleet defines them; its
//! parent depends on it.
//!
//! The fleet has two faces, both through its parent:
//!
//! - The workers', which the parent routes to and from their channels
//!   (domain/hosts.md, sections 2 and 4). A worker's channel is named by the
//!   protocol's token for it, `channel`, and is in contact from its
//!   [`Event::Hello`], the first thing said on it, until its
//!   [`Event::Lost`]. [`Request::Assign`] is a call to the worker, answered
//!   by one [`Event::Answer`] under the run's and the attempt's names. The
//!   worker keeps the answer, and the slot it takes, until the fleet sends
//!   [`Request::Acknowledge`], which it does once the parent has made the
//!   answer durable, or once the answer is for an attempt fenced off; it
//!   sends it again after every hello until then. A refusal goes once and
//!   keeps nothing. [`Request::Inbound`], [`Request::Cancel`] and
//!   [`Request::Relayed`] are notices to the worker hosting an attempt;
//!   [`Event::Relay`] is a call of the run's, which the fleet passes up and
//!   whose answer it passes down, at most once; [`Event::Bounced`] and
//!   [`Event::Told`] are notices. [`Request::Refuse`] turns a worker away
//!   at its hello, when the fleet has no room for another: its channel is
//!   to be closed. An answer from a channel not in contact is dropped, and
//!   not acknowledged. Everything a worker sends names a run and an attempt,
//!   and is dropped unless that attempt is the parent's live claim (attempts
//!   are fenced): only its answer is still taken once it is cancelled.
//! - The parent's own. [`Event::Start`] and [`Event::Adopt`] are calls,
//!   each ended by exactly one of [`Request::Answered`], [`Request::Lost`],
//!   [`Request::Withdrawn`] or [`Request::Refused`]; a start is told
//!   [`Request::Placed`] before, each time it is assigned, and an adoption
//!   once a worker is found to host it. An attempt a worker refuses as busy
//!   is placed again, not ended. An answer handed to the parent is answered
//!   by exactly one [`Event::Acknowledge`], once the parent has made it
//!   durable or no longer wants it: until then its worker keeps it. After a
//!   restart, the parent adopts the claims its records hold and then says
//!   [`Event::Loaded`]; an attempt a worker lists that no claim adopts is
//!   told as [`Request::Listed`], and waits to be adopted for the grace from
//!   then. [`Event::Cancel`] and [`Event::Inbound`] name an attempt, and an
//!   inbound event that does not reach a worker comes back as
//!   [`Request::Undelivered`]. A [`Request::Relay`] is a call the parent
//!   answers with exactly one [`Event::Relayed`].
//!
//! What the fleet passes through and never reads is the parent's. An
//! assignment (its charter, workspace and snapshot) is named by its run and
//! attempt, and the parent attaches it to the [`Request::Assign`] that
//! names them. An inbound event, a relayed call and its answer, a run's
//! answer, turns and facts are each named by a token the parent issues, and
//! echoed exactly once: in the request that passes it on, or in a
//! [`Request::Drop`] that tells the parent to forget it. Tokens of different
//! families may be equal.

use alloc::boxed::Box;

use skein_lib::{Duration, ReplyTo, Token};

/// A kind of host that can run an attempt.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HostKind {
    /// A worker connected on a channel.
    Worker,
    /// The engine's own local host.
    Engine,
}

/// The kinds a run's charter permits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kinds {
    /// Only workers may host the run.
    Workers,
    /// Only the engine may host the run.
    Engine,
    /// Either kind may host the run.
    Both,
}

impl Kinds {
    /// Whether a host kind is allowed by the charter.
    #[must_use]
    pub const fn allows(self, kind: HostKind) -> bool {
        match self {
            Kinds::Workers => matches!(kind, HostKind::Worker),
            Kinds::Engine => matches!(kind, HostKind::Engine),
            Kinds::Both => true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Grant {
    pub account: u32,
    pub generation: u64,
    pub valid: Duration,
}

/// parent -> fleet
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// From the parent, a call: place the attempt `attempt` of the run `run`
    /// on a worker with a free slot, preferring one that holds `workstream`,
    /// and assign it; then answer once it has ended. It waits for a slot, and
    /// for its run's earlier attempts to be gone; a newer attempt of the run
    /// withdraws it.
    Start {
        reply_to: ReplyTo,
        run: Token,
        attempt: Token,
        workstream: u64,
        /// Host kinds the charter permits.
        kinds: Kinds,
    },
    /// From the parent, a claim restored after restart. A worker may report
    /// it within the grace; an engine-hosted claim is settled at once.
    Adopt {
        reply_to: ReplyTo,
        run: Token,
        attempt: Token,
        /// The committed prefix restored with the claim; zero before its first turn.
        kept: u32,
        /// Kind of host that held the claim before restart.
        kind: HostKind,
        /// Whether a turn or call was committed for this attempt.
        worked: bool,
    },
    /// From the parent: cancel the attempt `attempt` of the run `run`. Its
    /// call is still ended by its answer, unless it was never placed.
    Cancel {
        run: Token,
        attempt: Token,
    },
    /// From the parent: an inbound event for the attempt `attempt` of the run
    /// `run`, to pass to its worker.
    Inbound {
        run: Token,
        attempt: Token,
        event: Token,
    },
    /// From the parent, the one answer to a `Relay`.
    Relayed {
        to: ReplyTo,
        answer: Token,
    },
    /// From the parent, the one answer to an `Answered`: the answer of the
    /// attempt `attempt` of the run `run` is durable, or not wanted, and its
    /// worker may forget it.
    Acknowledge {
        run: Token,
        attempt: Token,
    },
    /// From the parent: its cold read after a restart is done, every claim
    /// its records hold adopted. Attempts workers list that no claim adopts
    /// wait to be adopted for the grace from now on, and those listed later
    /// for the grace from their listing.
    Loaded,
    Grant {
        run: Token,
        attempt: Token,
        grant: Grant,
    },
    Rejected {
        channel: Token,
        run: Token,
        attempt: Token,
        account: u32,
        generation: u64,
    },
    Exhausted {
        channel: Token,
        run: Token,
        attempt: Token,
        account: u32,
        retry_after: Duration,
    },
    /// From a worker, first on its channel: its slots, the workstreams it
    /// holds workspaces for, and the runs it hosts or holds answers of.
    Hello {
        channel: Token,
        hello: Hello,
    },
    /// From the protocol: the channel of a worker closed.
    Lost {
        channel: Token,
    },
    /// From a worker, the answer to an `Assign`: sent at once, and again
    /// after every hello until it is acknowledged. Taken only from a channel
    /// in contact.
    Answer {
        channel: Token,
        run: Token,
        attempt: Token,
        answer: Answer,
        payload: Token,
    },
    /// From the current worker: an opaque turn numbered from one. Its body
    /// is echoed exactly once by `Turned` or `Drop`; a duplicate before
    /// commitment is dropped unacknowledged. Strays' turns wait for adoption.
    Turn {
        channel: Token,
        run: Token,
        attempt: Token,
        turn: u32,
        body: Token,
    },
    /// The parent committed the prefix through `turn`. Commitments arrive in
    /// order; the worker may forget this turn. Adoption restores the prefix.
    TurnKept {
        run: Token,
        attempt: Token,
        turn: u32,
    },
    /// The parent could not commit an admitted turn. Release its admission
    /// and tell its current worker to retry it after a backoff.
    TurnBusy {
        run: Token,
        attempt: Token,
        turn: u32,
    },
    /// From a worker, a host call of the attempt `attempt` of the run `run`,
    /// which the worker names `call`, relayed as it is.
    Relay {
        channel: Token,
        run: Token,
        attempt: Token,
        call: Token,
        body: Token,
    },
    /// From a worker: an inbound event for the attempt `attempt` of the run
    /// `run` was not passed on, for `bounce`.
    Bounced {
        channel: Token,
        run: Token,
        attempt: Token,
        name: Token,
        bounce: Bounce,
    },
    /// From a worker: a fact the run told, best effort.
    Told {
        channel: Token,
        run: Token,
        attempt: Token,
        fact: Token,
    },
}

/// fleet -> parent
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// To the chosen host, a call to run an attempt. Engine slots use the
    /// reserved local token zero; workers use their channel tokens.
    Assign {
        channel: Token,
        kind: HostKind,
        run: Token,
        attempt: Token,
    },
    Grant {
        channel: Token,
        run: Token,
        attempt: Token,
        grant: Grant,
    },
    Rejected {
        run: Token,
        attempt: Token,
        account: u32,
        generation: u64,
    },
    Exhausted {
        run: Token,
        attempt: Token,
        account: u32,
        retry_after: Duration,
    },
    /// To a worker: an inbound event for the attempt it hosts.
    Inbound {
        channel: Token,
        run: Token,
        attempt: Token,
        event: Token,
    },
    /// To a worker: cancel the attempt `attempt` of the run `run`. Sent
    /// again on every channel whose hello lists it unanswered, the first
    /// having maybe been lost.
    Cancel {
        channel: Token,
        run: Token,
        attempt: Token,
    },
    /// To a worker: the answer to its relayed call `call`.
    Relayed {
        channel: Token,
        run: Token,
        attempt: Token,
        call: Token,
        answer: Token,
    },
    /// To a worker: the engine has the answer of the attempt `attempt` of the
    /// run `run` durably, or does not want it, and the worker forgets it.
    Acknowledge {
        channel: Token,
        run: Token,
        attempt: Token,
    },
    /// To the parent: this turn's body, once per admission, for commitment.
    /// The parent owns the body after this request and answers with `TurnKept`
    /// or `TurnBusy`. An engine restart may hand an uncommitted turn on again.
    Turned {
        run: Token,
        attempt: Token,
        turn: u32,
        body: Token,
    },
    /// To the current worker: this turn is committed or fenced off.
    AcknowledgeTurn {
        channel: Token,
        run: Token,
        attempt: Token,
        turn: u32,
    },
    /// To the current worker: no admission or commitment room; keep this turn
    /// and retry after a backoff. A received body also gets `Drop`.
    TurnBusy {
        channel: Token,
        run: Token,
        attempt: Token,
        turn: u32,
    },
    /// About a worker: the fleet has no room for it, or its hello is beyond
    /// the limits. Close its channel; it dials again later.
    Refuse {
        channel: Token,
    },
    /// To the parent: the attempt is on a worker, assigned or found there.
    Placed {
        run: Token,
        attempt: Token,
    },
    /// To the parent: a worker lists the attempt `attempt` of the run `run`,
    /// which no claim adopts. It waits to be adopted for the grace, from the
    /// parent's `Loaded` at the earliest, and is then cancelled.
    Listed {
        run: Token,
        attempt: Token,
    },
    /// To the parent, terminal for `Start` and `Adopt`: the worker's answer,
    /// never `Busy`. Each gets the parent's `Acknowledge`, until which its
    /// worker keeps it (a refusal keeps nothing, and the fleet forgets it at
    /// once).
    Answered {
        to: ReplyTo,
        run: Token,
        attempt: Token,
        answer: Answer,
        payload: Token,
    },
    /// To the parent, terminal for an engine-hosted adoption with no committed work.
    /// It consumes no try of the task.
    NotStarted {
        to: ReplyTo,
        run: Token,
        attempt: Token,
    },
    /// To the parent, terminal for `Start` and `Adopt`: the attempt is
    /// presumed lost, its worker out of contact past the grace or none
    /// having said it hosts it. Whatever still comes for it is dropped.
    Lost {
        to: ReplyTo,
        run: Token,
        attempt: Token,
    },
    /// To the parent, terminal for `Start` and `Adopt`: the attempt was
    /// withdrawn, for `withdrawal`.
    Withdrawn {
        to: ReplyTo,
        run: Token,
        attempt: Token,
        withdrawal: Withdrawal,
    },
    /// To the parent, terminal for `Start` and `Adopt`: refused at the
    /// entrance, nothing done.
    Refused {
        to: ReplyTo,
        run: Token,
        attempt: Token,
        refusal: Refusal,
    },
    /// To the parent, a call: a host call of the run's, relayed as it is.
    Relay {
        reply_to: ReplyTo,
        run: Token,
        attempt: Token,
        body: Token,
    },
    /// To the parent: an inbound event its worker did not pass on, for
    /// `bounce`.
    Bounced {
        run: Token,
        attempt: Token,
        name: Token,
        bounce: Bounce,
    },
    /// To the parent: the inbound event `event` reached no worker, for
    /// `undelivered`. The parent keeps it.
    Undelivered {
        run: Token,
        attempt: Token,
        event: Token,
        undelivered: Undelivered,
    },
    /// To the parent: a fact the run told.
    Told {
        run: Token,
        attempt: Token,
        fact: Token,
    },
    /// To the parent: forget `payload`, which goes no further: it was for an
    /// attempt fenced off or gone, a duplicate, or a call the fleet had no
    /// room for.
    Drop {
        payload: Token,
    },
}

/// What a worker says first on every channel (domain/engine.md, section 8).
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct Hello {
    /// Declared stop bound: contact grace, then max(cancel grace, push
    /// deadline), then save commit and push. It must be strictly below the engine's grace;
    /// `None` preserves the first payload version's unchecked behavior.
    pub graces: Option<Duration>,
    /// How many runs it hosts at once: none once it is shutting down.
    pub slots: u32,
    pub workstreams: Box<[u64]>,
    /// The runs it hosts, and those whose answers it holds.
    pub hosting: Box<[Hosted]>,
}

/// A run a worker hosts, or whose answer it holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Hosted {
    pub run: Token,
    pub attempt: Token,
    pub phase: Phase,
}

/// Where a hosted run is in its lifecycle (domain/hosts.md, 4.2), as a hello
/// says.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Phase {
    Preparing,
    Starting,
    /// Its agent is at work.
    Active,
    /// It yielded, and waits for its next inbound event.
    Waiting,
    /// How it ends is decided, and it answers next.
    Ending,
    /// It answered while there was no channel: the answer follows the hello.
    Answered,
}

/// How a run's attempt was answered (domain/hosts.md, 4.3): what the fleet
/// acts on. The rest is the payload's.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Answer {
    /// It ended with its outcome.
    Ended,
    /// It parked, with its snapshot if it had one.
    Parked,
    /// It failed, for a typed failure.
    Failed,
    /// Refused at the entrance, every slot taken or the worker shutting
    /// down: the fleet places the attempt again, and nothing more on that
    /// worker until it frees a slot. Never handed to the parent.
    Busy,
    /// Refused at the entrance: the assignment does not fit the worker's
    /// limits.
    Invalid,
}

/// Why a worker did not pass an inbound event on (domain/hosts.md, 4.2).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bounce {
    /// It holds more bytes than the worker's limits allow.
    TooLarge,
    /// The run is not live yet, and holds as many events as it may.
    Full,
    /// The run is ending.
    Ending,
}

/// Why an inbound event reached no worker.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Undelivered {
    /// The attempt waits to be placed.
    Unplaced,
    /// Its worker is out of contact, or not found yet.
    Adrift,
    /// The attempt is not the parent's live claim: cancelled, withdrawn,
    /// lost, answered, or never made.
    Gone,
}

/// Why an attempt was withdrawn.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Withdrawal {
    /// The parent cancelled it before it was placed.
    Cancelled,
    /// The parent started or adopted a newer attempt of its run. Placed, it
    /// is cancelled, and fenced.
    Replaced,
}

/// Why a start or an adoption was refused at the entrance.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Refusal {
    /// The fleet tracks as many of the parent's attempts as it may.
    Busy,
    /// The workstream task number is zero.
    Workstream,
    /// The fleet knows the attempt already.
    Duplicate,
}
