//! Ops's infrastructure connector (examples.md, section 3.2;
//! domain/connectors.md, sections 3, 4, 6 and 11).
//!
//! It keeps the resources tasks rely on, fresh service and environment facts,
//! three level-triggered procedures, and durable outbox entries. The root
//! commits every record before releasing an effect. A keyed restart can be
//! recovered by operation ID; a backend with no IDs holds an uncertain task.
//!
//! | State | Event | Next state | Emits |
//! | --- | --- | --- | --- |
//! | named | fact changes by another hand | named | drift |
//! | procedure active | missing condition | waiting | fresh read |
//! | procedure active | action needed | effect outstanding | save, effect |
//! | effect outstanding | made | waiting for condition | save, wait |
//! | waiting for condition | condition met | finished | erase, finish |
//! | effect kept | journal release | effect sent | save, apply after commit |
//! | effect uncertain | lookup or deadline | settled, retry or held | outcome, save |
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod domain;
mod limits;

pub use boundary::{
    ApplyResult, Backend, Description, Effect, Entry, Environment, EnvironmentFact, Event, Form, Hold, Key, Looked,
    Named, Outcome, Phase, Pool, Procedure, ProcedurePhase, ProcedureSignal, ProcedureState, Record, RecordKey,
    Recovery, Request, Resource, Service, ServiceFact, StepDecision, SystemEvent, SystemRequest,
};
pub use domain::{Domain, MAX_OUT, fire, next_deadline, step};
pub use limits::{Limits, worst_case};

#[cfg(test)]
mod tests;
