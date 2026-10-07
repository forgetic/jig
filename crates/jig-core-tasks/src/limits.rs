//! Root-configured admission and memory bounds (domain/tasks.md, section 10). Counts retained live state and bounded scratch; parents count
//! their owned input/output copies separately. This module performs no admission
//! mutation, authority decision or allocation during the bound calculation.
use crate::domain::Task;
use crate::{AuthorityExecutor, Class, Fact, Grant, Parameter, Retries, Verdict};
use alloc::boxed::Box;
use skein_lib::{Deadlines, Id, List, Map, Queue, Slab};

/// Immutable root-configured limits for live task state, finite sources, owned semantic payloads
/// and retry policies; validate through `worst_case` before construction. (domain/tasks.md, section 10).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Maximum retained live task slots, name-index entries and retry alarms.
    pub tasks: u32,
    /// Maximum retained finite period, pool and recurring allotment identities.
    pub funders: u32,
    /// Maximum live tasks per project, including every member of an admitted batch.
    pub project_tasks: u32,
    /// Maximum lifetime tasks made in a requester tree, including its root; completed delegates do
    /// not return this capacity.
    pub tree_tasks: u32,
    /// Maximum structural depth below the requester-tree root, which has depth zero.
    pub depth: u32,
    /// Maximum live direct delegates of one requester task.
    pub delegates: u32,
    /// Maximum introduced peer references per live task.
    pub references: u32,
    /// Maximum standing interests per task.
    pub subscriptions: u32,
    /// Maximum directly created tasks in one atomic nonempty `Make` batch.
    pub batch: u32,
    /// Maximum immutable dependency identities per task; also bounds its remaining live
    /// `waiting_on` subset.
    pub dependencies: u32,
    /// Maximum resources required by one task.
    pub holdings: u32,
    /// Maximum connector resource kinds installed in this deployment.
    pub hold_kinds: u32,
    /// Maximum literal segments in one resource name.
    pub hold_segments: u32,
    /// Maximum aggregate literal bytes in one resource name.
    pub hold_bytes: u32,
    /// Maximum tasks waiting on any one resource.
    pub hold_waiters: u32,
    /// Wall-clock bound before a task waiting for holds is held for review.
    pub hold_wait: skein_lib::Duration,
    /// Shape bound for typed historical input identities checked by the root before delegation.
    pub inputs: u32,
    /// Maximum combined specification words and byte-valued parameter bytes per task.
    pub spec_bytes: u32,
    /// Maximum typed parameters per specification.
    pub parameters: u32,
    /// Maximum result/reason bytes; contracts cannot allow larger results and cancellation may
    /// retain one bounded reason plus one bounded partial result.
    pub result_bytes: u32,
    /// Maximum whole unread word messages kept by one live task.
    pub inbox_messages: u32,
    /// Maximum total unread word bytes kept by one live task.
    pub inbox_bytes: u32,
    /// Maximum bytes in one admitted word message.
    pub message_bytes: u32,
    /// Time an undecided proposal waits at a nonfinal holder before passing upward.
    pub proposal_stall: skein_lib::Duration,
    /// Time an undecided escalation waits at a nonfinal holder before passing upward.
    pub escalation_stall: skein_lib::Duration,
    /// Maximum connector resources on which a task has saved state.
    pub saved_resources: u32,
    /// Maximum distinct-code choices in a nonempty verdict contract.
    pub contract_choices: u32,
    /// Maximum retained configured agent-charter numbers.
    pub charters: u32,
    /// Maximum resource grants per task authority carrier.
    pub authority_grants: u32,
    /// Maximum base segments per carried grant pattern.
    pub authority_segments: u32,
    /// Maximum total bytes across every grant's base and terminal segments in one authority
    /// carrier.
    pub authority_bytes: u32,
    /// Maximum carried authority executor-permission entries.
    pub executor_kinds: u32,
    /// Validated per-class failure retry and pause policies.
    pub retries: Retries,
    /// Optional content-free observation capacity; overflow is diagnostic and changes no decision.
    pub facts: u32,
}

