//! The views child streams four live subjects: a run, a task tree, a
//! project's goals, and a party's inbox (domain/engine.md, 11).
//!
//! A watch begins with its parent's snapshot. Its delivery is first.
//! Each watcher has one delivery in flight and a bounded backlog. Overflow
//! drops old chunks and counts them for the next delivery. Views keep no
//! history and never affect decisions. The parent owns authorization,
//! snapshots, and delivery to its clients.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod domain;
mod facts;
mod limits;
#[cfg(test)]
mod tests;
mod watch;

pub use boundary::{Chunk, End, Event, Kind, Refusal, Request, Subject};
pub use domain::{Domain, fire, max_out, step};
pub use facts::{Dropped, Fact, Lost};
pub use limits::{Limits, worst_case};
