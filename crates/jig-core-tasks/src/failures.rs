//! Per-class task failure history and configured equal-jitter backoff
//! (domain/tasks.md, section 5.5). Root classifies external failures;
//! this module uses injected randomness and owns no clock or peer state.
use skein_lib::{Duration, Rng};

/// Root-reported activation failure category; each category has its own bounded retry policy and
/// counter. (domain/tasks.md, section 5.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Class {
    /// Transient peer/read/transcript failure reported by the root.
    Transient,
    /// Failure requiring an external correction, such as an unavailable resource or invalid
    /// assignment.
    Permanent,
    /// `Run` itself reported execution failure.
    Run,
    /// `Agent` crashed, violated its channel contract or stalled.
    Agent,
    /// Worker was lost.
    Lost,
    /// Returned result violated the admitted task contract.
    Invalid,
}

/// Task's per-class failure counters, updated with saturation; counts describe history rather
/// than a retry permission. (domain/tasks.md, section 5.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Tries {
    pub transient: u32,
    /// Count of failures awaiting an external correction.
    pub permanent: u32,
    pub run: u32,
    pub agent: u32,
    pub lost: u32,
    /// Count of results that violated the task contract.
    pub invalid: u32,
}

impl Tries {
    /// All failure counters zero; a `new` task and a successful park start with this value.
    pub const NONE: Tries = Tries { transient: 0, permanent: 0, run: 0, agent: 0, lost: 0, invalid: 0 };

    /// Pure selection of the counter or retry policy for `class`; no allocation, mutation, output
    /// or lifecycle transition.
    #[must_use]
    pub const fn of(&self, class: Class) -> u32 {
        match class {
            Class::Transient => self.transient,
            Class::Permanent => self.permanent,
            Class::Run => self.run,
            Class::Agent => self.agent,
            Class::Lost => self.lost,
            Class::Invalid => self.invalid,
        }
    }

    pub(crate) fn add(&mut self, class: Class) {
        match class {
            Class::Transient => self.transient = self.transient.saturating_add(1),
            Class::Permanent => self.permanent = self.permanent.saturating_add(1),
            Class::Run => self.run = self.run.saturating_add(1),
            Class::Agent => self.agent = self.agent.saturating_add(1),
            Class::Lost => self.lost = self.lost.saturating_add(1),
            Class::Invalid => self.invalid = self.invalid.saturating_add(1),
        }
    }
}

/// Root-configured retry count and equal-jitter exponential delay bounds; startup validates
/// representable usable settings. (domain/tasks.md, section 5.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Retry {
    /// Failures allowed before the task is held; startup rejects `u32::MAX`.
    pub retries: u32,
    /// Positive initial exponential-backoff delay; startup rejects zero.
    pub base: Duration,
    /// Backoff ceiling, required to be at least `base`; jitter selects within the upper half of the
    /// capped delay.
    pub max: Duration,
}

/// One configured retry policy per exhaustive failure class. (domain/tasks.md, section 5.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Retries {
    /// `Transient`-failure and pre-execution refusal pause policy.
    pub transient: Retry,
    /// Policy for failures requiring an external correction.
    pub permanent: Retry,
    pub run: Retry,
    pub agent: Retry,
    pub lost: Retry,
    pub invalid: Retry,
}

impl Retries {
    /// Pure selection of the counter or retry policy for `class`; no allocation, mutation, output
    /// or lifecycle transition.
    #[must_use]
    pub const fn of(&self, class: Class) -> Retry {
        match class {
            Class::Transient => self.transient,
            Class::Permanent => self.permanent,
            Class::Run => self.run,
            Class::Agent => self.agent,
            Class::Lost => self.lost,
            Class::Invalid => self.invalid,
        }
    }
}

pub(crate) fn backoff(times: u32, retry: Retry, rng: &mut Rng) -> Duration {
    let factor = 1_u64.checked_shl(times.saturating_sub(1)).unwrap_or(u64::MAX);
    let ceiling = retry.base.saturating_mul(factor).min(retry.max);
    let half = ceiling.as_nanos().div_euclid(2);
    Duration::from_nanos(half.saturating_add(rng.below(half.saturating_add(1))))
}
