//! Runs Smith's domain on the engine's own slots (domain/hosts.md, sections 2,
//! 5 and 8). Each live assignment owns one Smith domain. The host keeps turns
//! and answers until the core acknowledges them, and fences every callback by
//! task and attempt. It knows no application, connector, or store record.
//!
//! [`step`] handles assignments, messages, call and completion terminals, and
//! acknowledgements. [`fire`] advances Smith's alarms or a cancelled run's
//! grace. [`resume`] advances Smith's deferred work. The caller drains requests
//! before the next entry and calls [`reclaim`] once per iteration.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod charter;
mod domain;
mod limits;
#[cfg(test)]
mod tests;

pub use boundary::{
    Assignment, Budget, Charter, Completion, Contract, Event, FieldRule, Grant, ItemRule, Items, MessageRefusal, Model,
    Prices, Refusal, Request, Section, TextRule, Tool, ToolEffect, VerdictRule, WorkspaceTools,
};
pub use domain::{Host, fire, max_out, next_deadline, reclaim, resume, step};
pub use limits::{Limits, worst_case};
