//! A domain world for the host child domain (programming-model.md,
//! 4.5; testing-strategy.md, 2.2; hosts.md, sections 4 and 9): the
//! host, with the world as its parent, a scripted engine assigning it runs
//! ([`engine`]), and the parent's capabilities scripted ([`parent`]):
//! workspaces that prepare, delivery and save, and agents whose runs make host
//! calls, yield, park, end, fail, hang and get stopped. One loop drives them,
//! deterministically from a seed.
//!
//! The world owns the clock and the seeds, and stands in for everything around
//! the host: the top level that will route between it, the engine link and
//! its siblings (its grace on losing contact, the report on coming back, and
//! shutdown included), the workspace and agent child domains, and the protocol
//! layers below. It checks the contracts as it goes: every assignment is
//! answered exactly once, a refused one at once and never admitted, one beyond
//! the limits refused for what is beyond them; nothing is saved or released
//! while the run's agent may still be running or a delivery is in flight; a slot
//! is taken from admission until the run has answered, and the run answers
//! only once it has left live and all of it is released; a stale attempt, or
//! the attempt hosted assigned again, never acts; a host shutting down
//! admits nothing more; inbound events reach a run once each, in the order
//! sent; every host call is answered once, every call of a run that has left
//! live (a cancelled one among them) as unavailable, those relayed in flight
//! as it leaves, and a delivery in flight then with how it went; a run is
//! answered as it first said it finishes, also when it says so winding down
//! after a stop; and the report on coming back lists exactly the runs
//! admitted and not answered. Once it settles: nothing in flight, every slot
//! free, no call open, every workspace released and every agent gone.
//!
//! The peer world in [`turn_world`] adds a committing engine,
//! a link that loses and restores contact, and the host's agent and workspace
//! capabilities. It checks the retained turns, their replay and exact ACKs,
//! independent agent acknowledgements, answer order, and best-effort agent facts (hosts.md, 11).

pub mod engine;
pub mod inline_world;
pub mod parent;
pub mod turn_world;
mod world;

pub use skein_world::domain::Span;
pub use world::{Outage, Settings, Stats, World};

pub mod fixtures;
