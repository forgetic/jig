//! Ops's reference root: copy its journal and routing shape, replace the
//! connector arms and the application's pure translations (domain/root.md).
//! The core decides admission, authority, ordering and restart. Each child
//! keeps its values; this root carries only their requested continuations.
//! Every write and output leaves through the journal.
//! Ops configures observed verdicts and brief gathering for thirty seconds;
//! tool inputs are bounded at 64 KiB and sixteen nesting levels.
//! The copying application replaces those values with its own limits.
//!
//! | State | Event | Next state | Emits |
//! | --- | --- | --- | --- |
//! | open | admitted event | decision gathered | at most one commit |
//! | waiting | durable acknowledgement | outputs eligible | journal releases in order |
//! | open | no journal room | unchanged | busy |
//! | waiting | failed commit | stopped | no later held output |
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;
mod assemble;
mod boundary;
mod domain;
mod limits;
mod restart;
mod route;
mod store;
mod translate;
pub use boundary::{Event, Output, Released, Store, Timer};
pub use domain::{Config, Domain, Numbers, fire, release, step};
pub use limits::{Limits, worst_case};
pub use store::{Key, Range, Record, Write};
#[cfg(test)]
mod tests;
