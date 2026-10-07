//! Ops's observability connector (examples.md, sections 3.1 and 5;
//! domain/connectors.md, sections 5 to 7 and 9).
//!
//! The connector keeps current service facts, subscriptions, watch procedures,
//! staged silences and their durable outbox. It never knows the infrastructure
//! connector or the core. The root commits every `Save` or `Erase` before
//! releasing accompanying work, and restores records before live work.
//!
//! | State | Event | Next state | Emits |
//! | --- | --- | --- | --- |
//! | facts stale | judge | waiting for read | wait, read facts |
//! | waiting | fresh facts | judged | verdict, changed |
//! | subscribed | alert from outside | subscribed | classified news |
//! | watch active | new wake batch | watch active | save, triage |
//! | silence staged | keep | outbox kept | save, make after commit |
//! | outbox kept | make | outbox sent | save, system apply after commit |
//! | outbox uncertain | lookup finds key | settled | erase, outcome |
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod domain;
mod limits;

pub use boundary::{
    Alert, Class, Classed, Effect, Entry, Event, Fact, Key, LoadPoint, Outcome, Phase, Read, Record, RecordKey,
    Request, Requirement, Resource, Service, SystemEvent, SystemRequest, Topic, Verdict, Watch, Window,
};
pub use domain::{Domain, MAX_OUT, fire, next_deadline, step};
pub use limits::{Limits, worst_case};

#[cfg(test)]
mod tests;
