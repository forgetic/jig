//! Root-configured capacities and checked heap accounting (domain/people.md, sections 2–5). `worst_case` validates limits before construction;
//! bounds cover retained state and scratch. Root accounts output queues and
//! payloads separately. These calculations know no IO, credentials or tasks.

use crate::domain::{Answered, Pending, SignIn};
use crate::{Fact, Holding, Identity, IdentityKey, InitialOwner, RequestKey};
use alloc::boxed::Box;
use skein_lib::{Deadlines, Duration, Id, List, Map, Queue, Slab};

/// Immutable root-configured capacities and payload limits for people's retained state and one-step
/// outputs; validate via `worst_case` before construction. (domain/people.md, sections 2–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Maximum retained parties and provider/subject index entries.
    pub people: u32,
    /// Maximum cached unread result references per person; older entries are paged from tasks.
    pub inbox_entries: u32,
    /// Maximum live secret-free sign-ins and expiry alarms.
    pub sign_ins: u32,
    /// Maximum retained project role sets.
    pub projects: u32,
    /// Maximum unique-person holdings per project role set.
    pub holdings: u32,
    /// Maximum goals whose priorities one keyed request changes together.
    pub goals: u32,
    /// Maximum retained configured first-owner entries; also bounds bootstrap output matches.
    pub initial_owners: u32,
    /// Maximum completed keys plus slots reserved by pending flights.
    pub requests: u32,
    /// Maximum simultaneous routed keyed flights.
    pub pending: u32,
    /// Positive maximum reply destinations per flight, including its first caller.
    pub waiters: u32,
    /// Maximum combined subject, login and display-name bytes per party.
    pub identity_bytes: u32,
    /// Maximum opening-word or escalation rejection-reason bytes per keyed ask, including pending
    /// and completed copies.
    pub words: u32,
    /// Maximum owned bytes carried by one keyed task amendment.
    pub amendment_bytes: u32,
    /// Nonzero configured lifetime projected once from admission's wall/monotonic environment.
    pub sign_in_lifetime: Duration,
    /// Nonzero time for which completed person keys replay their durable answers.
    pub request_retention: Duration,
    /// Capacity of optional content-free observations; overflow increments a diagnostic lost
    /// counter.
    pub facts: u32,
}

/// Containers, retained bytes, pending copies and bounded bootstrap config. Checked heap bound
/// under `limits` for retained containers/bytes, pending copies, bootstrap owners, restoration
/// sign-in snapshot, role-update scratch and facts. Returns `None` for arithmetic overflow, zero
/// waiters or zero sign-in lifetime; validates `max_out` additions. Caller separately counts
/// incoming/outgoing payloads and Request queue storage.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    limits.initial_owners.checked_add(4)?;
    limits.waiters.checked_add(1)?;
    limits.sign_ins.checked_add(1)?;
    limits.holdings.checked_add(1)?;
    if limits.waiters == 0 || limits.sign_in_lifetime == Duration::ZERO || limits.request_retention == Duration::ZERO {
        return None;
    }
    let ask_bytes = u64::from(limits.words)
        .max(u64::from(limits.amendment_bytes))
        .max(u64::from(limits.holdings).checked_mul(u64::try_from(size_of::<Holding>()).ok()?)?)
        .max(u64::from(limits.goals).checked_mul(u64::try_from(size_of::<(u64, u32)>()).ok()?)?);
    let people = Map::<u64, Identity>::worst_case(limits.people)?
        .checked_add(Map::<IdentityKey, u64>::worst_case(limits.people)?)?
        .checked_add(u64::from(limits.people).checked_mul(u64::from(limits.identity_bytes).checked_mul(2)?)?)?;
    let roles = Map::<u32, Box<[Holding]>>::worst_case(limits.projects)?.checked_add(
        u64::from(limits.projects)
            .checked_mul(u64::from(limits.holdings))?
            .checked_mul(u64::try_from(size_of::<Holding>()).ok()?)?,
    )?;
    // At most one role per holding; only held roles can have a cached inbox.
    let waiting_recipients = limits.people.checked_add(limits.projects.checked_mul(limits.holdings)?)?;
    let waiting = Map::<crate::Whom, Box<[crate::Entry]>>::worst_case(waiting_recipients)?
        .checked_add(
            u64::from(waiting_recipients).checked_mul(List::<crate::Entry>::worst_case(limits.inbox_entries)?)?,
        )?
        .checked_add(List::<crate::Whom>::worst_case(waiting_recipients)?)?
        .checked_add(List::<crate::Entry>::worst_case(limits.inbox_entries)?)?;
    people
        .checked_add(roles)?
        .checked_add(waiting)?
        .checked_add(Map::<u64, u64>::worst_case(limits.people)?)?
        .checked_add(Map::<u64, Box<[crate::ResultRef]>>::worst_case(limits.people)?)?
        .checked_add(
            u64::from(limits.people).checked_mul(List::<crate::ResultRef>::worst_case(limits.inbox_entries)?)?,
        )?
        .checked_add(List::<crate::ResultRef>::worst_case(limits.inbox_entries)?)?
        .checked_add(Map::<u64, SignIn>::worst_case(limits.sign_ins)?)?
        .checked_add(Deadlines::<u64>::worst_case(limits.sign_ins)?)?
        .checked_add(Map::<RequestKey, Answered>::worst_case(limits.requests)?)?
        .checked_add(Map::<RequestKey, crate::domain::OpenWatch>::worst_case(limits.requests)?)?
        .checked_add(Deadlines::<RequestKey>::worst_case(limits.requests)?)?
        .checked_add(u64::from(limits.requests).checked_mul(ask_bytes)?)?
        .checked_add(Slab::<Pending>::worst_case(limits.pending)?)?
        .checked_add(Map::<RequestKey, Id<Pending>>::worst_case(limits.pending)?)?
        .checked_add(u64::from(limits.pending).checked_mul(List::<skein_lib::ReplyTo>::worst_case(limits.waiters)?)?)?
        .checked_add(u64::from(limits.pending).checked_mul(ask_bytes)?)?
        .checked_add(u64::from(limits.initial_owners).checked_mul(
            u64::try_from(size_of::<InitialOwner>()).ok()?.checked_add(u64::from(limits.identity_bytes))?,
        )?)?
        .checked_add(List::<(u64, SignIn)>::worst_case(limits.sign_ins)?)?
        .checked_add(List::<Holding>::worst_case(limits.holdings)?)?
        .checked_add(Queue::<Fact>::worst_case(limits.facts)?)
}
