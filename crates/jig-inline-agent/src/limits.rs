//! The in-process slots' checked memory bound (domain/hosts.md, section 5.2).

use core::mem::size_of;
use skein_lib::{Duration, Map, Queue, Token};
use smith_domain as smith;

use crate::boundary::Request;
use crate::domain::{Relay, Run};

/// Fixed slots, Smith bounds and codec receiving allowances.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    pub slots: u32,
    pub smith: smith::Limits,
    pub window: smith::Window,
    pub charter: smith_charter::v1::Limits,
    pub transcript: smith_transcript::v2::Limits,
    pub cancel_grace: Duration,
}

/// Worst case across live Smith domains and the inline routing queues.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    let per_run = smith::worst_case(&limits.smith)?
        .checked_add(Map::<Token, Relay>::worst_case(limits.smith.run.calls)?)?
        .checked_add(Map::<Token, bool>::worst_case(limits.smith.run.conversations)?)?
        .checked_add(Queue::<Token>::worst_case(limits.smith.run.messages)?)?;
    Map::<Token, Run>::worst_case(limits.slots)?
        .checked_add(u64::from(limits.slots).checked_mul(per_run)?)?
        .checked_add(Queue::<smith::Request>::worst_case(smith::max_out(&limits.smith))?)?
        .checked_add(Queue::<Request>::worst_case(max_out(limits))?)?
        .checked_add(
            u64::from(limits.smith.endpoints)
                .checked_mul(u64::try_from(size_of::<smith::run::charter::Endpoint>()).ok()?)?,
        )
}

/// The largest parent queue needed for one Smith event.
#[must_use]
pub const fn max_out(limits: &Limits) -> u32 {
    smith::max_out(&limits.smith)
        .saturating_add(limits.smith.run.calls)
        .saturating_add(limits.smith.run.conversations)
        .saturating_add(2)
}
