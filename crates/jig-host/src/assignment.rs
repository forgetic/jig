//! Admission bounds for a hosted run (domain/hosts.md, section 6.2).
//! Workspace items belong to the application and stay opaque here.

use crate::boundary::{Assignment, Invalid};
use crate::limits::Limits;

/// Whether `assignment` fits `limits`, and what about it does not.
pub(crate) fn check(assignment: &Assignment, limits: &Limits) -> Result<(), Invalid> {
    if len(&assignment.charter) > limits.charter_bytes {
        return Err(Invalid::Charter);
    }
    if let Some(snapshot) = &assignment.snapshot
        && len(snapshot) > limits.snapshot_bytes
    {
        return Err(Invalid::Snapshot);
    }
    if u64::try_from(assignment.grants.len()).expect("a length fits") > u64::from(limits.accounts) {
        return Err(Invalid::Grants);
    }
    for (index, grant) in assignment.grants.iter().enumerate() {
        for other in assignment.grants.iter().skip(index.saturating_add(1)) {
            if grant.account == other.account {
                return Err(Invalid::Grants);
            }
        }
    }
    Ok(())
}

/// The length of `bytes`.
pub(crate) fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).expect("a length fits in a u64")
}
