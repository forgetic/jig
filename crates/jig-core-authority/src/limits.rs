//! Admission, queue room and owned policy memory (domain/authority.md, 6–10).

use alloc::boxed::Box;
use core::mem::size_of;

use skein_lib::Map;

use crate::{Authority, Executor, Grant, Implication, Name, Pattern, Policy, Requirement, ResourceScope, Role};

/// Root-configured capacities for policy state and admitted questions; owned values do not enforce
/// these bounds until admission. (domain/authority.md, sections 6–11).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Maximum live project policies.
    pub projects: u32,
    /// Maximum roles per project policy.
    pub roles: u32,
    /// Maximum generic requirements per deployment or project policy.
    pub requirements: u32,
    /// Maximum supplied verdicts and guards per effect question.
    pub facts: u32,
    /// Maximum grants per admitted authority value.
    pub grants: u32,
    /// Maximum permitted executor entries per admitted authority value.
    pub executors: u32,
    /// Maximum base or resource-name segment count.
    pub segments: u32,
    /// Maximum bytes per base segment or terminal value.
    pub segment_bytes: u32,
    /// Maximum retained connector-kind implication pairs.
    pub implications: u32,
    /// Maximum directly delegated tasks per batch question.
    pub batch: u32,
    /// Maximum model-account usability reports per run question.
    pub accounts: u32,
    /// Maximum written resources per run question.
    pub writes: u32,
}

/// Room for the largest check, including every independent reason. Checked free-slot bound for any
/// one `check_*` finding output; caller reserves that room before the query. Returns `None` on
/// bound arithmetic overflow; policy lifecycle outputs use `POLICY_MAX_OUT` separately.
#[must_use]
pub fn max_out(limits: &Limits) -> Option<u32> {
    let batch = limits.batch.checked_mul(9)?.checked_add(12)?;
    let run = limits.writes.checked_mul(5)?.checked_add(limits.accounts)?.checked_add(12)?;
    let effect = limits.requirements.checked_mul(4)?.checked_add(8)?;
    Some(batch.max(run).max(effect))
}

/// Heap held by the domain's rules and full project table. Questions and finding queues belong to
/// the caller and are counted there. Checked retained-heap bound for deployment rules and a full
/// live policy table under `limits`; `None` means unrepresentable arithmetic. Caller separately
/// counts question payloads, constructed needs and output queues.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    let authority = authority_heap(limits)?;
    let requirements = requirement_heap(limits)?;
    let role_heap = bytes(u64::from(limits.roles), size_of::<Role>())?
        .checked_add(authority.checked_mul(u64::from(limits.roles))?)?;
    let policy = authority.checked_add(requirements)?.checked_add(role_heap)?;
    let configured = authority
        .checked_add(requirements)?
        .checked_add(bytes(u64::from(limits.implications), size_of::<Implication>())?)?;
    Map::<u32, Policy>::worst_case(limits.projects)?
        .checked_add(policy.checked_mul(u64::from(limits.projects))?)?
        .checked_add(configured)
}

fn bytes(count: u64, size: usize) -> Option<u64> {
    count.checked_mul(u64::try_from(size).ok()?)
}

fn pattern_heap(limits: &Limits) -> Option<u64> {
    let segment = u64::from(limits.segment_bytes).checked_add(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?;
    u64::from(limits.segments).checked_mul(segment)?.checked_add(u64::from(limits.segment_bytes))
}

fn authority_heap(limits: &Limits) -> Option<u64> {
    let grants = u64::from(limits.grants)
        .checked_mul(u64::try_from(size_of::<Grant>()).ok()?.checked_add(pattern_heap(limits)?)?)?;
    let notes = u64::from(limits.grants)
        .checked_mul(u64::try_from(size_of::<ResourceScope>()).ok()?.checked_add(pattern_heap(limits)?)?)?;
    grants.checked_add(notes)?.checked_add(bytes(u64::from(limits.executors), size_of::<Executor>())?)
}

fn requirement_heap(limits: &Limits) -> Option<u64> {
    let one = u64::try_from(size_of::<Requirement>()).ok()?.checked_add(pattern_heap(limits)?)?;
    u64::from(limits.requirements).checked_mul(one)
}

pub(crate) fn within(len: usize, limit: u32) -> bool {
    match u32::try_from(len) {
        Ok(len) => len <= limit,
        Err(_) => false,
    }
}

pub(crate) fn pattern_within(pattern: &Pattern, limits: &Limits) -> bool {
    if !name_within_segments(&pattern.segments, limits) {
        return false;
    }
    match &pattern.last {
        crate::Last::Exact(bytes) | crate::Last::Open(bytes) => within(bytes.len(), limits.segment_bytes),
    }
}

pub(crate) fn name_within(name: &Name, limits: &Limits) -> bool {
    name_within_segments(&name.segments, limits)
}

fn name_within_segments(segments: &[Box<[u8]>], limits: &Limits) -> bool {
    if !within(segments.len(), limits.segments) {
        return false;
    }
    for segment in segments {
        if !within(segment.len(), limits.segment_bytes) {
            return false;
        }
    }
    true
}

pub(crate) fn authority_within(authority: &Authority, limits: &Limits) -> bool {
    if !within(authority.grants.len(), limits.grants)
        || !within(authority.delegation.kinds.len(), limits.executors)
        || !within(authority.note_resources.len(), limits.grants)
        || authority.notes.0 & !7 != 0
    {
        return false;
    }
    for grant in &authority.grants {
        if !pattern_within(&grant.pattern, limits) {
            return false;
        }
    }
    for scope in &authority.note_resources {
        if !pattern_within(&scope.pattern, limits) {
            return false;
        }
    }
    true
}

pub(crate) fn requirements_within(requirements: &[Requirement], limits: &Limits) -> bool {
    if !within(requirements.len(), limits.requirements) {
        return false;
    }
    for requirement in requirements {
        if !pattern_within(&requirement.pattern, limits) {
            return false;
        }
    }
    true
}
