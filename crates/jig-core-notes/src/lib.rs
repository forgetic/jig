//! Notes learned by tasks and corrected by parties, in the store (jig's
//! domain/engine.md, section 10). Entries are loaded on demand and every
//! entry's scope has a durable index line. A write compares the recalled
//! revision, then emits both records in one decision. The parent commits them
//! together. Notes know neither a connector's system nor the parent's store
//! encoding. [`step`] is the only entry point; there are no timers. Scope
//! indexes are bounded and evicted by least recent use; recall loads bodies
//! only for the page asked for.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod domain;
mod limits;
mod read;
#[cfg(test)]
mod tests;

pub use boundary::{
    Author, Change, Entry, Event, Key, Last, Line, New, Pattern, Range, Recall, Record, Refusal, Request, Rows, Scope,
};
pub use domain::{Domain, MAX_OUT, max_out, step, valid_entry, valid_line, valid_new};
pub use limits::{Limits, worst_case};
pub use read::valid_recall;
