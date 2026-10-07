//! What the host tells whoever watches the worker (worker-domain.md, section
//! 7): a fact for each step of a hosted run's lifecycle, content-free (the
//! engine's names and classifications, never what the charter, the run or the
//! engine said), in a bounded queue the parent drains at its own pace.
//!
//! Facts are outside the boundary's flow control: they are not requests, take
//! no room in `out`, and when the queue is full they are dropped and counted.
//! Nothing the host decides depends on whether a fact was kept. An assignment
//! refused at the entrance tells nothing: no run was hosted. What the run
//! itself reports is not the host's: the top level forwards it.

use skein_lib::{Queue, Token};

use crate::boundary::Failure;

/// Something that happened to the hosted run `run`'s attempt `attempt`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fact {
    /// The assignment was admitted.
    Admitted { run: Token, attempt: Token },
    /// Its workspace was prepared.
    Prepared { run: Token, attempt: Token },
    /// Its agent started.
    Started { run: Token, attempt: Token },
    /// It answered: parked.
    Parked { run: Token, attempt: Token },
    /// It answered: ended with an outcome.
    Ended { run: Token, attempt: Token },
    /// It answered: failed, for `failure`.
    Failed { run: Token, attempt: Token, failure: Failure },
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
