//! Turns retained for the engine until it acknowledges them (domain/hosts.md,
//! section 6.4). The worker root handles channel retries; the host owns the
//! bytes and the credit bound that pauses an agent reader.

use skein_lib::bytes::copy_of;
use skein_lib::{Map, Token};

use crate::boundary::Turn;
use crate::limits::Limits;

/// A turn's name on the engine link.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct Name {
    pub(crate) run: Token,
    pub(crate) attempt: Token,
    pub(crate) turn: u32,
}

/// A turn held with the agent that will receive more read credit.
#[derive(Debug)]
pub(crate) struct Pending {
    agent: Token,
    turn: Turn,
}

/// The bounded turns retained across a channel loss and an ended run.
#[derive(Debug)]
pub(crate) struct Turns {
    pending: Map<Name, Pending>,
    abandoned: u64,
}

impl Turns {
    pub(crate) fn new(limits: &Limits) -> Turns {
        let capacity = limits.slots.checked_mul(limits.turns).expect("worst_case accepted turn capacity");
        Turns { pending: Map::with_capacity(capacity), abandoned: 0 }
    }

    pub(crate) fn len(&self) -> u32 {
        self.pending.len()
    }

    pub(crate) const fn abandoned(&self) -> u64 {
        self.abandoned
    }

    /// Reserve one whole possible next turn before granting another read.
    pub(crate) fn credit(&self, run: Token, attempt: Token, limits: &Limits) -> bool {
        let mut count = 0_u32;
        let mut bytes = 0_u64;
        for (name, pending) in &self.pending {
            if name.run == run && name.attempt == attempt {
                count = count.checked_add(1).expect("bounded turns");
                bytes = bytes.checked_add(len(&pending.turn.body)).expect("bounded retained bytes");
            }
        }
        count < limits.turns
            && match bytes.checked_add(limits.turn_bytes) {
                Some(next) => next <= limits.turn_queue_bytes,
                None => false,
            }
    }

    /// Keep a validated turn, returning whether the agent may read another.
    pub(crate) fn retain(&mut self, agent: Token, run: Token, attempt: Token, turn: Turn, limits: &Limits) -> bool {
        assert!(self.credit(run, attempt, limits), "a turn consumes reserved credit");
        let name = Name { run, attempt, turn: turn.turn };
        let old = self.pending.insert(name, Pending { agent, turn }).expect("turn credit checked capacity");
        assert!(old.is_none(), "consecutive turn names do not repeat");
        self.credit(run, attempt, limits)
    }

    /// Drop an acknowledged turn and name its agent for renewed read credit.
    pub(crate) fn acknowledge(&mut self, run: Token, attempt: Token, turn: u32) -> Option<Token> {
        match self.pending.remove(&Name { run, attempt, turn }) {
            Some(pending) => Some(pending.agent),
            None => None,
        }
    }

    pub(crate) fn holds(&self, run: Token, attempt: Token, turn: u32) -> bool {
        self.pending.contains_key(&Name { run, attempt, turn })
    }

    pub(crate) fn has_run(&self, run: Token, attempt: Token) -> bool {
        for (name, _) in &self.pending {
            if name.run == run && name.attempt == attempt {
                return true;
            }
        }
        false
    }

    /// A copy for one transmission; the retained bytes remain until ACK.
    pub(crate) fn get(&self, run: Token, attempt: Token, turn: u32) -> Option<Turn> {
        let pending = self.pending.get(&Name { run, attempt, turn })?;
        Some(copy(&pending.turn))
    }

    /// A copy of the `index`th retained turn, in name order, for rehello.
    pub(crate) fn nth(&self, index: u32) -> Option<(Token, Token, Turn)> {
        let mut at = 0_u32;
        for (name, pending) in &self.pending {
            if at == index {
                return Some((name.run, name.attempt, copy(&pending.turn)));
            }
            at = at.checked_add(1).expect("bounded turns");
        }
        None
    }

    /// Count each turn abandoned after shutdown and a lost channel.
    pub(crate) fn give_up(&mut self) {
        for _ in 0..self.pending.capacity() {
            let Some((name, _)) = self.pending.first() else {
                break;
            };
            let name = *name;
            self.pending.remove(&name);
            self.abandoned = self.abandoned.saturating_add(1);
        }
    }
}

fn copy(turn: &Turn) -> Turn {
    Turn { turn: turn.turn, spent: turn.spent, read: turn.read, body: copy_of(&turn.body) }
}

fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).expect("a length fits in u64")
}
