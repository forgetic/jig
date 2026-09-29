//! Time as plain data.
//!
//! The pure steps never read a clock: the loop reads `Instant::now()` once per
//! iteration and hands the result to every step as a [`Time`]. Tests and
//! replays construct `Time` values directly. See
//! `docs/explanation/sans-io-shell.md` §4.4.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Duration;

/// Monotonic nanoseconds since the loop started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(pub u64);

impl Time {
    pub const ZERO: Time = Time(0);

    /// `self + d`, saturating at the far future.
    pub fn after(self, d: Duration) -> Time {
        let nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        Time(self.0.saturating_add(nanos))
    }

    /// How long from `self` until `later` (zero if `later` has passed).
    pub fn until(self, later: Time) -> Duration {
        Duration::from_nanos(later.0.saturating_sub(self.0))
    }
}

/// Pending deadlines, earliest first: a min-heap with lazy deletion.
///
/// There is no cancel and no reschedule. A step that changes its mind simply
/// schedules again, and the old entry stays in the heap until it expires.
/// **Stale keys are the caller's to ignore:** when [`Deadlines::pop_expired`]
/// yields a key, the caller checks its own state (is the entity still there,
/// is this still its deadline?) and does nothing if not. The cost is an early
/// wake-up now and then ([`Deadlines::next`] may report a stale entry's
/// time), and entries that linger until they expire; both are cheap next to
/// keeping a cancellable index in sync.
///
/// Ties expire in key order, so iteration is deterministic (house rule 10).
#[derive(Clone, Debug)]
pub struct Deadlines<K> {
    heap: BinaryHeap<Reverse<(Time, K)>>,
}

impl<K: Copy + Ord> Default for Deadlines<K> {
    fn default() -> Self {
        Deadlines {
            heap: BinaryHeap::new(),
        }
    }
}

impl<K: Copy + Ord> Deadlines<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wake for `key` at `at`. Scheduling a key again does not remove its
    /// earlier entry.
    pub fn schedule(&mut self, at: Time, key: K) {
        self.heap.push(Reverse((at, key)));
    }

    /// The earliest entry's time, stale or not: what to pass on as the step's
    /// deadline.
    pub fn next(&self) -> Option<Time> {
        self.heap.peek().map(|Reverse((at, _))| *at)
    }

    /// Move every key due at or before `now` into `out`, earliest first.
    pub fn pop_expired(&mut self, now: Time, out: &mut Vec<K>) {
        while let Some(Reverse((at, key))) = self.heap.peek() {
            if *at > now {
                break;
            }
            out.push(*key);
            self.heap.pop();
        }
    }

    /// Entries in the heap, stale ones included.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u64) -> Time {
        Time(n)
    }

    #[test]
    fn empty_has_no_next() {
        let mut d = Deadlines::<u32>::new();
        assert_eq!(d.next(), None);
        let mut out = Vec::new();
        d.pop_expired(t(100), &mut out);
        assert!(out.is_empty());
        assert!(d.is_empty());
    }

    #[test]
    fn next_is_the_earliest() {
        let mut d = Deadlines::new();
        d.schedule(t(30), 1u32);
        d.schedule(t(10), 2);
        d.schedule(t(20), 3);
        assert_eq!(d.next(), Some(t(10)));
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn pop_expired_is_inclusive_and_ordered() {
        let mut d = Deadlines::new();
        d.schedule(t(30), 1u32);
        d.schedule(t(10), 2);
        d.schedule(t(20), 3);
        d.schedule(t(40), 4);
        let mut out = vec![99];
        d.pop_expired(t(30), &mut out);
        // Appends, never clears (house rule 11).
        assert_eq!(out, vec![99, 2, 3, 1]);
        assert_eq!(d.next(), Some(t(40)));
        out.clear();
        d.pop_expired(t(39), &mut out);
        assert!(out.is_empty());
        d.pop_expired(t(40), &mut out);
        assert_eq!(out, vec![4]);
        assert_eq!(d.next(), None);
    }

    #[test]
    fn ties_expire_in_key_order() {
        let mut d = Deadlines::new();
        for k in [5u32, 1, 4, 2, 3] {
            d.schedule(t(7), k);
        }
        let mut out = Vec::new();
        d.pop_expired(t(7), &mut out);
        assert_eq!(out, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn rescheduling_leaves_a_stale_entry_for_the_caller_to_ignore() {
        let mut d = Deadlines::new();
        d.schedule(t(10), 1u32);
        d.schedule(t(50), 1); // the caller moved key 1's deadline to 50
        assert_eq!(d.next(), Some(t(10)), "the stale entry still wakes early");
        let mut out = Vec::new();
        d.pop_expired(t(10), &mut out);
        assert_eq!(out, vec![1], "stale: the caller sees its deadline is 50");
        assert_eq!(d.next(), Some(t(50)));
        out.clear();
        d.pop_expired(t(50), &mut out);
        assert_eq!(out, vec![1]);
        assert!(d.is_empty());
    }

    #[test]
    fn time_arithmetic_saturates() {
        assert_eq!(
            Time(u64::MAX - 1).after(Duration::from_secs(1)),
            Time(u64::MAX)
        );
        assert_eq!(t(10).until(t(5)), Duration::ZERO);
        assert_eq!(t(5).until(t(10)), Duration::from_nanos(5));
        assert_eq!(Time::ZERO.after(Duration::MAX), Time(u64::MAX));
    }
}
