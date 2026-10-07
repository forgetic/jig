use skein_lib::Map;

use crate::{Entry, Fact, Record, Service, Watch};

/// Bounds for observability's working set and one step's outputs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Limits {
    /// Cached service facts.
    pub services: u32,
    /// Live subscriptions.
    pub subscriptions: u32,
    /// Standing watches.
    pub watches: u32,
    /// Staged silences.
    pub staged: u32,
    /// Outstanding silences.
    pub effects: u32,
    /// Pending verdicts.
    pub judges: u32,
    /// Pending reads.
    pub reads: u32,
    /// Maximum subscribers reached by one alert.
    pub subscribers_per_alert: u32,
    /// Maximum load samples per service.
    pub samples_per_service: u32,
    /// Maximum services in one watch.
    pub services_per_watch: u32,
    /// Maximum alert numbers in one batch.
    pub alerts_per_batch: u32,
    /// Maximum bytes in a name or rule.
    pub name_bytes: u32,
    /// Maximum bytes in a read answer.
    pub answer_bytes: u32,
    /// Maximum attempts before an uncertain effect is held.
    pub max_attempts: u32,
    /// Seconds from sending an attempt to its absolute retry deadline.
    pub retry_after_seconds: u64,
}

/// Conservative heap bound for the connector's tables and values.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.name_bytes == 0
        || limits.answer_bytes == 0
        || limits.max_attempts == 0
        || limits.retry_after_seconds == 0
        || limits.subscribers_per_alert > limits.subscriptions
    {
        return None;
    }
    let name = u64::from(limits.name_bytes).checked_mul(2)?;
    Map::<Service, Fact>::worst_case(limits.services)?
        .checked_add(Map::<(crate::Topic, u64), (u8, u8)>::worst_case(limits.subscriptions)?)?
        .checked_add(Map::<u64, Watch>::worst_case(limits.watches)?)?
        .checked_add(Map::<skein_lib::Token, crate::Effect>::worst_case(limits.staged)?)?
        .checked_add(Map::<crate::Key, Entry>::worst_case(limits.effects)?)?
        .checked_add(Map::<skein_lib::Token, (crate::Requirement, Service, u64)>::worst_case(limits.judges)?)?
        .checked_add(Map::<skein_lib::Token, u32>::worst_case(limits.reads)?)?
        .checked_add(
            u64::from(limits.services)
                .checked_mul(name.checked_add(u64::from(limits.samples_per_service).checked_mul(16)?)?)?,
        )?
        .checked_add(u64::from(limits.watches).checked_mul(u64::from(limits.services_per_watch).checked_mul(name)?)?)?
        .checked_add(u64::from(limits.subscriptions).checked_mul(name)?)?
        .checked_add(u64::from(limits.judges).checked_mul(name)?)?
        .checked_add(u64::from(limits.staged).checked_mul(u64::from(limits.name_bytes))?)?
        .checked_add(u64::from(limits.effects).checked_mul(u64::from(limits.name_bytes))?)?
        .checked_add(u64::from(limits.subscribers_per_alert).checked_mul(16)?)?
        .checked_add(u64::from(limits.alerts_per_batch).checked_mul(8)?)?
        .checked_add(Map::<u64, Record>::worst_case(limits.watches)?)?
        .checked_add(u64::from(limits.answer_bytes))
}
