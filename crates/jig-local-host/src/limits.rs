//! The host's slot and retained-turn bounds (domain/hosts.md, section 5).
//! Every slot prices one Smith domain and its turns awaiting core commit.

use core::mem::size_of;
use skein_lib::{Duration, Map, Queue, Token};
use smith_domain as smith;

use crate::domain::{Hosted, Relay, Retained};

/// Immutable capacity and grace configuration for the local host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    pub slots: u32,
    pub smith: smith::Limits,
    pub window: smith::Window,
    pub cancel_grace: Duration,
}

/// A checked upper bound for all busy slots and their retained turns.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.window.turns == 0 || limits.window.bytes < smith::max_turn_bytes(&limits.smith)? {
        return None;
    }
    let smith = smith::worst_case(&limits.smith)?;
    let retained = Map::<u32, Retained>::worst_case(limits.window.turns)?.checked_add(limits.window.bytes)?;
    let per_slot = smith
        .checked_add(retained)?
        .checked_add(Map::<Token, bool>::worst_case(limits.smith.run.conversations)?)?
        .checked_add(Map::<Token, Relay>::worst_case(limits.smith.run.calls)?)?;
    Map::<u32, Hosted>::worst_case(limits.slots)?
        .checked_add(u64::from(limits.slots).checked_mul(per_slot)?)?
        .checked_add(Queue::<smith::Request>::worst_case(smith::max_out(&limits.smith))?)?
        .checked_add(
            u64::try_from(size_of::<smith::run::charter::Endpoint>())
                .ok()?
                .checked_mul(u64::from(limits.smith.endpoints))?,
        )
}
