//! Bounds for notes (jig's domain/engine.md, sections 10 and 13).

use alloc::boxed::Box;
use core::mem::size_of;

use skein_lib::{List, Map};

use crate::boundary::{Entry, Line, Record, Scope};
use crate::domain::Pending;
use crate::read::{Cached, ReadPending};

/// The parent's fixed bounds for notes and store pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// Scope indexes kept in memory at once.
    pub scopes: u32,
    /// Entries admitted in one scope.
    pub entries_per_scope: u32,
    /// Largest connector pattern, in bytes.
    pub pattern_bytes: u32,
    /// Largest one-line description, in bytes.
    pub description_bytes: u32,
    /// Largest body, in bytes.
    pub body_bytes: u32,
    /// Task references in one entry.
    pub references: u32,
    /// Records in one store load page.
    pub load_rows: u32,
    /// Index lines in one answer.
    pub lines: u32,
    /// Entries in one recall page.
    pub recalled: u32,
}

/// The maximum heap the pending call and incoming page can hold, or `None`
/// when a bound cannot be represented. Outputs move to the parent.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.scopes == 0 || limits.entries_per_scope == 0 || limits.load_rows == 0 || limits.recalled == 0 {
        return None;
    }
    let _max_lines = limits.scopes.checked_mul(limits.entries_per_scope)?;
    let pattern_room =
        u64::from(limits.pattern_bytes).checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?.checked_add(1)?)?;
    let pending = u64::try_from(size_of::<Pending>())
        .ok()?
        .checked_add(pattern_room)?
        .checked_add(u64::from(limits.description_bytes))?
        .checked_add(u64::from(limits.body_bytes))?
        .checked_add(List::<u64>::worst_case(limits.references)?)?;
    let entry = u64::try_from(size_of::<Entry>())
        .ok()?
        .checked_add(pattern_room)?
        .checked_add(u64::from(limits.description_bytes))?
        .checked_add(u64::from(limits.body_bytes))?
        .checked_add(List::<u64>::worst_case(limits.references)?)?;
    let line = u64::try_from(size_of::<Line>())
        .ok()?
        .checked_add(pattern_room)?
        .checked_add(u64::from(limits.description_bytes))?;
    let page = List::<Record>::worst_case(limits.load_rows)?
        .checked_add(u64::from(limits.load_rows).checked_mul(entry.max(line))?)?;
    let per_index = pattern_room
        .checked_add(Map::<u64, Line>::worst_case(limits.entries_per_scope)?)?
        .checked_add(u64::from(limits.entries_per_scope).checked_mul(line)?)?;
    let indexes = Map::<Scope, Cached>::worst_case(limits.scopes)?
        .checked_add(u64::from(limits.scopes).checked_mul(per_index)?)?;
    let reading = u64::try_from(size_of::<ReadPending>())
        .ok()?
        .checked_add(Map::<u64, Line>::worst_case(limits.entries_per_scope)?)?
        .checked_add(List::<Scope>::worst_case(limits.scopes)?)?
        .checked_add(List::<Line>::worst_case(limits.lines)?)?
        .checked_add(List::<u64>::worst_case(limits.recalled)?)?
        .checked_add(List::<Entry>::worst_case(limits.recalled)?)?
        .checked_add(u64::from(limits.scopes).checked_mul(pattern_room)?)?
        .checked_add(u64::from(limits.lines).checked_mul(line)?)?
        .checked_add(u64::from(limits.recalled).checked_mul(entry)?)?
        .checked_add(u64::from(limits.description_bytes))?;
    // An entry lookup clones its single row while deciding the write.
    indexes.checked_add(pending.max(reading))?.checked_add(page)?.checked_add(entry)
}
