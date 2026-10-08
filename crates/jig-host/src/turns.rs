//! Turns retained for the engine until it acknowledges them (domain/hosts.md,
//! section 6.4). The root handles link retries when needed; the host owns the
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

/// A turn staged from the agent or admitted into the hub's retention window.
#[derive(Debug)]
pub(crate) struct Pending {
    agent: Token,
    turn: Turn,
    admitted: bool,
}

/// The bounded turns retained across a channel loss and an ended run.
#[derive(Debug)]
pub(crate) struct Turns {
    pending: Map<Name, Pending>,
    abandoned: u64,
}

impl Turns {
    pub(crate) fn new(limits: &Limits) -> Turns {
        let capacity = limits
            .slots
            .checked_mul(limits.turns)
            .expect("checked per-run capacity")
            .checked_mul(2)
            .expect("worst_case accepted turn capacity");
        Turns { pending: Map::with_capacity(capacity), abandoned: 0 }
    }

    pub(crate) fn len(&self) -> u32 {
        {
            let mut count = 0_u32;
            for (_, pending) in &self.pending {
                if pending.admitted {
                    count = count.checked_add(1).expect("bounded turns");
                }
            }
            count
        }
    }

    pub(crate) const fn abandoned(&self) -> u64 {
        self.abandoned
    }

    /// Reserve one whole next turn before acknowledging it to the agent.
    pub(crate) fn credit(&self, run: Token, attempt: Token, limits: &Limits) -> bool {
        let mut count = 0_u32;
        let mut bytes = 0_u64;
        for (name, pending) in &self.pending {
            if name.run == run && name.attempt == attempt && pending.admitted {
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

    /// Stage a turn until normal retention room is free; stopping takes it at once.
    pub(crate) fn retain(&mut self, agent: Token, run: Token, attempt: Token, turn: Turn) {
        let name = Name { run, attempt, turn: turn.turn };
        let old = self
            .pending
            .insert(name, Pending { agent, turn, admitted: false })
            .expect("retention plus one agent window was reserved");
        assert!(old.is_none(), "consecutive turn names do not repeat");
    }

    /// Take staged turns in order, acknowledging the agent once the hub holds them.
    pub(crate) fn admit(
        &mut self,
        run: Token,
        attempt: Token,
        stopping: bool,
        limits: &Limits,
        out: &mut skein_lib::Queue<crate::Request>,
    ) {
        for _ in 0..limits.turns {
            if !stopping && !self.credit(run, attempt, limits) {
                break;
            }
            let mut next = None;
            for (name, pending) in &self.pending {
                if name.run == run && name.attempt == attempt && !pending.admitted {
                    next = Some(*name);
                    break;
                }
            }
            let Some(name) = next else {
                break;
            };
            let pending = self.pending.get_mut(&name).expect("staged turn exists");
            pending.admitted = true;
            out.push(crate::Request::Turn { agent: pending.agent, run, attempt, turn: copy(&pending.turn) });
            out.push(crate::Request::AcknowledgeAgentTurn { agent: pending.agent, turn: name.turn });
        }
    }

    /// Drop a turn the engine committed.
    pub(crate) fn acknowledge(&mut self, run: Token, attempt: Token, turn: u32) {
        self.pending.remove(&Name { run, attempt, turn });
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
        if pending.admitted { Some(copy(&pending.turn)) } else { None }
    }

    /// A copy of the `index`th retained turn, in name order, for rehello.
    pub(crate) fn nth(&self, index: u32) -> Option<(Token, Token, Turn)> {
        let mut at = 0_u32;
        for (name, pending) in &self.pending {
            if !pending.admitted {
                continue;
            }
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
