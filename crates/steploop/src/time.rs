//! Time as plain data.
//!
//! The pure steps never read a clock: the loop reads `Instant::now()` once per
//! iteration and hands the result to every step as a [`Time`]. Tests and
//! replays construct `Time` values directly. See
//! `docs/explanation/sans-io-shell.md` §4.4.

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
