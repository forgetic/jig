//! What the views tell whoever watches the engine (domain/engine.md, section
//! 11): a fact for each thing that happened, content-free (kinds and counts,
//! never a token or a report's bytes), in a bounded queue the parent drains
//! at its own pace.
//!
//! Facts are outside the boundary's flow control: they are not requests, take
//! no room in `out`, and when the queue is full they are dropped and counted.
//! Nothing the views decide depends on whether a fact was kept. What the
//! views lose is also counted in [`Lost`], which drops nothing.

use skein_lib::Queue;

use crate::boundary::{End, Refusal};

/// Something that happened.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fact {
    /// A run is followed from now on, or is not, for want of room.
    Followed,
    Unfollowed,
    /// A report was streamed to this many watchers.
    Reported {
        watchers: u32,
    },
    /// One committed turn reached this many run and item watchers.
    Turn {
        watchers: u32,
    },
    /// A report was dropped, for `dropped`.
    Dropped {
        dropped: Dropped,
    },
    /// A task or inbox change was streamed to this many watchers.
    Changed {
        watchers: u32,
    },
    /// A watch was taken, refused at the entrance, or ended.
    Watching,
    Refused {
        refusal: Refusal,
    },
    Ended {
        end: End,
    },
    /// A watcher's backlog overflowed: `missed` chunks waiting were dropped,
    /// which it is told of with its next delivery.
    Overflowed {
        missed: u32,
    },
    /// A delivery of `chunks` chunks went to a watcher.
    Delivered {
        chunks: u32,
    },
    /// A watcher's stream did not take a delivery of `chunks` chunks, which
    /// it is told it missed with its next.
    Undelivered {
        chunks: u64,
    },
}

/// Why a report was dropped.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dropped {
    /// Its run is not followed.
    Unfollowed,
    /// Its content is past the limits.
    Oversized,
}

/// What the views lost since the domain was made, each count saturating.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Lost {
    /// Runs not followed, for want of room.
    pub runs: u64,
    /// Reports dropped: past the limits, or of a run not followed.
    pub reports: u64,
    /// Chunks a watcher missed: dropped from its full backlog, in a delivery
    /// its stream did not take, or of a report dropped.
    pub chunks: u64,
    pub facts: u64,
}

impl Lost {
    pub(crate) const NONE: Lost = Lost { runs: 0, reports: 0, chunks: 0, facts: 0 };
}

/// What was lost, as [`Facts::lose`] counts it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Loss {
    Runs,
    Reports,
    Chunks,
}

/// The facts not yet drained, and what was lost, facts that did not fit
/// included.
#[derive(Debug)]
pub(crate) struct Facts {
    queue: Queue<Fact>,
    lost: Lost,
}

impl Facts {
    pub(crate) fn with_capacity(capacity: u32) -> Facts {
        Facts { queue: Queue::with_capacity(capacity), lost: Lost::NONE }
    }

    /// Keeps `fact` if there is room for it, and counts it otherwise.
    pub(crate) fn push(&mut self, fact: Fact) {
        if self.queue.try_push(fact).is_err() {
            self.lost.facts = self.lost.facts.saturating_add(1);
        }
    }

    pub(crate) fn pop(&mut self) -> Option<Fact> {
        self.queue.pop()
    }

    /// Counts `count` more of `loss` lost.
    pub(crate) fn lose(&mut self, loss: Loss, count: u64) {
        let counter = match loss {
            Loss::Runs => &mut self.lost.runs,
            Loss::Reports => &mut self.lost.reports,
            Loss::Chunks => &mut self.lost.chunks,
        };
        *counter = counter.saturating_add(count);
    }

    pub(crate) fn lost(&self) -> Lost {
        self.lost
    }
}
