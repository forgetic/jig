//! An ordered domain fake of the store boundary (`domain/testing.md`, 4, 7).
//! The caller translates its writes into opaque keys and rows. Whole commits
//! apply in number order; completions also leave in order, once durable.
//! Held completions, delayed writes and captured pages model separate waits.
//! A failure stops application until a crash; a crash retains durable rows and
//! drops transactions, completions and pages in flight. Restore reads only the
//! requested live ranges; the fake does not interpret a record's contents.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};

/// One opaque mutation in an atomic transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write<K, R> {
    /// Replace the row at this address.
    Save { key: K, row: R },
    /// Remove the row at this address.
    Erase(K),
}

/// A numbered transaction waiting for the fake store to apply it.
#[derive(Debug)]
pub struct Pending<K, R> {
    /// Journal-issued commit number.
    pub number: u64,
    /// Whole atomic write set.
    pub writes: Box<[Write<K, R>]>,
    /// Ticks before application is allowed.
    pub delay: u32,
}

/// One captured key-ordered page, possibly delivered later.
#[derive(Debug)]
pub struct Page<K, R> {
    /// Durable rows from one read instant.
    pub rows: Box<[R]>,
    /// Exclusive cursor if more rows remained at that instant.
    pub next: Option<K>,
    /// Ticks before the caller may receive the page.
    pub delay: u32,
}

/// Faults at the store's domain face; delays use deterministic world ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Withhold the next completions; later ones wait behind them.
    Hold { commits: u32 },
    /// Fail this transaction before applying any of its writes.
    Fail { commit: u64 },
    /// Add this many ticks to every later page request.
    SlowPages { by: u32 },
}

/// Ordered durable rows, transactions and independently scheduled pages.
#[derive(Debug)]
pub struct Store<K, R> {
    /// Last transaction whose whole write set applied.
    pub applied: u64,
    /// Durable opaque rows, ordered by their addresses.
    pub rows: BTreeMap<K, R>,
    /// Submitted transactions not yet applied.
    pub pending: VecDeque<Pending<K, R>>,
    /// Applied completions held from the caller, in commit order.
    pub held: VecDeque<u64>,
    /// A later application fails at this number, if selected.
    pub fail_at: Option<u64>,
    pages: VecDeque<(u64, Page<K, R>)>,
    hold_next: u32,
    page_delay: u32,
    failed: bool,
}

impl<K: Ord + Clone, R: Clone> Default for Store<K, R> {
    fn default() -> Self {
        Self {
            applied: 0,
            rows: BTreeMap::new(),
            pending: VecDeque::new(),
            held: VecDeque::new(),
            fail_at: None,
            pages: VecDeque::new(),
            hold_next: 0,
            page_delay: 0,
            failed: false,
        }
    }
}

impl<K: Ord + Clone, R: Clone> Store<K, R> {
    /// Construct an empty store for a first deployment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure one fault at the boundary, without changing durable data.
    pub fn fault(&mut self, fault: Fault) {
        match fault {
            Fault::Hold { commits } => self.hold_next = commits,
            Fault::Fail { commit } => self.fail_at = Some(commit),
            Fault::SlowPages { by } => self.page_delay = by,
        }
    }

    /// Queue one whole numbered transaction; submissions must remain ordered.
    pub fn submit(&mut self, number: u64, writes: Box<[Write<K, R>]>, delay: u32) {
        assert!(!self.failed, "a failed store requires a cold start");
        let previous = self.pending.back().map_or(self.applied, |pending| pending.number);
        assert_eq!(number, previous.checked_add(1).expect("commit number fits"), "submissions keep journal order");
        self.pending.push_back(Pending { number, writes, delay });
    }

