//! An ordered domain fake of the store boundary (`domain/testing.md`, 4).
//! Rows live in one ordered map. Whole commits wait in flight, apply in
//! number order, and can have their completion held or failed. Pages are
//! captured from durable rows and may be delivered after a chosen delay.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use jig_test_domain::{Key, Record, Write};
use std::collections::{BTreeMap, VecDeque};

/// A numbered transaction waiting for the fake store to apply it.
#[derive(Debug)]
pub struct Pending {
    /// Journal-issued commit number.
    pub number: u64,
    /// Whole atomic write set.
    pub writes: Box<[Write]>,
    /// Ticks before application is allowed.
    pub delay: u32,
}

/// One captured key-ordered page, possibly delivered later.
#[derive(Debug)]
pub struct Page {
    /// Durable rows from one read instant.
    pub rows: Box<[Record]>,
    /// Exclusive cursor if more rows remained at that instant.
    pub next: Option<Key>,
    /// Ticks before the caller may receive the page.
    pub delay: u32,
}

/// Ordered durable rows and commit flights.
#[derive(Debug, Default)]
pub struct Store {
    /// Last transaction whose whole write set applied.
    pub applied: u64,
    /// Durable typed rows, ordered by their keys.
    pub rows: BTreeMap<Key, Record>,
    /// Submitted transactions not yet applied.
    pub pending: VecDeque<Pending>,
    /// Applied completions held from the caller.
    pub held: VecDeque<u64>,
    /// A later application fails at this number, if selected.
    pub fail_at: Option<u64>,
}

impl Store {
    /// Construct an empty store for a first deployment.
    #[must_use]
    pub fn new() -> Store {
        Store::default()
    }

    /// Queue one whole numbered transaction.
    pub fn submit(&mut self, number: u64, writes: Box<[Write]>, delay: u32) {
        let previous = self.pending.back().map_or(self.applied, |pending| pending.number);
        assert_eq!(number, previous + 1, "submissions keep journal order");
        self.pending.push_back(Pending { number, writes, delay });
    }

    /// Advance one store tick; return a completion unless the caller holds it.
    /// A selected failure leaves all of this transaction's rows unapplied.
    ///
    /// # Errors
    /// Returns the failed transaction number when the selected application fails.
    pub fn tick(&mut self, hold_completion: bool) -> Result<Option<u64>, u64> {
        let Some(front) = self.pending.front_mut() else { return Ok(None) };
        if front.delay != 0 {
            front.delay -= 1;
            return Ok(None);
        }
        let pending = self.pending.pop_front().expect("ready transaction");
        if self.fail_at == Some(pending.number) {
            self.pending.clear();
            return Err(pending.number);
        }
        assert_eq!(pending.number, self.applied + 1, "fake applies in order");
        for write in pending.writes {
            match write {
                Write::Save(row) => {
                    self.rows.insert(row.key(), row);
                }
                Write::Erase(key) => {
                    self.rows.remove(&key);
                }
            }
        }
        self.applied = pending.number;
        if hold_completion {
            self.held.push_back(pending.number);
            Ok(None)
        } else {
            Ok(Some(pending.number))
        }
    }

    /// Release the oldest withheld acknowledgement after the rows are durable.
    pub fn release_held(&mut self) -> Option<u64> {
        self.held.pop_front()
    }

    /// Capture one ordered page from currently durable rows.
    #[must_use]
    pub fn page(&self, first: &Key, last: &Key, after: Option<&Key>, most: u32, delay: u32) -> Page {
        assert!(most > 0, "a store page has positive room");
        let count = usize::try_from(most).expect("page count fits usize");
        let mut rows = Vec::new();
        for (key, row) in self.rows.range(first.clone()..=last.clone()) {
            if after.is_none_or(|previous| key > previous) {
                rows.push(row.clone());
                if rows.len() > count {
                    break;
                }
            }
        }
        let more = rows.len() > count;
        if more {
            rows.pop();
        }
        let next = if more { Some(rows.last().expect("positive page").key()) } else { None };
        Page { rows: rows.into_boxed_slice(), next, delay }
    }
}
