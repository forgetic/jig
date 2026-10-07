//! Jig's worker host (hosts.md, section 6). It admits assignments, asks the
//! application workspace to prepare and serve deliveries, supervises an agent
//! through the worker root, and answers after the agent and workspace settle.
//! The root translates its engine, workspace and agent capabilities. The host
//! has no timers; its callers bound every wait (hosts.md, sections 6 and 7).
//! Facts are best effort and never govern a decision.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod assignment;
mod boundary;
mod call;
mod domain;
mod facts;
mod hosted;
mod limits;
#[cfg(test)]
mod tests;
mod turns;

pub use boundary::{
    AgentFailure, Answer, AnswerV2, AnsweredCall, Ask, Assignment, AssignmentTyped, AssignmentV2, Bounce, Delivery,
    DeliveryOutcome, EndingV2, Event, Failure, Finish, FinishV2, FromAgent, Grant, Hosting, Invalid, Phase,
    Preparation, Reason, Refusal, Reply, Request, RunFailure, SettledAnswer, ToAgent, Turn, Work, Workspace,
};
pub use domain::{Domain, max_out, resume, step};
pub use facts::{Fact, Told};
pub use limits::{Limits, worst_case};
