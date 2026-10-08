//! Owner policy amendments and committed snapshots (domain/people.md,
//! section 5.2; domain/authority.md, sections 6 and 10). The people child
//! retains requests by key; the root checks and applies these values.

use alloc::boxed::Box;
use core::mem::size_of;
use skein_lib::Duration;

use crate::{Authority, Last, Pattern};

/// A keyed edit to one mutable part of a project's policy.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PolicyChange {
    /// Replace project spend for future periods and decisions.
    ProjectSpend { period_spend: u64 },
    /// Replace one role's authority, allotment and proposal decisions.
    Role(PolicyRole),
    /// Replace the project's system-neutral effect requirements.
    Requirements { requirements: Box<[Requirement]> },
    /// Replace the grants for writes projecting this project's goals.
    Projections { grants: Box<[crate::Grant]> },
    /// Replace the map from a connector's permission number to a project role.
    Permissions { mappings: Box<[PermissionRole]> },
}

/// Mutable project policy saved as one full committed value.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PolicyValue {
    pub period_spend: u64,
    /// Grants for goal projection writes.
    pub projections: Box<[crate::Grant]>,
    pub roles: Box<[PolicyRole]>,
    pub requirements: Box<[Requirement]>,
    pub permissions: Box<[PermissionRole]>,
}

/// One role's grant, funding and request decision authority.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PolicyRole {
    pub number: u32,
    pub authority: Authority,
    pub period_spend: u64,
    pub requests: u16,
    pub decides: u8,
}

/// A connector's permission mapped to an existing project role at adoption.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PermissionRole {
    pub connector: u16,
    pub permission: u16,
    pub role: u32,
}

/// A connector-owned judge selected by project policy.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Judge {
    pub connector: u16,
    pub requirement: u16,
    pub parameters: u32,
}

/// Whether the connector checks a verdict at application or it was observed earlier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Guard {
    Guarded,
    Observed { freshness: Duration },
}

/// One authority requirement on an effect kind and resource pattern.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Requirement {
    pub connector: u16,
    pub kind: u16,
    pub pattern: Pattern,
    pub judge: Judge,
    pub guard: Guard,
    pub must_be_guarded: bool,
}

/// Checked deep-byte size of one owner policy payload.
#[must_use]
pub fn policy_bytes(policy: &PolicyValue) -> Option<u64> {
    let mut total =
        u64::try_from(policy.roles.len()).ok()?.checked_mul(u64::try_from(size_of::<PolicyRole>()).ok()?)?;
    for role in &policy.roles {
        total = total.checked_add(crate::authority_bytes(&role.authority)?)?;
    }
    total = total.checked_add(grants_bytes(&policy.projections)?)?;
    total = total.checked_add(requirements_bytes(&policy.requirements)?)?;
    total.checked_add(
        u64::try_from(policy.permissions.len()).ok()?.checked_mul(u64::try_from(size_of::<PermissionRole>()).ok()?)?,
    )
}

/// Checked deep-byte size of system-neutral requirements.
#[must_use]
pub fn requirements_bytes(requirements: &[Requirement]) -> Option<u64> {
    let mut total =
        u64::try_from(requirements.len()).ok()?.checked_mul(u64::try_from(size_of::<Requirement>()).ok()?)?;
    for requirement in requirements {
        let pattern = &requirement.pattern;
        total = total.checked_add(
            u64::try_from(pattern.segments.len()).ok()?.checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
        )?;
        for segment in &pattern.segments {
            total = total.checked_add(u64::try_from(segment.len()).ok()?)?;
        }
        total = total.checked_add(match &pattern.last {
            Last::Exact(word) | Last::Open(word) => u64::try_from(word.len()).ok()?,
        })?;
    }
    Some(total)
}

/// Checked deep-byte size of one keyed amendment.
#[must_use]
pub fn policy_change_bytes(change: &PolicyChange) -> Option<u64> {
    match change {
        PolicyChange::ProjectSpend { .. } => Some(0),
        PolicyChange::Projections { grants } => grants_bytes(grants),
        PolicyChange::Role(role) => crate::authority_bytes(&role.authority),
        PolicyChange::Requirements { requirements } => requirements_bytes(requirements),
        PolicyChange::Permissions { mappings } => {
            u64::try_from(mappings.len()).ok()?.checked_mul(u64::try_from(size_of::<PermissionRole>()).ok()?)
        }
    }
}

fn grants_bytes(grants: &[crate::Grant]) -> Option<u64> {
    let mut total = u64::try_from(grants.len()).ok()?.checked_mul(u64::try_from(size_of::<crate::Grant>()).ok()?)?;
    for grant in grants {
        total = total.checked_add(
            u64::try_from(grant.pattern.segments.len())
                .ok()?
                .checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
        )?;
        for segment in &grant.pattern.segments {
            total = total.checked_add(u64::try_from(segment.len()).ok()?)?;
        }
        total = total.checked_add(match &grant.pattern.last {
            Last::Exact(value) | Last::Open(value) => u64::try_from(value.len()).ok()?,
        })?;
    }
    Some(total)
}
