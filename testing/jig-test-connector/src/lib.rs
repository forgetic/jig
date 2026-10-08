//! A configurable application connector for jig's worlds (domain/connectors.md).
//!
//! The connector keeps adopted roles, live task names, pool and system facts,
//! topic subscriptions, procedure state and an effect outbox. It knows
//! neither the root nor any other connector. The
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
//! | fact stale | judge | awaiting read | wait, system read |
//! | awaiting read | fresh fact | verdict reached | changed, verdict |
//! | procedure active | step | awaiting outcome | save, checked decision |
//! | section staged | cut | smaller section | ready size |
//! | value staged | handover or drop | value absent | one transfer or none |
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod domain;
mod limits;
mod outbox;
mod procedures;
mod requirements;
mod values;

pub use boundary::{
    Adoption, ApplyResult, Attempt, Class, Classed, Config, Description, Effect, EffectPhase, Event, Fact, Form, Hold,
    HoldMode, Item, Key, KindSpec, Looked, Named, Origin, OutboxEntry, Outcome, Path, PoolSpec, ProcedureAction,
    ProcedureResult, ProcedureSignal, ProcedureSpec, ProcedureState, Read, Record, RecordKey, Recovery, Request,
    RequirementSpec, ResourceRole, ResourceSpec, RestartStep, StepDecision, SystemEvent, SystemRequest, TopicSpec,
    Verdict,
};
pub use domain::{Domain, MAX_OUT, closing_ready, fire, next_deadline, resume, step};
pub use limits::{Limits, worst_case};

#[cfg(test)]
mod tests;
