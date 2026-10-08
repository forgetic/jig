//! Checked retained and in-flight heap bounds for the core
//! (`domain/engine.md`, sections 5.6 and 10; `domain/root.md`, section 10).
//! Child bounds cover their own state. This module prices the core's route
//! tables, owned payload copies, store loads and one complete decision.

use alloc::boxed::Box;
use core::mem::size_of;
use jig_core_accounts as accounts;
use jig_core_authority as authority;
use jig_core_brief as brief;
use jig_core_fleet as fleet;
use jig_core_notes as notes;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_views as views;
use skein_lib::{List, Map, Queue};

use crate::{CallKey, Event, HistoricalResult, Limits, Request};

pub(crate) fn policy_owned_bytes(policy: &crate::RunPolicy) -> Option<u64> {
    let mut bytes = sizeof(size_of::<crate::RunPolicy>())?
        .checked_add(u64::try_from(policy.instructions.len()).ok()?)?
        .checked_add(u64::try_from(policy.model.name.len()).ok()?)?
        .checked_add(u64::try_from(policy.alternatives.len()).ok()?.checked_mul(sizeof(size_of::<crate::Model>())?)?)?;
    for model in &policy.alternatives {
        bytes = bytes.checked_add(u64::try_from(model.name.len()).ok()?)?;
    }
    Some(bytes)
}

fn bytes(count: u32, each: u64) -> Option<u64> {
    u64::from(count).checked_mul(each)
}

fn sizeof(value: usize) -> Option<u64> {
    u64::try_from(value).ok()
}

fn task_carrier(limits: &Limits) -> Option<u64> {
    let task = &limits.tasks;
    let names = bytes(
        task.holdings,
        sizeof(size_of::<tasks::Holding>())?
            .checked_add(bytes(task.hold_segments, sizeof(size_of::<Box<[u8]>>())?)?)?
            .checked_add(u64::from(task.hold_bytes))?,
    )?;
    let authority = bytes(
        task.authority_grants,
        sizeof(size_of::<tasks::Grant>())?
            .checked_add(bytes(task.authority_segments.checked_mul(2)?, sizeof(size_of::<Box<[u8]>>())?)?)?,
    )?
    .checked_add(u64::from(task.authority_bytes))?
    .checked_add(bytes(task.executor_kinds, sizeof(size_of::<tasks::AuthorityExecutor>())?)?)?
    .checked_add(bytes(task.authority_grants, sizeof(size_of::<tasks::ResourceScope>())?)?)?;
    sizeof(size_of::<tasks::New>())?
        .checked_add(u64::from(task.spec_bytes))?
        .checked_add(bytes(task.parameters, sizeof(size_of::<tasks::Parameter>())?)?)?
        .checked_add(bytes(task.inputs, sizeof(size_of::<u64>())?)?)?
        .checked_add(bytes(task.dependencies, sizeof(size_of::<u64>())?)?)?
        .checked_add(bytes(task.contract_choices, sizeof(size_of::<tasks::Verdict>())?)?)?
        .checked_add(u64::from(task.result_bytes))?
        .checked_add(names)?
        .checked_add(authority)
}

fn context_carrier(limits: &Limits) -> Option<u64> {
    let task = &limits.tasks;
    task_carrier(limits)?
        .checked_add(sizeof(size_of::<tasks::RunContext>())?)?
        .checked_add(bytes(task.inbox_messages, sizeof(size_of::<tasks::Word>())?)?)?
        .checked_add(u64::from(task.inbox_bytes))?
        .checked_add(bytes(task.delegates, sizeof(size_of::<tasks::DelegateState>())?)?)?
        .checked_add(bytes(task.saved_resources, sizeof(size_of::<tasks::SavedResource>())?)?)?
        .checked_add(u64::from(task.result_bytes))
}

