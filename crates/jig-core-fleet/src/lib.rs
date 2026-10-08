//! The fleet child domain of the core
//! (programming-model.md, 4.5; domain/engine.md, sections 3 and 8): the
//! core's knowledge of hosts and the runs they hold. Workers dial in,
//! and each says hello first on its channel: its slots, the workstreams it
//! holds workspaces for, and the runs it hosts with where each is
//! (domain/hosts.md, section 2). The engine's own configured slots are a
//! second host, present from construction and lost only on restart. The fleet
//! places each run on a permitted host with a free slot, preferring a worker
//! that holds its workstream,
//! and the run waits, bounded, while none has one; never two attempts of a
//! run's workstream at once. It fences attempts: once an attempt is
//! cancelled or replaced, what its worker still sends is dropped, its answer
//! aside. It hands each answer to the parent once, and acknowledges it to
//! the worker, which keeps it and its slot until then, only once the parent
//! has made it durable. A worker that refuses an attempt as busy gets
//! nothing more until it frees a slot, and the attempt is placed again. It
//! likewise passes numbered turns to the parent once per admission and
//! acknowledges them after commitment. A restored claim carries its committed
//! prefix; pending stray turns wait for adoption in bounded room. Turn
//! pressure is answered busy for the worker to retry after a backoff. It
//! keeps a lost worker's runs for a grace, and once the grace passes
//! presumes them lost; a worker that comes back says what it hosts, and the
//! fleet keeps what is still claimed and cancels the rest. After an engine
//! restart the parent adopts the claims its records hold, and says when it
//! has: a claim no worker reports within the grace is lost, and a run a
//! worker reports that no claim adopts within the grace from then is
//! cancelled, the parent told of it meanwhile. It relays inbound events and
//! cancels down to a run's worker, a run's host calls up to the parent and
//! their answers back, each call answered once, and its bounces and facts
//! up.
//!
//! Sans-io: [`step`], [`fire`] and [`resume`] turn events into requests and
//! change nothing but the [`Domain`] they are given. Every effect is a
//! [`Request`] that its parent routes to a worker's channel, the engine's host,
//! or its own state. Their outcomes come back
//! later through the parent as an [`Event`]. The fleet owns its timers: the
//! grace of a lost channel, and that of an adoption.
//!
//! The fleet knows workers, slots, workstreams, runs, attempts and phases,
//! and how an answer ends; charters, transcripts, outcomes, inbound events,
//! relayed calls and facts are the parent's, named by tokens the fleet passes
//! on or hands back. A run and an attempt are tokens to the fleet, as the host
//! protocol packs them.
//!
//! What happens is also told as content-free [`Fact`]s, kept in a bounded
//! queue the parent drains ([`Domain::pop_fact`]); what does not fit is dropped
//! and counted, and nothing the fleet decides depends on it.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod attempt;
mod boundary;
mod call;
mod channel;
mod domain;
mod facts;
mod limits;
#[cfg(test)]
mod tests;
mod turn;

pub use boundary::{
    Answer, Bounce, Event, Grant, Hello, HostKind, Hosted, Kinds, Phase, Refusal, Request, TypedAssignment, TypedCall,
    TypedMessage, Undelivered, Withdrawal,
};
pub use domain::{Domain, fire, max_out, resume, step};
pub use facts::Fact;
pub use limits::{Limits, worst_case};
