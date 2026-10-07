//! The tasks child: bounded live lifecycle, immutable dependencies and authentic
//! finite funding (programming-model.md, section 4.5; domain/tasks.md, sections 2–5 and 10). It keeps live task rows and name indices, retry timers,
//! configured agent charters, period/pool ledgers and optional diagnostic facts.
//! It keeps bounded pointers to ended results named by live work. It never knows protocol bytes, worker channels, connector internals, people
//! authentication or the authority child's policy tables. Root supplies those
//! decisions through independent semantic types.
//!
//! `Domain::new` starts restoring. `Stored::Live`, `Stored::Ledger` and `Stored::Stub`
//! enter `Restore`; `Restored` validates the complete live graph and authentic
//! financial reservations before bounded activation/adoption outputs. Historical
//! ended rows remain in root storage and cannot re-enter live state

//!
//! `step` receives root-authorized batches, finite-source inputs and actual
//! lifecycle notifications. Reply-bearing inputs have one terminal reply;
//! notifications can be ignored when stale. Priced turns/terminals preflight
//! the whole lifecycle/financial admission before posting the new cumulative
//! expense delta. Root owns exact transport replay proof and transcript rows;
//! tasks keeps no replay receipt or result-delivery credit

//!
//! `Request::Activate` carries a temporary bounded `RunContext`, not rendered
//! bytes or a second mutable task ledger. Root gathers the actual brief and
//! claims the task. `Request::Close` awaits root's actual closing obligations;
//! `Ended`, the historical task row, and original-source
//! posting share one atomic decision. The current root exposes person result notices and
//! delays outward replies/effects until durability; historical result reads
//! remain the root's route (domain/tasks.md, section 5.6; domain/engine.md, sections 5.6, 7.1 and 9).
//!
//! `fire` expires at most one projected retry timer per call and advances bounded
//! cascades. Caller reserves free `Request` room from `max_out`; `worst_case`
//! counts retained containers/payloads and bounded scratch, while callers count
//! queues and owned output copies. `stored_bytes` measures existing row heap
//! without cloning; `Domain::funding` borrows authentic finite accounting without
//! creating a mutable root shadow (domain/tasks.md, section 10).
//! Current chat construction supplies a report contract and empty parameters.
//! A held person chat owns one bounded semantic escalation revision/recipient
//! or rejected reason. Root authenticates its actual read/decision route, resolves
//! eligibility and owns immutable transport history; this child accepts/rejects/
//! passes only the authorized exact revision.
//! Agent and procedure executors are routed. The root checks historical inputs
//! before delegation; person executor routes are added in later increments.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;
mod admission;
mod batch;
mod boundary;
mod closing;
mod control;
mod domain;
mod escalation;
mod facts;
mod failures;
mod funders;
mod inbox;
mod limits;
mod moving;
mod owned;
mod person;
mod procedure;
mod proposals;
mod recurring;
mod refs;
mod run;
mod stored;
mod subscriptions;
#[cfg(test)]
mod tests;
mod value;
mod wake;
pub use batch::{valid_authority, valid_contract, valid_spec};
pub use boundary::{
    Accepted, Active, Cause, Closing, Contract, DelegateState, DelegationContext, End, Ending, Event, Executor, Hold,
    InvalidResult, Key, MessageKind, New, NewsClass, NoticeState, Parameter, Party, PersonAddress, Phase, Problem,
    ProcedureDecision, QuestionCredit, RecurringOverlap, RecurringState, RecurringTemplate, Refusal, Request,
    ResultFollowups, ResultKind, ResultsWake, RunContext, SavedResource, Spec, Stage, Status, Stored, Stub,
    Subscription, SubscriptionKind, TaskRecord, TaskResult, Verdict, WakePolicy, WakeRule, Was, Word,
};
pub use control::{Amendment, Change, Control, History};
pub use domain::{Domain, ViewTask, fire, max_out, step, view_phase};
pub use escalation::{Escalation, EscalationContext, EscalationDecision, EscalationHolder, EscalationOutcome};
pub use facts::Fact;
pub use failures::{Class, Retries, Retry, Tries};
pub use funders::FundingRecord;
pub use limits::{Limits, worst_case};
pub use owned::{stored_bytes, terminal_bytes};
pub use proposals::{
    PersonProposal, PersonProposalState, Proposal, ProposalAction, ProposalDecision, ProposalHolder, ProposalKind,
    ProposalOutcome, ProposalState,
};
pub use value::{
    Authority, AuthorityExecutor, Budget, Delegation, Funder, Grant, Last, Numbers, Pattern, ResourceScope, Scopes,
    Tools,
};
