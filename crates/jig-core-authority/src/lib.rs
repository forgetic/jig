//! What a task may do, as an owned value with a preorder, its carved funding,
//! and the rules each action passes (domain/authority.md, sections 3–10;
//! programming-model.md, 4.5).
//!
//! Pure policy over values supplied by the engine's root: resource coverage,
//! authority order and accounting over snapshots. [`Domain`] holds only the
//! deployment's rules, project policies and their admission limits. It keeps
//! no task, funding numbers, connector or clock. The tasks child keeps the
//! numbers; accounting answers say what they become. Connector kind orders
//! are supplied as validated [`Implies`] data. Checks allocate nothing,
//! refuse oversized inputs at admission and write every independent reason
//! into caller-reserved bounded queues. [`needs`] constructs an owned value;
//! policy events update the bounded table and emit one lifecycle fact.

//!
//! Entry contracts: `check_*` borrow admitted snapshots, return one answer
//! and write bounded findings; the root supplies connector verdicts and commits
//! allowed numbers with actions exactly once (domain/authority.md, sections
//! 7–9). `at_most`, coverage and accounting functions are pure value queries,
//! not child event protocols. Callers bound owned inputs before these queries;
//! `needs` copies bounded data and its output is counted by the caller.
//! `step` alone updates the policy table and emits one terminal `PolicyFact`
//! per event, with free room `POLICY_MAX_OUT` (domain/authority.md, section 6).
//! `max_out` bounds finding room and `worst_case` bounds retained policy heap;
//! callers count question payloads and queues (domain/authority.md, section 11).
//! This child never authenticates people, discovers task standing or funding
//! links, executes effects, or recognizes duplicate settlements.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod boundary;
mod check;
mod domain;
mod limits;
mod numbers;
mod order;
mod rules;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_numbers;
#[cfg(test)]
mod tests_policy;
mod value;

pub use boundary::{
    Action, Answer, BatchAsk, Call, CallAsk, Checked, Delegate, Effect, EffectAccess, EffectAsk, EffectResource,
    Finding, Given, Holder, NoteScope, PersonAsk, PersonRequest, RunAsk, Source, Verdict, Write, Writer,
};
pub use check::{check_batch, check_call, check_effect, check_request, check_run, covers, needed_judges, needs};
pub use domain::{Domain, Event, POLICY_MAX_OUT, PolicyFact, PolicyRefusal, step};
pub use limits::{Limits, max_out, worst_case};
pub use numbers::{Numbers, carve, charge, left, settle};
pub use order::{FITS_MAX_OUT, Lack, Lacks, fits};
pub use order::{
    Implication, Implies, at_most, grant_at_most, grant_covers, pattern_at_most, pattern_covers, resolve_task_grants,
};
pub use rules::{Guard, Judge, Policy, ProposalKind, Proposals, RequestKind, Requests, Requirement, Role, Rules};
pub use value::{Authority, Budget, Delegation, Executor, Grant, Last, Name, Pattern, ResourceScope, Scopes, Tools};
