use skein_lib::{Id, Map, Queue, Set, Slab, Token};

use crate::boundary::{Delivery, Reason};
use crate::call::Call;
use crate::facts::Fact;
use crate::facts::Told;
use crate::hosted::{Hosted, NamedEvent};
use crate::turns::{Name, Pending};

/// The host child domain's limits (section 7), handed by its parent to every
/// step read-only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Runs hosted at once: the run slots. An assignment beyond them is
    /// refused as busy.
    pub slots: u32,
    /// Distinct grants retained for a hosted attempt.
    pub accounts: u32,
    /// The most bytes of a charter.
    pub charter_bytes: u64,
    /// The most bytes of a snapshot: an assignment's, or a parked run's.
    pub snapshot_bytes: u64,
    /// Opaque transcript, including the committed call tail.
    pub transcript_bytes: u64,
    /// One completed turn body.
    pub turn_bytes: u64,
    /// The most bytes of a run's declared outcome. A run that says more has
    /// broken the rules.
    pub outcome_bytes: u64,
    /// The most bytes of detail an answer carries for operators: the tail of
    /// what it is given.
    pub detail_bytes: u32,
    /// Inbound events a run holds until it is live. Beyond them, an event is
    /// bounced.
    pub held: u32,
    /// The most bytes of an inbound event.
    pub event_bytes: u64,
    /// Host calls a run may have in flight at once, its delivery among them. A
    /// call beyond them is answered as busy.
    pub run_calls: u32,
    /// Facts kept until the parent drains them. Beyond them, facts are
    /// dropped and counted.
    pub facts: u32,
    /// Agent facts kept for the engine while the channel cannot take them.
    pub told: u32,
    /// The most bytes in one agent fact.
    pub fact_bytes: u64,
    /// Unacknowledged turns per attempt and their total body bytes.
    pub turns: u32,
    pub turn_queue_bytes: u64,
}

/// The host calls in flight at once, across runs, under `limits`, or `None` if
/// they do not fit a `u32`.
pub(crate) fn calls(limits: &Limits) -> Option<u32> {
    limits.slots.checked_mul(limits.run_calls)
}

/// The most memory the domain holds under `limits`, in bytes (6.3), or `None`
/// if it does not fit a `u64`.
///
/// It counts the containers, their bookkeeping included, and the payloads, not
/// allocator overhead. What the host passes on (a workspace, the bodies of
/// host calls and their answers, what a save did) is moved into a
/// request in the step it arrives in, and what it makes for a request (an
/// answer, a report) is moved out in the step that makes it: either is its
/// receiver's to count. One delivery terminal is counted at the step peak,
/// alongside the runs' retained state.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.turns > 0 && (limits.turn_bytes == 0 || limits.turn_queue_bytes < limits.turn_bytes) {
        return None;
    }
    // The reserved output bound must be representable without saturation.
    // Cancelling each relay emits its reply and its cancellation request.
    let cancellations = limits.run_calls.checked_mul(2)?;
    if cancellations.checked_add(2).is_none() || limits.held.checked_add(limits.accounts)?.checked_add(2).is_none() {
        return None;
    }
    let hosted = Slab::<Hosted>::worst_case(limits.slots)?;
    let names = Map::<Token, Id<Hosted>>::worst_case(limits.slots)?;
    let ready = Map::<Id<Hosted>, Reason>::worst_case(limits.slots)?;
    let call_count = calls(limits)?;
    let calls =
        Slab::<Call>::worst_case(call_count)?.checked_add(u64::from(call_count).checked_mul(limits.event_bytes)?)?;
    let facts = Queue::<Fact>::worst_case(limits.facts)?;
    let told =
        Queue::<Told>::worst_case(limits.told)?.checked_add(u64::from(limits.told).checked_mul(limits.fact_bytes)?)?;
    let capacity = limits.slots.checked_mul(limits.turns)?;
    let turns = Map::<Name, Pending>::worst_case(capacity)?
        .checked_add(u64::from(limits.slots).checked_mul(limits.turn_queue_bytes)?)?;
    // Until its agent starts, a run holds its charter, its snapshot and the
    // inbound events that came meanwhile; from when it is told how the run
    // finishes, the outcome, the snapshot or the detail of the failure.
    let held = Queue::<NamedEvent>::worst_case(limits.held)?
        .checked_add(u64::from(limits.held).checked_mul(limits.event_bytes)?)?;
    let starting =
        limits.charter_bytes.checked_add(limits.snapshot_bytes.max(limits.transcript_bytes))?.checked_add(held)?;
    let ending = limits.outcome_bytes.max(limits.snapshot_bytes).max(u64::from(limits.detail_bytes));
    // Throughout, the opaque workspace handle, grants, and relayed calls in
    // flight (a delivery is held inline).
    let run_calls = Set::<Id<Call>>::worst_case(limits.run_calls)?;
    let run = starting
        .max(ending)
        .checked_add(run_calls)?
        .checked_add(u64::from(limits.accounts).checked_mul(u64::try_from(size_of::<crate::Grant>()).ok()?)?)?;
    let runs = u64::from(limits.slots).checked_mul(run)?;
    // One terminal arrives per step; its fixed payload is included.
    let delivered = u64::try_from(size_of::<Delivery>()).ok()?;
    hosted
        .checked_add(names)?
        .checked_add(ready)?
        .checked_add(calls)?
        .checked_add(facts)?
        .checked_add(told)?
        .checked_add(turns)?
        .checked_add(runs)?
        .checked_add(delivered)
}
