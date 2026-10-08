//! Copy the checked sum and reserve a whole route before changing a child.
//! Connector and endpoint limits are application configuration (domain/root.md, 10).
use crate::boundary::Payload;
use crate::domain::{Assignment, HostCall, ReadCall, Work};
use crate::{Output, Write};
use alloc::boxed::Box;
use jig_charter as charter;
use jig_core as core;
use jig_core_authority as authority;
use jig_host as host;
use jig_inline_agent as agent;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{Journal, JournalLimits, JournalRoom, Map, Queue, Token};

/// Each child receives its own limits and the journal reserves their sum.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The core and its children.
    pub core: core::Limits,
    /// Observability's working set and outbox.
    pub observability: observability::Limits,
    /// Infrastructure's working set and outbox.
    pub infrastructure: infrastructure::Limits,
    /// Engine-slot run hub.
    pub host: host::Limits,
    /// Smith domains in the engine.
    pub agent: agent::Limits,
    /// The one commit barrier.
    pub journal: JournalLimits,
    /// Maximum synchronous continuations for one admitted event.
    pub routes: u32,
}

/// The whole child route, including every synchronous hand-off.
#[must_use]
pub fn room(limits: &Limits) -> Option<JournalRoom> {
    let core = core::room_max(&limits.core)?;
    let children = infrastructure::MAX_OUT
        .checked_add(observability::MAX_OUT)?
        .checked_add(host::max_out(&limits.host))?
        .checked_add(agent::max_out(&limits.agent))?;
    Some(JournalRoom {
        writes: core.writes.checked_add(children)?.checked_add(1)?,
        held: core.held.checked_add(children)?,
    })
}

/// Child states, journal, and bounded child-requested continuations.
/// Owned payload bytes are priced wherever the root retains or queues them;
/// values moved out of a child are no longer covered by that child's state.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    let calls = limits.host.slots.checked_mul(limits.host.run_calls)?;
    let callbacks = limits.host.slots.checked_mul(limits.agent.smith.run.calls)?;
    let effects = limits.core.call_records.checked_add(limits.core.tasks.tasks)?;
    let payloads = limits.core.fleet.turns.checked_add(limits.core.fleet.attempts)?;
    let children = core::worst_case(&limits.core)?
        .checked_add(observability::worst_case(&limits.observability)?)?
        .checked_add(infrastructure::worst_case(&limits.infrastructure)?)?
        .checked_add(host::worst_case(&limits.host)?)?
        .checked_add(agent::worst_case(&limits.agent)?)?;
    let tables = Map::<u64, Assignment>::worst_case(limits.core.tasks.tasks)?
        .checked_add(Map::<u64, Box<[charter::Section]>>::worst_case(limits.core.tasks.tasks)?)?
        .checked_add(Map::<core::CallKey, (Box<[u8]>, Box<[u8]>)>::worst_case(limits.core.call_records)?)?
        .checked_add(Map::<Token, ReadCall>::worst_case(limits.observability.reads)?)?
        .checked_add(Map::<Token, Payload>::worst_case(payloads)?)?
        .checked_add(Map::<Token, infrastructure::Effect>::worst_case(effects)?)?
        .checked_add(Map::<Token, (Token, authority::Judge, [u8; 32])>::worst_case(limits.core.authority.facts)?)?
        .checked_add(Map::<u64, (u64, u16, u32)>::worst_case(limits.core.tasks.tasks)?)?
        .checked_add(Map::<Token, u64>::worst_case(limits.core.tasks.tasks)?)?
        .checked_add(Map::<Token, Token>::worst_case(limits.host.slots)?)?
        .checked_add(Map::<(Token, Token), [u8; 16]>::worst_case(callbacks)?)?
        .checked_add(Map::<Box<[u8]>, HostCall>::worst_case(calls)?)?;
    // A complete assignment includes its charter, transcript, grants and
    // settled call tail. These bytes also bound any other concrete output.
    let assignment = limits
        .host
        .charter_bytes
        .checked_add(limits.host.transcript_bytes)?
        .checked_add(u64::from(limits.core.call_records).checked_mul(u64::from(limits.core.call_answer_bytes))?)?
        .checked_add(u64::from(limits.host.accounts).checked_mul(64)?)?;
    let value = assignment
        .checked_add(limits.core.policy_bytes)?
        .checked_add(u64::from(limits.core.tasks.batch).checked_mul(u64::from(limits.core.run_bytes))?)?
        .checked_add(limits.host.turn_bytes)?
        .checked_add(limits.host.outcome_bytes)?
        .checked_add(u64::from(limits.observability.answer_bytes))?
        .checked_add(65_536)?;
    let envelopes = u64::from(limits.core.call_records.checked_add(limits.observability.reads)?.checked_add(calls)?)
        .checked_mul(limits.core.fleet.call_name_bytes.checked_add(32)?)?;
    let owned = u64::from(limits.core.tasks.tasks)
        .checked_mul(assignment)?
        .checked_add(u64::from(limits.core.tasks.tasks).checked_mul(u64::from(limits.core.brief.brief_bytes))?)?
        .checked_add(envelopes)?
        .checked_add(u64::from(payloads).checked_mul(value)?)?
        .checked_add(u64::from(effects).checked_mul(u64::from(limits.infrastructure.name_bytes).checked_mul(8)?)?)?
        .checked_add(
            u64::from(limits.agent.smith.endpoints).checked_mul(limits.host.charter_bytes.checked_add(64)?)?,
        )?;
    let queued = limits
        .journal
        .writes
        .checked_add(limits.journal.held)?
        .checked_add(limits.journal.now)?
        .checked_add(limits.routes)?;
    children
        .checked_add(tables)?
        .checked_add(owned)?
        .checked_add(Journal::<Write, Output>::worst_case(&limits.journal)?)?
        .checked_add(Queue::<Work>::worst_case(limits.routes)?)?
        .checked_add(u64::from(queued).checked_mul(value)?)?
        // Tokenization and the bounded JSON nesting stack coexist with a
        // decoded tool input only during its synchronous translation.
        .checked_add(67_108_864)
}