/// Pure checked heap ceiling for the core's eight children, its retained
/// route state, loads and the largest synchronous decision. Invalid child
/// limits or unrepresentable arithmetic return `None`; a root adds its own
/// journal and connectors when pricing the whole application.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one checked sum names each retained and in-flight owner")]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.resume_bytes == 0
        || limits.run_bytes == 0
        || limits.call_records == 0
        || limits.notes.scopes < 3_u32.checked_add(limits.authority.grants)?
    {
        return None;
    }
    let route = crate::routing::room_max(limits)?;
    let children = [
        tasks::worst_case(&limits.tasks)?,
        authority::worst_case(&limits.authority)?,
        people::worst_case(&limits.people)?,
        fleet::worst_case(&limits.fleet)?,
        brief::gather_worst_case(&limits.brief)?,
        accounts::worst_case(&limits.accounts)?,
        notes::worst_case(&limits.notes)?,
        views::worst_case(&limits.views)?,
    ];
    let mut total = 0_u64;
    for child in children {
        total = total.checked_add(child)?;
    }
    // Every inline map value is at most an Event, and every key at most a
    // CallKey. Boxed values are charged separately below.
    let largest = size_of::<Event>();
    for value in [
        size_of::<crate::GoalRoute>(),
        size_of::<crate::PersonTaskRoute>(),
        size_of::<crate::PersonProposalRoute>(),
        size_of::<crate::RoutedCall>(),
        size_of::<crate::routing::NoteRoute>(),
        size_of::<crate::CallPart>(),
        size_of::<crate::routing::Creation>(),
        size_of::<crate::routing::DelegateCreation>(),
        size_of::<crate::routing::ProcedureCreation>(),
        size_of::<crate::run::Transcript>(),
        size_of::<crate::RunProof>(),
    ] {
        if value > largest {
            return None;
        }
    }
    for capacity in [
        limits.views.watchers,     // watching
        limits.tasks.tasks,        // view phases
        limits.load_slots,         // reads
        limits.tasks.tasks,        // dependency results
        limits.people.pending,     // made
        limits.people.pending,     // goal routes
        limits.fleet.calls,        // delegation
        limits.people.pending,     // proposal routes
        limits.tasks.tasks,        // ending positions
        limits.people.pending,     // messages
        limits.people.pending,     // moves
        limits.people.pending,     // person tasks
        limits.people.pending,     // person escalations
        limits.people.pending,     // creations
        limits.call_records,       // delegate creations
        limits.tasks.tasks,        // procedure creations
        limits.authority.projects, // permission roles
        limits.call_records,       // pending calls
        limits.call_records,       // answered calls
        limits.fleet.calls,        // routed calls
        limits.brief.briefs,       // briefs awaiting notes
        limits.tasks.tasks,        // contexts
        limits.tasks.tasks,        // workspaces
        limits.tasks.tasks,        // transcripts
        limits.tasks.tasks,        // proofs
        limits.tasks.tasks,        // restoring proofs
        limits.tasks.tasks,        // unreported restored claims
        limits.tasks.tasks,        // claims
    ] {
        total = total.checked_add(Map::<CallKey, Event>::worst_case(capacity)?)?;
    }
    total = total.checked_add(List::<u32>::worst_case(limits.authority.projects)?)?;
    total = total.checked_add(Map::<skein_lib::Token, crate::routing::NoteRoute>::worst_case(2)?)?;
    total = total.checked_add(Map::<u64, bool>::worst_case(limits.brief.briefs)?)?;
    total = total.checked_add(bytes(limits.connectors, sizeof(size_of::<u16>())?)?)?;
    total = total.checked_add(Queue::<Box<tasks::RunContext>>::worst_case(limits.tasks.tasks)?)?;
    total = total.checked_add(Queue::<fleet::Event>::worst_case(limits.tasks.tasks.checked_mul(2)?)?)?;

    let task = &limits.tasks;
    let new_task = task_carrier(limits)?;
    let context = context_carrier(limits)?;
    let historical = sizeof(size_of::<HistoricalResult>())?.checked_add(u64::from(task.result_bytes))?;
    let histories = task.dependencies.checked_add(task.inputs)?;
    total = total.checked_add(bytes(task.tasks, bytes(histories, historical)?)?)?;
    total = total.checked_add(bytes(task.tasks.checked_mul(2)?, context)?)?;
    let creations = limits
        .people
        .pending
        .checked_add(limits.call_records.checked_mul(task.batch)?)?
        .checked_add(task.tasks.checked_mul(task.batch)?)?;
    total = total.checked_add(bytes(creations, new_task)?)?;
    total = total.checked_add(new_task)?;
    total = total.checked_add(bytes(
        limits.fleet.calls.checked_add(limits.load_slots)?,
        bytes(task.batch.checked_mul(task.inputs)?, sizeof(size_of::<tasks::Stub>())?)?,
    )?)?;
    total = total.checked_add(bytes(
        task.tasks,
        Queue::<Box<[u8]>>::worst_case(limits.resume_bytes)?.checked_add(u64::from(limits.resume_bytes))?,
    )?)?;
    total = total.checked_add(bytes(limits.authority.projects, limits.policy_bytes)?)?;
    total = total.checked_add(limits.policy_bytes.checked_mul(2)?)?;
    total = total.checked_add(u64::from(limits.run_bytes))?;
    total = total.checked_add(bytes(
        limits.call_records,
        bytes(task.batch, sizeof(size_of::<u64>())?)?
            .checked_add(bytes(authority::max_out(&limits.authority)?, sizeof(size_of::<authority::Finding>())?)?)?,
    )?)?;
    let note_pattern =
        u64::from(limits.notes.pattern_bytes).checked_mul(sizeof(size_of::<Box<[u8]>>())?.checked_add(1)?)?;
    let note_entry = sizeof(size_of::<notes::Entry>())?
        .checked_add(note_pattern)?
        .checked_add(u64::from(limits.notes.description_bytes))?
        .checked_add(u64::from(limits.notes.body_bytes))?
        .checked_add(List::<u64>::worst_case(limits.notes.references)?)?;
    total = total.checked_add(bytes(limits.call_records, bytes(limits.notes.recalled, note_entry)?)?)?;
    total = total.checked_add(bytes(task.tasks, u64::from(task.result_bytes).checked_mul(3)?)?)?;
    total = total.checked_add(u64::from(task.message_bytes))?;

    // One decision may retain a routed event, a request, an owned row/load
    // copy and a connector handoff at each bounded route position.
    let payload = new_task
        .checked_mul(u64::from(task.batch.max(1)))?
        .checked_add(context)?
        .checked_add(limits.policy_bytes)?
        .checked_add(u64::from(limits.run_bytes))?
        .checked_add(u64::from(limits.brief.brief_bytes))?
        .checked_add(u64::from(limits.resume_bytes))?
        .checked_add(u64::from(limits.people.amendment_bytes))?
        .checked_add(u64::from(limits.people.identity_bytes))?
        .checked_add(u64::from(limits.people.words))?
        .checked_add(u64::from(limits.notes.body_bytes))?
        .checked_add(u64::from(limits.views.snapshot_bytes))?;
    total = total.checked_add(Queue::<Event>::worst_case(route.writes)?)?;
    total = total.checked_add(Queue::<Request>::worst_case(route.writes.checked_add(1)?)?)?;
    total = total.checked_add(bytes(route.writes, payload)?)?;
    total = total.checked_add(bytes(limits.load_slots, payload)?)?;
    Some(total)
}
