//! Jig's host (domain/hosts.md, sections 4 to 6) keeps the lifecycle of runs
//! on the slots of a worker or an engine. It admits assignments, asks an
//! optional application workspace to prepare and serve deliveries, supervises
//! an agent capability, and answers after both have settled. It keeps turns
//! and answers until the core acknowledges them. The root translates the link
//! to the core, workspace and agent capabilities; an engine root gives it a
//! direct link that is never lost and no workspace. The agent may be a process
//! or an inline agent, and the host does not distinguish them.
//! The host arms no timers; its callers bound each wait. Facts are best effort
//! and never govern a decision.

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
    AgentFailure, Answer, AnswerV2, AnsweredCall, Ask, Assignment, AssignmentTyped, Bounce, Delivery, DeliveryOutcome,
    EndingV2, Event, Failure, Finish, FinishV2, FromAgent, Grant, Hosting, Invalid, Phase, Preparation, Reason,
    Refusal, Reply, Request, RunFailure, SettledAnswer, ToAgent, Turn, Work, Workspace,
};
pub use domain::{Domain, max_out, resume, step};
pub use facts::{Fact, Told};
pub use limits::{Limits, worst_case};
