use skein_lib::{Deadlines, Duration, Map, Queue};

use crate::Fact;
use crate::domain::Account;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    pub accounts: u32,
    pub refresh_margin: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub rejected_interval: Duration,
    pub spent_attention: Duration,
    pub facts: u32,
}

/// Accounts hold fixed-sized names and times; every secret is below them.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.accounts == 0 || limits.backoff_base == Duration::ZERO || limits.backoff_max < limits.backoff_base {
        return None;
    }
    Map::<u32, Account>::worst_case(limits.accounts)?
        .checked_add(Deadlines::<u32>::worst_case(limits.accounts)?)?
        .checked_add(Queue::<Fact>::worst_case(limits.facts)?)
}
