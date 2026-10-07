//! What the fleet tells whoever watches the engine (domain/engine.md, section
//! 11): a fact for each thing that happened, content-free (kinds and counts,
//! never a run's name or a payload), in a bounded queue the parent drains at
//! its own pace.
//!
//! Facts are outside the boundary's flow control: they are not requests, take
//! no room in `out`, and when the queue is full they are dropped and counted.
//! Nothing the fleet decides depends on whether a fact was kept.

use skein_lib::Queue;

use crate::boundary::{Answer, Refusal};

/// Something that happened.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fact {
    /// A worker said hello, listing `listed` runs.
    Hello { listed: u32 },
    /// A worker was turned away at its hello.
    TurnedAway,
    /// A worker's channel was lost, with `adrift` attempts on it, kept for
    /// the grace.
    Lost { adrift: u32 },
    /// A start or an adoption was refused at the entrance.
    Refused { refusal: Refusal },
    /// An attempt was assigned to a worker.
    Placed,
    /// A worker listed an attempt the parent had adopted, or one adrift.
    Found,
    /// A worker listed an attempt the parent has not claimed: it waits to be
    /// adopted.
    Stray,
    /// An attempt not adopted in time, not the claim of its run, or listed
    /// beyond the room, was cancelled.
    Fenced,
    /// A stray's kept answer not adopted in time, or cancelled, was
    /// forgotten.
    Forgotten,
    /// An attempt's answer went to the parent.
    Answered { answer: Answer },
    /// A worker refused an attempt as busy: it is placed again.
    Busy,
    /// An attempt was presumed lost.
    PresumedLost,
    /// An answer sent again, or for an attempt fenced off or no longer
    /// tracked, was dropped.
    Duplicate,
    /// Something a worker sent for an attempt fenced off or gone, from a
    /// channel not in contact, or beyond what a hello may list; or a reply
    /// for an attempt gone, was dropped.
    Dropped,
}

/// The facts not yet drained, and how many did not fit.
#[derive(Debug)]
pub(crate) struct Facts {
    queue: Queue<Fact>,
    lost: u64,
}

impl Facts {
    pub(crate) fn with_capacity(capacity: u32) -> Facts {
        Facts { queue: Queue::with_capacity(capacity), lost: 0 }
    }

    /// Keeps `fact` if there is room for it, and counts it otherwise.
    pub(crate) fn push(&mut self, fact: Fact) {
        if self.queue.try_push(fact).is_err() {
            self.lost = self.lost.saturating_add(1);
        }
    }

    pub(crate) fn pop(&mut self) -> Option<Fact> {
        self.queue.pop()
    }

    pub(crate) fn lost(&self) -> u64 {
        self.lost
    }
}
