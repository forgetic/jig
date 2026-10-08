//! The root wraps its children's vocabulary. Replace the connector arms when
//! copying this application; keep Store and Released (domain/root.md, 2–4).
use crate::{Range, Record, Write};
use alloc::boxed::Box;
use jig_core as core;
use jig_core_fleet as fleet;
use jig_core_tasks as tasks;
use jig_host as host;
use jig_inline_agent as agent;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{Queue, ReplyTo, Token, Wall};

/// One store response, belonging to its named owner.
#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "store pages own complete typed child events")]
pub enum Store {
    /// This whole transaction is durable.
    Committed { number: u64 },
    /// This transaction failed and no later held output may leave.
    Failed { number: u64 },
    /// One row in the core's requested restoration order.
    Restore(Record),
    /// The store completed this exact restart range.
    Restored(core::RestartStep),
    /// Ordered opaque transcript rows for a prepared activation.
    Transcript { task: u64, rows: Box<[core::TurnRecord]>, done: bool },
    /// A child-specific loaded page in the core's vocabulary.
    Loaded(core::Event),
}

/// A released output addressed to a child; iterate feeds it back as one event.
#[derive(Debug)]
pub enum Released {
    /// A core continuation.
    Core(core::Event),
    /// A fleet continuation named by the core.
    Fleet(fleet::Event),
    /// One infrastructure entry may now be made.
    Infrastructure(infrastructure::Event),
    /// One observability entry may now be made.
    Observability(observability::Event),
    /// The engine's own run hub, over its permanent link.
    Host(host::Event),
    /// The hub's agent capability, in the same process.
    Agent(smith_host_domain::Event),
    /// A procedure may choose its next step after the preceding decision.
    Procedure { task: u64, step: u64, connector: u16, code: u32 },
    /// One core-selected stage of cold restart.
    Restart(core::RestartStep),
}

/// Inputs to the reference root; protocol adapters supply typed child values.
#[derive(Debug)]
pub enum Event {
    /// A typed core input, including authenticated parties and accounts.
    Core(core::Event),
    /// Observability's typed protocol input.
    Observability(observability::Event),
    /// Infrastructure's typed protocol input.
    Infrastructure(infrastructure::Event),
    /// The host's agent or engine-side input.
    Host(host::Event),
    /// An LLM terminal for the named inline agent slot.
    Llm { client: Token, completion: agent::Completion },
    /// A typed store terminal or page.
    Store(Store),
    /// A child-addressed output released by the journal.
    Released(Released),
    /// Begin the core's cold-start script.
    Restart,
    /// One due core timer.
    Timer(core::Timer),
    /// One observability retry deadline.
    ObservabilityTimer,
    /// One infrastructure retry deadline.
    InfrastructureTimer,
    /// The inline agent's next deadline; the hub arms no timers.
    HostTimer,
    /// A decoded infrastructure effect with the core's caller envelope.
    Effect {
        to: ReplyTo,
        key: core::CallKey,
        effect: infrastructure::Effect,
        deadline: Wall,
        proposal: Option<Box<[u8]>>,
    },
}

/// A timer retains its owner's identity through the root's admission.
#[derive(Clone, Copy, Debug)]
pub enum Timer {
    /// A core child selected by the timer adapter.
    Core(core::Timer),
    /// Observability's retry deadline.
    Observability,
    /// Infrastructure's retry deadline.
    Infrastructure,
    /// The inline agent's deadline.
    Agent,
}

/// The journal is the only source of root outputs.
#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "the journal owns complete concrete outputs")]
pub enum Output {
    /// One atomic transaction, numbered by the journal.
    Commit { number: u64, writes: Queue<Write> },
    /// Load this child's rows for the core-selected restart step.
    Load { step: core::RestartStep, range: Range },
    /// A core-owned store read, routed by its variant.
    CoreLoad(core::Now),
    /// Core output for its party, view, note or external protocol owner.
    Core(core::Held),
    /// Core output with no durable dependency, such as a view fact.
    Now(core::Now),
    /// One request to the shared production's observability API.
    Observability(observability::SystemRequest),
    /// One request to the shared production's infrastructure API.
    Infrastructure(infrastructure::SystemRequest),
    /// A bounded read answer, with no state-changing effect.
    Read { owner: Token, bytes: Box<[u8]> },
    /// The inline agent's lower LLM request.
    Llm { client: Token, request: smith_domain::Request },
    /// Feed this released continuation back as an event.
    ToChild(Released),
    /// Admission failed before a child changed; the sender retains its input.
    Busy,
    /// A failed journal commit or refused restart stopped this engine.
    Stop,
}

/// A turn or terminal the fleet asks the root to carry back by opaque token.
#[derive(Debug)]
pub(crate) enum Payload {
    Turn { task: u64, attempt: u64, turn: u32, spent: u64, read: Option<u64>, body: Box<[u8]> },
    Answer { task: u64, attempt: u64, spent: u64, end: tasks::End },
    Word(tasks::Word),
    Settled(core::SettledCall),
}
