//! What the host tells whoever watches the worker (domain/hosts.md, sections
//! 6.2 and 6.5): a fact for each step of a hosted run's lifecycle, content-free (the
//! engine's names and classifications, never what the charter, the run or the
//! engine said), in a bounded queue the parent drains at its own pace.
//!
//! Facts are outside the boundary's flow control: they are not requests, take
//! no room in `out`, and when the queue is full they are dropped and counted.
//! Nothing the host decides depends on whether a fact was kept. An assignment
//! refused at the entrance tells nothing: no run was hosted. The agent's
//! own facts wait in a separate bounded queue until the parent can send them.

use alloc::boxed::Box;

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

/// A live fact the agent reported, named for its run on the engine link.
#[derive(PartialEq, Eq, Debug)]
pub struct Told {
    pub run: Token,
    pub attempt: Token,
    pub fact: Box<[u8]>,
}

/// Agent facts wait for an open channel, within a fixed count and byte bound.
#[derive(Debug)]
pub(crate) struct AgentFacts {
    queue: Queue<Told>,
    lost: u64,
}

impl AgentFacts {
    pub(crate) fn with_capacity(capacity: u32) -> AgentFacts {
        AgentFacts { queue: Queue::with_capacity(capacity), lost: 0 }
    }

    pub(crate) fn push(&mut self, fact: Told, max_bytes: u64) {
        if u64::try_from(fact.fact.len()).expect("a length fits in u64") > max_bytes
            || self.queue.try_push(fact).is_err()
        {
            self.lost = self.lost.saturating_add(1);
        }
    }

    pub(crate) fn pop(&mut self) -> Option<Told> {
        self.queue.pop()
    }

    pub(crate) const fn lost(&self) -> u64 {
        self.lost
    }
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
