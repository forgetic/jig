//! An agent capability composed in the engine (domain/hosts.md, section 5.2).
//! Each run is a Smith domain. Its parent speaks the parent-facing vocabulary
//! of Smith's process host, in the same order, while completions pass directly
//! to the root's protocol layer. No pipe, process or workspace is held here.
//!
//! | State | Event | Next state | Emits |
//! |---|---|---|---|
//! | Empty | Spawn | Live or empty | Started, then Admitted or Gone |
//! | Live | Message, Answer, Grant, Acknowledge | Live | Smith's notices |
//! | Live | Stop | Winding down | Smith's cancel requests |
//! | Winding down | Answer or grace | Empty | Answered, Gone |

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;

mod boundary;
mod domain;
mod limits;
mod translation;

#[cfg(test)]
mod tests;

pub use boundary::{Completion, Request};
pub use domain::{Agent, fire, max_out, next_deadline, reclaim, resume, step, terminal};
pub use limits::{Limits, worst_case};
