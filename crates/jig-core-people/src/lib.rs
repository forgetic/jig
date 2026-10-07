//! People as parties (programming-model.md, 4.5; domain/people.md).
//! Keeps identities, secret-free sign-ins, project roles, keyed answers,
//! task-derived bounded inbox references and each person's result and reply read position.
//! Knows tasks only by number; the parent checks authority and makes tasks.
//! `Save`/`Erase` join the parent's decision; the parent holds replies until
//! durable (domain/engine.md, 5.6).
//!
//! The boundary admits adoption and other owner requests by key, checks their
//! project role and retains committed answers for replay after a restart
//! (domain/people.md, section 5.1). This child never sees provider
//! credentials, protocol bytes, task internals or authority policy state.
//! The root supplies authenticated identities and authoritative role changes.
//! It mints fresh deployment person candidates and sign-in numbers; the protocol
//! keeps cookie secrets or their digests separately, named by the root-issued
//! sign-in number (domain/people.md, section 3; domain/engine.md, sections 4 and 5.4).
//!
//! `Domain::new` starts restoring. `step` admits typed rows, then `Restored`
//! validates references and arms deadlines; failed restoration stays unready.
//! Ready requests share person-scoped keys across sign-ins: identical pending
//! copies join bounded waiters, saved copies replay the outcome, and conflicting
//! payloads refuse (domain/people.md, sections 3 and 5.1).
//! Each `Route` requires exactly one matching `Decided`; permanent outcomes
//! save one keyed answer, while `Busy`/`NotReady` closes the flight without saving
//! so its key can retry . `Save` and `Erase` are
//! root decision outputs, not IO operations; task changes and keyed answers
//! share one atomic commit (domain/people.md, section 5.1).
//! `fire` expires at most one due sign-in per call. The caller reserves free
//! request room from `max_out`, accounts output bytes separately, and reclaims
//! retired flights at the iteration boundary. `worst_case` includes retained
//! state and bounded scratch. `Domain::role` supplies current membership for root
//! escalation coverage/standing without authentication or copying. `Domain::has_project` is a pure bounded startup
//! lookup; `Domain::person` is a pure sign-in lookup after restoration, checking
//! both its projected monotonic deadline and saved wall expiry for root result
//! routing, even before timer firing.
//! Optional diagnostic facts do not affect behavior (domain/people.md, section 3).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;
mod amendment;
mod boundary;
mod domain;
mod facts;
mod limits;
mod policy_value;
#[cfg(test)]
mod tests;
pub use amendment::{
    Amendment, Authority, Delegation, Executor, Grant, Last, Parameter, Pattern, ResourceScope, ResultsWake, Spec,
    WakePolicy, WakeRule, amendment_bytes, authority_bytes,
};
pub use boundary::{
    Adoption, Ask, Entry, EntryKind, EscalationChoice, EscalationDecision, Event, Holding, Identity, IdentityKey,
    InitialOwner, Key, Kind, NoteChange, NoteScope, Outcome, PersonResult, ProposalChoice, ProposalDecision, Refusal,
    Reply, Request, RequestKey, ResourceName, ResourceRole, ResultRef, Role, Seed, Stored, WatchSubject, Whom,
};
pub use domain::{Domain, fire, max_out, step};
pub use facts::Fact;
pub use limits::{Limits, worst_case};
pub use policy_value::{
    Guard, Judge, PermissionRole, PolicyChange, PolicyRole, PolicyValue, Requirement, policy_bytes,
    policy_change_bytes, requirements_bytes,
};
