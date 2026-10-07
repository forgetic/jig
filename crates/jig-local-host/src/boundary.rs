//! The local host's vocabulary toward the application root (domain/hosts.md,
//! sections 2, 5, 8 and 9). The root translates its core's assignment and
//! decodes host calls; Smith's provider values pass to the protocol layer.

use alloc::boxed::Box;
use skein_lib::{Duration, Time, Token};
use smith_domain as smith;

pub use jig_charter::{
    Budget, Charter, Contract, FieldRule, ItemRule, Items, Model, Prices, Section, TextRule, Tool, ToolEffect,
    VerdictRule, WorkspaceTools,
};

/// A secret-free credential grant for an LLM account.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grant {
    pub account: u32,
    pub generation: u64,
    pub valid: Duration,
}

/// One claimed run, already committed by the core before admission here.
#[derive(PartialEq, Eq, Debug)]
pub struct Assignment {
    pub task: u64,
    pub attempt: u32,
    pub charter: Charter,
    pub brief: Box<[Section]>,
    /// Concrete history decoded by the root's protocol layer.
    pub transcript: Option<smith::Transcript>,
    /// Durable answers after the last turn, decoded by the root.
    pub calls: Box<[smith::AnsweredCall]>,
    pub grants: Box<[Grant]>,
}

/// One provider terminal, with the owner of its earlier completion request.
#[derive(PartialEq, Eq, Debug)]
pub enum Completion {
    /// The provider returned a completion.
    Completed { owner: Token, completion: smith::llm::Completion },
    /// The provider failed this completion.
    Failed { owner: Token, failure: smith::llm::Failure, evidence: smith::llm::Evidence, detail: Box<[u8]> },
    /// The provider confirmed cancellation.
    Cancelled { owner: Token },
}

/// Parent -> local host. Task and attempt fence every input after assignment.
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// Claim a configured slot for a committed assignment.
    Assign { slot: u32, assignment: Box<Assignment> },
    /// Deliver one message whose label and words are attested UTF-8.
    Message { task: u64, attempt: u32, name: u64, label: Box<[u8]>, words: Box<[u8]> },
    /// Return one host call to Smith.
    Answered { task: u64, attempt: u32, relay: smith::run::RelayName, reply: smith::run::HostReply },
    /// Release retained turns through the committed turn number.
    AcknowledgeTurn { task: u64, attempt: u32, turn: u32 },
    /// Release an answered or stopped run's slot.
    AcknowledgeAnswer { task: u64, attempt: u32 },
    /// Update a provider account's credential grant.
    Grant { task: u64, attempt: u32, grant: Grant },
    /// Start Smith's cancellation grace.
    Cancel { task: u64, attempt: u32 },
    /// Return one provider terminal to Smith.
    Completion { task: u64, attempt: u32, terminal: Completion },
}

/// Why an assignment could not take its named slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The named slot is occupied.
    Busy,
    /// The assignment or slot is invalid.
    Invalid,
    /// This task already occupies another slot.
    Duplicate,
}

/// Why a message was returned to the core's inbox.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageRefusal {
    /// The addressed run is absent or has already answered.
    NoRun,
    /// The joined label and words exceed Smith's message limit.
    TooLarge,
    /// The label is empty.
    Invalid,
}

/// Local host -> application root. A `Protocol` request belongs below the
/// domain and retains Smith's typed completion or cancellation ownership.
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// Assignment could not enter its slot.
    Refused { task: u64, attempt: u32, why: Refusal },
    /// Smith accepted the run on its slot.
    Admitted { task: u64, attempt: u32 },
    /// Return a message the live run could not accept.
    Bounced { task: u64, attempt: u32, name: u64, why: MessageRefusal },
    /// Smith is waiting for a message or a host answer.
    Waiting { task: u64, attempt: u32, read: Option<u64> },
    /// One numbered turn to commit and acknowledge.
    Turn { task: u64, attempt: u32, number: u32, position: u32, read: Option<u64>, spent: u64, turn: smith::Turn },
    /// Ask the root to route one host tool call.
    Call {
        task: u64,
        attempt: u32,
        relay: smith::run::RelayName,
        name: smith::run::CallName,
        tool: Box<[u8]>,
        effect: smith::run::HostEffect,
        input: smith::run::HostInput,
        deadline: Time,
    },
    /// Withdraw one outstanding host tool call.
    WithdrawCall { task: u64, attempt: u32, relay: smith::run::RelayName },
    /// Smith's terminal answer, held until acknowledged.
    Answer { task: u64, attempt: u32, answer: smith::run::Answer },
    /// The cancellation grace expired before Smith answered.
    Stopped { task: u64, attempt: u32 },
    /// Smith's provider operation or notice, addressed to the protocol layer.
    Protocol { task: u64, attempt: u32, request: smith::Request },
}
