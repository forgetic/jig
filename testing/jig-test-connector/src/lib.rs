//! A configurable application connector for jig's worlds (domain/connectors.md).
//!
//! The connector keeps adopted roles, live task names, pool facts and topic
//! subscriptions. It knows neither the root nor any other connector. The
//! root translates its own vocabulary into the core's and commits each
//! [`Request::Save`] or [`Request::Erase`] with the decision that caused it.
//! `step` emits at most [`MAX_OUT`] requests. At restart, the root restores
//! records before allowing system hints or task work.
//!
//! | State | Event | Next state | Emits |
//! | --- | --- | --- | --- |
//! | unadopted | adopt | adopted | save, adopted |
//! | task absent | names | task live | save, named |
//! | task live | unnamed | task absent | erase |
//! | unsubscribed | subscribe | subscribed | save |
//! | subscribed | unsubscribe | unsubscribed | erase |
//! | pool known | system pool change | pool known | save, slots, drift |
//! | subscribed | system news | subscribed | classified news |
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod domain;
mod limits;
mod outbox;

pub use boundary::{
    Adoption, ApplyResult, Attempt, Class, Classed, Config, Description, Effect, EffectPhase, Event, Form, Hold,
    HoldMode, Key, KindSpec, Looked, Named, Origin, OutboxEntry, Outcome, Path, PoolSpec, Record, RecordKey, Recovery,
    Request, ResourceRole, ResourceSpec, SystemEvent, SystemRequest, TopicSpec,
};
pub use domain::{Domain, MAX_OUT, fire, next_deadline, step};
pub use limits::{Limits, worst_case};

#[cfg(test)]
mod tests;
