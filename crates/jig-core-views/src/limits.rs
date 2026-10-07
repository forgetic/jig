//! Admission and memory bounds for live views (domain/engine.md, 13).

use skein_lib::{Id, List, Map, Queue, Slab, Token};

use crate::boundary::Chunk;
use crate::facts::Fact;
use crate::watch::Watcher;

/// Fixed capacities for the views child.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Live runs followed.
    pub runs: u32,
    /// Open watches.
    pub watchers: u32,
    /// Chunks waiting behind one delivery.
    pub backlog: u32,
    /// Largest live report.
    pub report_bytes: u32,
    /// Largest initial snapshot.
    pub snapshot_bytes: u32,
    /// Best effort facts waiting to be drained.
    pub facts: u32,
}

pub(crate) fn slots(limits: &Limits) -> Option<u32> {
    limits.watchers.checked_mul(2)
}

/// Maximum live memory including watch backlogs and one event in hand.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.runs == 0 || limits.watchers == 0 || limits.backlog == 0 {
        return None;
    }
    let runs = Map::<Token, Token>::worst_case(limits.runs)?.checked_mul(2)?;
    let slots = slots(limits)?;
    let watchers =
        Slab::<Watcher>::worst_case(slots)?.checked_add(Map::<Token, Id<Watcher>>::worst_case(limits.watchers)?)?;
    let backlogs = u64::from(slots).checked_mul(Queue::<Chunk>::worst_case(limits.backlog)?)?.checked_add(
        u64::from(limits.watchers)
            .checked_mul(u64::from(limits.backlog))?
            .checked_mul(u64::from(limits.report_bytes))?,
    )?;
    let facts = Queue::<Fact>::worst_case(limits.facts)?;
    let hand = u64::from(limits.report_bytes)
        .max(u64::from(limits.snapshot_bytes))
        .checked_add(List::<Id<Watcher>>::worst_case(limits.watchers)?)?;
    runs.checked_add(watchers)?.checked_add(backlogs)?.checked_add(facts)?.checked_add(hand)
}