/// Maximum ended identities named by bounded live dependencies, inputs or references.
#[must_use]
pub(crate) fn stub_capacity(limits: &Limits) -> Option<u32> {
    limits.tasks.checked_mul(limits.dependencies.checked_add(limits.inputs)?.checked_add(limits.references)?)
}

/// Pure checked heap bound for live containers and nested payloads, finite ledgers, charter
/// configuration, optional facts and bounded graph/traversal scratch under `limits`. Returns `None`
/// on overflow or invalid task/batch/tree/retry settings. Validates output-bound arithmetic; caller
/// separately counts incoming data, `RunContext`/saved-row copies and `Request` queues.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one checked bound includes every retained task and hold container")]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    crate::domain::output_bound(limits)?;
    if limits.tasks == 0
        || limits.batch == 0
        || limits.tree_tasks == 0
        || limits.inbox_messages == 0
        || limits.inbox_bytes == 0
        || limits.message_bytes == 0
        || limits.message_bytes > limits.inbox_bytes
        || limits.hold_wait == skein_lib::Duration::ZERO
        || limits.proposal_stall == skein_lib::Duration::ZERO
        || limits.escalation_stall == skein_lib::Duration::ZERO
    {
        return None;
    }
    for class in [Class::Transient, Class::Permanent, Class::Run, Class::Agent, Class::Lost, Class::Invalid] {
        let retry = limits.retries.of(class);
        if retry.retries == u32::MAX || retry.base == skein_lib::Duration::ZERO || retry.max < retry.base {
            return None;
        }
    }
    let segments = u64::from(limits.authority_grants)
        .checked_mul(2)?
        .checked_mul(u64::from(limits.authority_segments))?
        .checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?;
    let payload = u64::from(limits.spec_bytes)
        .checked_add(u64::from(limits.inbox_bytes))?
        .checked_add(u64::from(limits.message_bytes))?
        .checked_add(u64::try_from(size_of::<crate::Word>()).ok()?)?
        .checked_add(u64::from(limits.inbox_messages).checked_mul(u64::try_from(size_of::<crate::Word>()).ok()?)?)?
        .checked_add(
            u64::from(limits.inbox_messages).checked_mul(u64::try_from(size_of::<crate::QuestionCredit>()).ok()?)?,
        )?
        .checked_add(
            u64::from(limits.saved_resources).checked_mul(
                u64::try_from(size_of::<crate::SavedResource>())
                    .ok()?
                    .checked_add(
                        u64::from(limits.authority_segments)
                            .checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
                    )?
                    .checked_add(u64::from(limits.authority_bytes))?,
            )?,
        )?
        .checked_add(u64::from(limits.result_bytes).checked_mul(3)?)?
        .checked_add(u64::from(limits.parameters).checked_mul(u64::try_from(size_of::<Parameter>()).ok()?)?)?
        .checked_add(u64::from(limits.inputs).checked_mul(8)?)?
        .checked_add(u64::from(limits.dependencies).checked_mul(16)?)?
        .checked_add(
            u64::from(limits.holdings).checked_mul(
                u64::try_from(size_of::<crate::Holding>())
                    .ok()?
                    .checked_add(
                        u64::from(limits.hold_segments).checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
                    )?
                    .checked_add(u64::from(limits.hold_bytes))?,
            )?,
        )?
        .checked_add(u64::from(limits.delegates).checked_mul(8)?)?
        .checked_add(u64::from(limits.references).checked_mul(8)?)?
        .checked_add(
            u64::from(limits.subscriptions).checked_mul(u64::try_from(size_of::<crate::Subscription>()).ok()?)?,
        )?
        .checked_add(u64::from(limits.contract_choices).checked_mul(u64::try_from(size_of::<Verdict>()).ok()?)?)?
        .checked_add(u64::from(limits.authority_grants).checked_mul(u64::try_from(size_of::<Grant>()).ok()?)?)?
        .checked_add(
            u64::from(limits.authority_grants).checked_mul(u64::try_from(size_of::<crate::ResourceScope>()).ok()?)?,
        )?
        .checked_add(segments)?
        .checked_add(u64::from(limits.authority_bytes))?
        .checked_add(
            u64::from(limits.executor_kinds).checked_mul(u64::try_from(size_of::<AuthorityExecutor>()).ok()?)?,
        )?;
    let proposed_member = u64::try_from(size_of::<crate::New>())
        .ok()?
        .checked_add(u64::from(limits.spec_bytes))?
        .checked_add(u64::from(limits.authority_bytes))?
        .checked_add(u64::from(limits.dependencies).checked_mul(8)?)?
        .checked_add(
            u64::from(limits.holdings).checked_mul(
                u64::try_from(size_of::<crate::Holding>())
                    .ok()?
                    .checked_add(
                        u64::from(limits.hold_segments).checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
                    )?
                    .checked_add(u64::from(limits.hold_bytes))?,
            )?,
        )?
        .checked_add(u64::from(limits.contract_choices).checked_mul(u64::try_from(size_of::<Verdict>()).ok()?)?)?;
    let proposal_payload = u64::try_from(size_of::<crate::Proposal>())
        .ok()?
        .checked_add(u64::from(limits.message_bytes).checked_mul(2)?)?
        .checked_add(u64::from(limits.batch).checked_mul(proposed_member)?)?;
    // The writer map owns both its opaque key and a durable slot row with a
    // second copy of that name. Count their nested segment vectors and bytes.
    let writer_names = u64::from(limits.tasks).checked_mul(u64::from(limits.holdings))?.checked_mul(2)?.checked_mul(
        u64::from(limits.hold_segments)
            .checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?
            .checked_add(u64::from(limits.hold_bytes))?,
    )?;
    Slab::<Task>::worst_case(limits.tasks)?
        .checked_add(Map::<crate::Funder, crate::FundingRecord>::worst_case(limits.funders)?)?
        .checked_add(Map::<u64, Id<Task>>::worst_case(limits.tasks)?)?
        .checked_add(Map::<u64, crate::Stub>::worst_case(stub_capacity(limits)?)?)?
        .checked_add(Deadlines::<u64>::worst_case(limits.tasks)?)?
        .checked_add(Deadlines::<u64>::worst_case(limits.tasks)?)?
        .checked_add(Map::<crate::holds::KindKey, crate::HoldKind>::worst_case(limits.hold_kinds)?)?
        .checked_add(Map::<crate::Name, crate::WriterSlot>::worst_case(limits.tasks.checked_mul(limits.holdings)?)?)?
        .checked_add(writer_names)?
        .checked_add(Deadlines::<u64>::worst_case(limits.tasks)?)?
        .checked_add(Deadlines::<u64>::worst_case(limits.tasks)?)?
        .checked_add(Deadlines::<u64>::worst_case(limits.tasks.checked_mul(limits.subscriptions)?)?)?
        .checked_add(Deadlines::<u64>::worst_case(limits.tasks)?)?
        .checked_add(Queue::<Fact>::worst_case(limits.facts)?)?
        .checked_add(
            u64::from(limits.tasks).checked_mul(
                payload
                    .checked_add(proposal_payload)?
                    .checked_add(proposal_payload)?
                    .checked_add(u64::try_from(size_of::<crate::RecurringState>()).ok()?)?,
            )?,
        )?
        .checked_add(u64::from(limits.charters).checked_mul(4)?)?
        // Bounded graph/admission and traversal snapshots; no recursive walk.
        .checked_add(List::<u64>::worst_case(limits.tasks.max(limits.batch))?.checked_mul(3)?)?
        .checked_add(List::<u32>::worst_case(limits.batch)?)?
        .checked_add(List::<u64>::worst_case(limits.delegates)?)
}