    /// Advance one store tick; a held completion fences every later completion.
    /// A selected failure leaves all of this transaction's rows unapplied and
    /// stops application of later commits until the next cold start.
    ///
    /// # Errors
    /// Returns the failed transaction number once, before any of its writes apply.
    pub fn tick(&mut self, hold_completion: bool) -> Result<Option<u64>, u64> {
        let Some(front) = self.pending.front_mut() else { return Ok(None) };
        if front.delay != 0 {
            front.delay -= 1;
            return Ok(None);
        }
        let pending = self.pending.pop_front().expect("ready transaction");
        if self.fail_at == Some(pending.number) {
            self.pending.clear();
            self.failed = true;
            return Err(pending.number);
        }
        assert_eq!(pending.number, self.applied.checked_add(1).expect("commit number fits"), "fake applies in order");
        for write in pending.writes {
            match write {
                Write::Save { key, row } => {
                    self.rows.insert(key, row);
                }
                Write::Erase(key) => {
                    self.rows.remove(&key);
                }
            }
        }
        self.applied = pending.number;
        let held = hold_completion || self.hold_next != 0 || !self.held.is_empty();
        self.hold_next = self.hold_next.saturating_sub(1);
        if held {
            self.held.push_back(pending.number);
            Ok(None)
        } else {
            Ok(Some(pending.number))
        }
    }

    /// Release the oldest withheld acknowledgement; a failed store sends none.
    pub fn release_held(&mut self) -> Option<u64> {
        if self.failed { None } else { self.held.pop_front() }
    }

    /// Lose all flights on a crash, retaining only the durable rows and number.
    /// Configured page slowness survives; a selected one-time failure does not.
    pub fn crash(&mut self) {
        self.pending.clear();
        self.held.clear();
        self.pages.clear();
        self.hold_next = 0;
        self.fail_at = None;
        self.failed = false;
    }

    /// Read a live range at cold start; historical rows outside it stay in storage.
    pub fn restore<'a>(&'a self, first: &'a K, last: &'a K) -> impl Iterator<Item = &'a R> {
        self.rows.range(first.clone()..=last.clone()).map(|(_, row)| row)
    }

    /// Capture one ordered page from currently durable rows.
    #[must_use]
    pub fn page(&self, first: &K, last: &K, after: Option<&K>, most: u32, delay: u32) -> Page<K, R> {
        assert!(most > 0, "a store page has positive room");
        let count = usize::try_from(most).expect("page count fits usize");
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for (key, row) in self.rows.range(first.clone()..=last.clone()) {
            if after.is_none_or(|previous| key > previous) {
                keys.push(key.clone());
                rows.push(row.clone());
                if rows.len() > count {
                    break;
                }
            }
        }
        let more = rows.len() > count;
        if more {
            rows.pop();
            keys.pop();
        }
        let next = if more { Some(keys.pop().expect("positive page")) } else { None };
        Page {
            rows: rows.into_boxed_slice(),
            next,
            delay: delay.checked_add(self.page_delay).expect("page delay fits"),
        }
    }

    /// Schedule a captured page, identified only by a caller-issued request number.
    pub fn load(&mut self, owner: u64, first: &K, last: &K, after: Option<&K>, most: u32) {
        assert!(!self.pages.iter().any(|(pending, _)| *pending == owner), "one flight per page request");
        let page = self.page(first, last, after, most, 0);
        self.pages.push_back((owner, page));
    }

    /// Advance every page flight once; each ready page is delivered exactly once.
    #[must_use]
    pub fn tick_pages(&mut self) -> Vec<(u64, Page<K, R>)> {
        let mut ready = Vec::new();
        for _ in 0..self.pages.len() {
            let (owner, mut page) = self.pages.pop_front().expect("counted page flight");
            if page.delay == 0 {
                ready.push((owner, page));
            } else {
                page.delay -= 1;
                self.pages.push_back((owner, page));
            }
        }
        ready
    }

    /// Whether a page request is still in flight.
    #[must_use]
    pub fn loading(&self) -> bool {
        !self.pages.is_empty()
    }
}
