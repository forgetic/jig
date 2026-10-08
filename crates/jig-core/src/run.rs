//! Live run and restart correlation held by the core (domain/engine.md, 3 and 7).

use crate::CallKey;
use alloc::boxed::Box;
use jig_core_tasks as tasks;
use skein_lib::{Queue, Token};

/// Bounded recent opaque conversation while a due task's store pages are read.
/// Empty turn bodies need no slot: they add nothing to a resumed conversation.
#[derive(Debug)]
pub struct Transcript {
    pub previous_attempt: u64,
    pub bytes: u64,
    pub kept: u64,
    pub turns: Queue<Box<[u8]>>,
}

#[derive(Debug)]
pub struct RestoringProof {
    pub attempt: u64,
    pub turn: u32,
    pub run_spent: u64,
    pub last_answer: Option<u64>,
}

/// One configured model and its deployment-unit prices (domain/agent.md, 4.6).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Model {
    pub dialect: u32,
    pub account: u32,
    pub endpoint: u32,
    pub name: Box<[u8]>,
    pub max_tokens: u32,
    pub input_price: u64,
    pub cached_price: u64,
    pub output_price: u64,
    pub price_unit: u32,
}

/// Root-owned charter policy, independent of Smith's run vocabulary.
#[derive(Clone, PartialEq, Eq, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "independent charter grants and run behavior")]
pub struct RunPolicy {
    pub instructions: Box<[u8]>,
    pub waiting: skein_lib::Duration,
    pub resume: bool,
    pub turns: u32,
    pub time: skein_lib::Duration,
    pub model: Model,
    pub alternatives: Box<[Model]>,
    pub inspect: bool,
    pub modify: bool,
    pub shell: bool,
    pub agents: bool,
    pub call_timeout: skein_lib::Duration,
}

/// The root's complete task-specific charter, kept behind one bounded
/// assignment cell so delivery queues carry a small fixed-size value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunCharter {
    pub policy: RunPolicy,
    pub contract: tasks::Contract,
    pub authority: tasks::Authority,
    pub budget: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoutedCall {
    Escalation { key: CallKey, task: u64, revision: u64 },
    Propose { key: CallKey, proposal: u64 },
    Decide { key: CallKey, proposal: u64 },
    Withdraw { key: CallKey, proposal: u64 },
    Accepting { key: CallKey, proposer: u64, proposal: u64, message: u64 },
    Message(CallKey),
    Introduce(CallKey),
    Subscribe { key: CallKey, subscription: u64 },
    Unsubscribe(CallKey),
    Control(CallKey),
}

#[derive(Debug)]
pub struct HistoricalResult {
    pub task: u64,
    pub kind: tasks::ResultKind,
    pub words: Box<[u8]>,
}

#[derive(Debug)]
pub struct PendingRelay {
    pub previous: Option<u64>,
    pub word: tasks::Word,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PersonProposalRoute {
    Deciding { request: Token, proposer: u64, proposal: u64, by: u64 },
    Accepting { request: Token, person: u64, proposer: u64, proposal: u64, message: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PersonTaskRoute {
    PoolSet { project: u32, person: u64 },
    Take(u64),
    HandBack(u64),
    Answer(u64),
    Cancel(u64),
    Release(u64),
    Prioritised(u32),
    Amended(u64),
    AmendProposed { task: u64, proposal: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GoalRoute {
    Proposing { proposal: u64 },
    Accepting { proposer: u64, proposal: u64, by: u64, task: u64 },
    Deciding { proposer: u64, proposal: u64, by: u64 },
}
