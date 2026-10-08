use skein_lib::{Deadlines, Duration, Id, Map, Queue, Set, Slab, Token};

use crate::attempt::{Attempt, Run};
use crate::call::PendingCall;
use crate::channel::Channel;
use crate::facts::Fact;
use crate::turn::Pending;

/// The fleet's limits (programming-model.md, section 7), handed by its parent
/// to every step read-only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Workers in contact at once. A worker beyond them is turned away at
    /// its hello.
    pub workers: u32,
    /// Slots hosted inside the engine; zero in applications using only workers.
    pub engine_slots: u32,
    /// The most runs a worker hosts at once: slots a hello says beyond them
    /// are not used, and a hello listing more runs is turned away.
    pub slots: u32,
    /// The workstreams kept per worker, for placement to prefer.
    pub workstreams: u32,
    /// The parent's attempts tracked at once: waiting to be placed, out on
    /// workers, or their answers not yet acknowledged; and those it replaced
    /// while a worker may still host them. A start or an adoption beyond them
    /// is refused as busy. Beyond them, room is kept for what workers list
    /// that the parent has not claimed, a worker's slots for each worker; a
    /// listing beyond that room is cancelled.
    pub attempts: u32,
    /// Relayed calls the parent serves at once. A call beyond them is
    /// dropped, and its run withdraws it past its own deadline.
    pub calls: u32,
    /// Maximum bytes of a run's opaque call name retained for each typed relay.
    pub call_name_bytes: u64,
    /// Turns awaiting adoption or the parent's commitment, across all
    /// attempts. Bodies are the parent's tokens, never bytes held here.
    pub turns: u32,
    /// How long the runs of a worker whose channel was lost are kept for it
    /// to come back, and how long an attempt a worker lists, and the parent
    /// has not claimed, waits to be adopted once the parent has loaded its
    /// claims.
    pub grace: Duration,
    /// Facts kept until the parent drains them. Beyond them, facts are
    /// dropped and counted.
    pub facts: u32,
}

/// The attempts tracked at once under `limits`: the parent's, and the room
/// kept for workers' listings; or `None` if they do not fit a `u32`.
pub(crate) fn tracked(limits: &Limits) -> Option<u32> {
    limits.attempts.checked_add(limits.workers.checked_mul(limits.slots)?)
}

/// The most memory the domain holds under `limits`, in bytes
/// (programming-model.md, 6.3), or `None` if it does not fit a `u64`.
///
/// It counts the containers, their bookkeeping included, not allocator overhead. What the fleet passes on (a hello's lists,
/// the parent's payloads, which it names by token) is moved into its state or
/// dropped in the step it arrives in: the workstreams it keeps are counted here, the
/// rest is its sender's to count.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    let count = limits.workers.checked_add(u32::from(limits.engine_slots > 0))?;
    let channels = Slab::<Channel>::worst_case(count)?.checked_add(Map::<Token, Id<Channel>>::worst_case(count)?)?;
    // Each worker: the attempts it hosts, and the workstream keys it holds.
    let hosts = Set::<Id<Attempt>>::worst_case(limits.slots)?;
    let keys = Map::<u64, u64>::worst_case(limits.workstreams)?;
    let workers = u64::from(limits.workers).checked_mul(hosts.checked_add(keys)?)?;
    let engine = if limits.engine_slots > 0 {
        Set::<Id<Attempt>>::worst_case(limits.engine_slots)?.checked_add(Map::<u64, u64>::worst_case(0)?)?
    } else {
        0
    };
    let tracked = tracked(limits)?;
    let attempts = Slab::<Attempt>::worst_case(tracked)?
        .checked_add(Map::<(Token, Token), Id<Attempt>>::worst_case(tracked)?)?
        .checked_add(Map::<Token, Run>::worst_case(tracked)?)?
        .checked_add(Map::<u64, Id<Attempt>>::worst_case(tracked)?)?
        .checked_add(Deadlines::<Id<Attempt>>::worst_case(tracked)?)?;
    let calls = Slab::<PendingCall>::worst_case(limits.calls)?
        .checked_add(u64::from(limits.calls).checked_mul(limits.call_name_bytes)?)?;
    let turns = Map::<(Id<Attempt>, u32), Pending>::worst_case(limits.turns)?;
    let facts = Queue::<Fact>::worst_case(limits.facts)?;
    channels
        .checked_add(workers)?
        .checked_add(engine)?
        .checked_add(attempts)?
        .checked_add(calls)?
        .checked_add(turns)?
        .checked_add(facts)
}
