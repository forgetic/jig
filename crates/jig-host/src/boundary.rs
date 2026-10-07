//! The host's boundary (domain/hosts.md, sections 4 to 7). Its root
//! translates core messages and the optional workspace and agent capabilities. A
//! request's `owner` names the hosted run or its call; a terminal echoes it.
//! The root owns application workspace items and interprets opaque tokens.

use alloc::boxed::Box;

use skein_lib::{Duration, ReplyTo, Token};

/// parent -> host
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// A named message with its sender label and words.
    InboundTyped {
        run: Token,
        attempt: Token,
        name: Token,
        sender: Box<[u8]>,
        words: Box<[u8]>,
    },
    /// A host or delivery call named in the run's own bytes.
    CalledTyped {
        owner: Token,
        call: Box<[u8]>,
        ask: Ask,
    },
    /// The agent withdrew its typed call after its deadline.
    WithdrawnTyped {
        owner: Token,
        call: Box<[u8]>,
    },
    /// An assignment with ordered turn bodies and calls settled after them.
    AssignTyped {
        reply_to: ReplyTo,
        assignment: AssignmentTyped,
    },
    /// A version-two run: transcript and committed call tail stay opaque.
    AssignV2 {
        reply_to: ReplyTo,
        assignment: AssignmentV2,
    },
    /// One completed conversation turn, from its started agent.
    Turn {
        owner: Token,
        turn: Turn,
    },
    /// A live agent fact, kept best effort for the engine.
    Facts {
        owner: Token,
        fact: Box<[u8]>,
    },
    /// The engine committed this turn; release it and grant agent read room.
    AcknowledgeTurn {
        run: Token,
        attempt: Token,
        turn: u32,
    },
    /// Version-two last word, with cumulative accounting.
    FinishedV2 {
        owner: Token,
        turns: u32,
        spent: u64,
        finish: FinishV2,
    },
    /// From the engine, a call: host the run of `assignment`, and answer once
    /// it has ended. A fresh call for an already hosted attempt is refused
    /// as busy; its parent deduplicates wire retransmissions before calling.
    Assign {
        reply_to: ReplyTo,
        assignment: Assignment,
    },
    /// From the engine: an inbound event for the run `run`'s attempt
    /// `attempt`, which goes down to the run as it arrives.
    Inbound {
        run: Token,
        attempt: Token,
        name: Token,
        event: Box<[u8]>,
    },
    /// From the engine: cancel the run `run`'s attempt `attempt`.
    Cancel {
        run: Token,
        attempt: Token,
    },
    Grant {
        run: Token,
        attempt: Token,
        grant: Grant,
    },
    /// From the engine: the answer to the relayed call `call` of the run
    /// `run`'s attempt `attempt`.
    Relayed {
        run: Token,
        attempt: Token,
        call: Token,
        answer: Box<[u8]>,
    },
    /// Terminal for a cancelled relay: its local delivery and wait have ended.
    RelayCancelled {
        call: Token,
    },
    /// From the top level: cancel every run hosted now, for `reason` (lost
    /// contact with the engine past its grace, or shutdown). The runs are
    /// cancelled one at a time, from the ready list. A root shutting down
    /// admits no more runs: assignments after it are refused as busy.
    CancelAll {
        reason: Reason,
    },
    /// From the top level: say what the host hosts, for the engine to keep or
    /// cancel on reconnecting.
    Report,
    /// From the top level: `answers` the host made have yet to be
    /// acknowledged by the engine. Each keeps its run's slot until it is, so
    /// that the engine, which frees a slot once it has the answer, never finds
    /// a host with more runs than slots.
    Unacknowledged {
        answers: u32,
    },
    /// Terminal for `Prepare`: the workspace is ready, and `workspace` names it
    /// from now on.
    Prepared {
        owner: Token,
        workspace: Token,
    },
    /// Terminal for `Prepare`: the workspace could not be prepared, and
    /// nothing of it is held.
    Unprepared {
        owner: Token,
        failure: Preparation,
        detail: Box<[u8]>,
    },
    /// The agent started, and `agent` names it from now on.
    Started {
        owner: Token,
        agent: Token,
    },
    /// A host call of the run, which the host answers with one `Reply`.
    /// `call` is the agent's name for it.
    Called {
        owner: Token,
        call: Token,
        ask: Ask,
    },
    /// The run withdrew its host call `call`, its own deadline for it having
    /// passed. A relayed call is answered at once as withdrawn; a delivery goes
    /// on, and is answered with how it went. A call answered already is not
    /// in flight, and nothing happens.
    Withdrawn {
        owner: Token,
        call: Token,
    },
    /// The agent could not take an inbound event for the run, for `bounce`.
    Bounced {
        owner: Token,
        name: Token,
        bounce: Bounce,
    },
    /// The run yielded: it waits for its next inbound event.
    Yielded {
        owner: Token,
    },
    /// The run said how it finishes. Its agent exits next. Said as it winds
    /// down after a stop, it is still the run's answer.
    Finished {
        owner: Token,
        finish: Finish,
    },
    /// The agent child domain is stopping the agent for `fault`.
    Faulted {
        owner: Token,
        fault: AgentFailure,
    },
    /// Terminal for `Start`: the agent and everything it started are gone.
    /// `detail` is for operators, such as the tail of its error output.
    Gone {
        owner: Token,
        detail: Box<[u8]>,
    },
    /// Terminal for a delivery, including the root's opaque left-work token.
    Delivered {
        owner: Token,
        delivery: Delivery,
    },
    /// Terminal for a save, including the root's opaque saved-work token.
    Saved {
        owner: Token,
        at: Option<Token>,
    },
}

/// host -> parent
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// Relay a named host tool call with its effect classification and input.
    RelayTyped {
        run: Token,
        attempt: Token,
        call: Box<[u8]>,
        delivery: Token,
        tool: Box<[u8]>,
        writes: bool,
        input: Box<[u8]>,
        deadline: Duration,
    },
    /// Pass a named message to the agent.
    DeliverTyped {
        agent: Token,
        name: Token,
        sender: Box<[u8]>,
        words: Box<[u8]>,
    },
    /// Answer a call under the agent's opaque name.
    ReplyTyped {
        agent: Token,
        call: Box<[u8]>,
        reply: Reply,
    },
    /// Start an agent with its activation and committed conversation state.
    StartTyped {
        owner: Token,
        workspace: Option<Token>,
        charter: Box<[u8]>,
        activation: u64,
        turns: Box<[Box<[u8]>]>,
        answered: Box<[AnsweredCall]>,
        grants: Box<[Grant]>,
    },
    /// Stable agent name on the engine wire; delivery identifies this local wait.
    RelayV2 {
        run: Token,
        attempt: Token,
        call: Token,
        delivery: Token,
        body: Box<[u8]>,
    },
    AnswerV2 {
        to: ReplyTo,
        run: Token,
        attempt: Token,
        answer: AnswerV2,
    },
    StartV2 {
        owner: Token,
        workspace: Option<Token>,
        charter: Box<[u8]>,
        transcript: Option<Box<[u8]>>,
        grants: Box<[Grant]>,
    },
    /// A transmission copy of a turn retained until engine acknowledgement.
    Turn {
        agent: Token,
        run: Token,
        attempt: Token,
        turn: Turn,
    },
    /// Whether the agent may read another turn after the retained bound.
    TurnCredit {
        agent: Token,
        read: bool,
    },
    DeliverV2 {
        owner: Token,
        workspace: Token,
        title: Box<[u8]>,
        body: Box<[u8]>,
    },
    /// To the engine, the answer to an `Assign`: exactly one per assignment.
    Answer {
        to: ReplyTo,
        run: Token,
        attempt: Token,
        answer: Answer,
    },
    /// To the engine: a host call of the run `run`'s attempt `attempt`, which
    /// the host names `call`, relayed as it is.
    Relay {
        run: Token,
        attempt: Token,
        call: Token,
        body: Box<[u8]>,
    },
    /// Cancel the local delivery and wait of a relay. Exactly one terminal
    /// follows: `RelayCancelled`, or `Relayed` if its answer won. Remote
    /// effects are not rolled back.
    CancelRelay {
        call: Token,
    },
    /// To the engine: an inbound event for the run `run`'s attempt `attempt`
    /// was not passed on, for `bounce`.
    Bounced {
        run: Token,
        attempt: Token,
        name: Token,
        bounce: Bounce,
    },
    /// To the top level, the answer to `Report`: every run hosted, in the
    /// order of the engine's names for them.
    Hosting {
        runs: Box<[Hosting]>,
    },
    /// Prepare `workspace` for the hosted run `owner`.
    Prepare {
        owner: Token,
        workspace: Workspace,
    },
    /// Abandon the prepare in flight for `owner`, whose run is cancelled. Its
    /// terminal still comes: `Unprepared`, or `Prepared` if the prepare won
    /// the race, and then the workspace is released.
    Abort {
        owner: Token,
    },
    /// Start an agent on `charter`, resumed from `snapshot` if there is one.
    /// A run without workspace items starts with no workspace.
    Start {
        owner: Token,
        workspace: Option<Token>,
        charter: Box<[u8]>,
        snapshot: Option<Box<[u8]>>,
        grants: Box<[Grant]>,
    },
    /// An inbound event for the run of the agent `agent`.
    Deliver {
        agent: Token,
        name: Token,
        event: Box<[u8]>,
    },
    /// The one answer to the host call `call` of the agent `agent`.
    Reply {
        agent: Token,
        call: Token,
        reply: Reply,
    },
    /// Stop the agent `agent`: cancel its run, then end what is left of it
    /// past the grace. Its start's `Gone` comes once it has all gone. Sent
    /// whenever its run leaves live, also once the run has said how it
    /// finishes or the agent was faulted, when it changes nothing: the agent
    /// child domain winds the agent down then anyway.
    Stop {
        agent: Token,
    },
    Grant {
        agent: Token,
        grant: Grant,
    },
    /// Ask the workspace to deliver the run's work for call `owner`.
    DeliverWorkspace {
        owner: Token,
        workspace: Token,
        message: Box<[u8]>,
    },
    /// Ask the workspace to save unfinished work.
    Save {
        owner: Token,
        workspace: Token,
    },
    /// The run is done with `workspace`: it goes back to the cache.
    Release {
        workspace: Token,
    },
}

/// The host's agent capability, translated by its root (domain/hosts.md,
/// section 6). These requests carry no process or channel vocabulary.
#[derive(PartialEq, Eq, Debug)]
pub enum ToAgent {
    /// Pass a named message with its sender label and words.
    MessageTyped {
        agent: Token,
        name: Token,
        sender: Box<[u8]>,
        words: Box<[u8]>,
    },
    /// Answer a call under the agent's opaque name.
    AnswerTyped {
        agent: Token,
        call: Box<[u8]>,
        reply: Reply,
    },
    /// Start an agent with its activation and committed conversation state.
    StartTyped {
        owner: Token,
        workspace: Option<Token>,
        charter: Box<[u8]>,
        activation: u64,
        turns: Box<[Box<[u8]>]>,
        answered: Box<[AnsweredCall]>,
        grants: Box<[Grant]>,
    },
    ReadCredit {
        agent: Token,
        read: bool,
    },
    StartV2 {
        owner: Token,
        workspace: Option<Token>,
        charter: Box<[u8]>,
        transcript: Option<Box<[u8]>>,
        grants: Box<[Grant]>,
    },
    Start {
        owner: Token,
        workspace: Option<Token>,
        charter: Box<[u8]>,
        snapshot: Option<Box<[u8]>>,
        grants: Box<[Grant]>,
    },
    Message {
        agent: Token,
        name: Token,
        event: Box<[u8]>,
    },
    Answer {
        agent: Token,
        call: Token,
        reply: Reply,
    },
    Grant {
        agent: Token,
        grant: Grant,
    },
    Cancel {
        agent: Token,
    },
}

/// What the agent capability reports back to the host. Its root translates
/// the application agent's own vocabulary into these events.
#[derive(PartialEq, Eq, Debug)]
pub enum FromAgent {
    /// A named host or delivery call with typed arguments.
    CalledTyped {
        owner: Token,
        call: Box<[u8]>,
        ask: Ask,
    },
    /// The agent withdrew its named call.
    WithdrawnTyped {
        owner: Token,
        call: Box<[u8]>,
    },
    Turn {
        owner: Token,
        turn: Turn,
    },
    Facts {
        owner: Token,
        fact: Box<[u8]>,
    },
    FinishedV2 {
        owner: Token,
        turns: u32,
        spent: u64,
        finish: FinishV2,
    },
    Started {
        owner: Token,
        agent: Token,
    },
    Called {
        owner: Token,
        call: Token,
        ask: Ask,
    },
    Withdrawn {
        owner: Token,
        call: Token,
    },
    Bounced {
        owner: Token,
        name: Token,
        bounce: Bounce,
    },
    Yielded {
        owner: Token,
    },
    Finished {
        owner: Token,
        finish: Finish,
    },
    Faulted {
        owner: Token,
        fault: AgentFailure,
    },
    Gone {
        owner: Token,
        detail: Box<[u8]>,
    },
}

impl Event {
    /// Bring an agent capability's report into the host's event stream.
    #[must_use]
    pub fn from_agent(event: FromAgent) -> Event {
        match event {
            FromAgent::CalledTyped { owner, call, ask } => Event::CalledTyped { owner, call, ask },
            FromAgent::WithdrawnTyped { owner, call } => Event::WithdrawnTyped { owner, call },
            FromAgent::Turn { owner, turn } => Event::Turn { owner, turn },
            FromAgent::Facts { owner, fact } => Event::Facts { owner, fact },
            FromAgent::FinishedV2 { owner, turns, spent, finish } => Event::FinishedV2 { owner, turns, spent, finish },
            FromAgent::Started { owner, agent } => Event::Started { owner, agent },
            FromAgent::Called { owner, call, ask } => Event::Called { owner, call, ask },
            FromAgent::Withdrawn { owner, call } => Event::Withdrawn { owner, call },
            FromAgent::Bounced { owner, name, bounce } => Event::Bounced { owner, name, bounce },
            FromAgent::Yielded { owner } => Event::Yielded { owner },
            FromAgent::Finished { owner, finish } => Event::Finished { owner, finish },
            FromAgent::Faulted { owner, fault } => Event::Faulted { owner, fault },
            FromAgent::Gone { owner, detail } => Event::Gone { owner, detail },
        }
    }
}

impl Request {
    /// Take an agent request for translation by the root, returning
    /// any request addressed to another capability unchanged.
    pub fn to_agent(self) -> Result<ToAgent, Request> {
        match self {
            Request::DeliverTyped { agent, name, sender, words } => {
                Ok(ToAgent::MessageTyped { agent, name, sender, words })
            }
            Request::ReplyTyped { agent, call, reply } => Ok(ToAgent::AnswerTyped { agent, call, reply }),
            Request::StartTyped { owner, workspace, charter, activation, turns, answered, grants } => {
                Ok(ToAgent::StartTyped { owner, workspace, charter, activation, turns, answered, grants })
            }
            Request::StartV2 { owner, workspace, charter, transcript, grants } => {
                Ok(ToAgent::StartV2 { owner, workspace, charter, transcript, grants })
            }
            Request::Start { owner, workspace, charter, snapshot, grants } => {
                Ok(ToAgent::Start { owner, workspace, charter, snapshot, grants })
            }
            Request::Deliver { agent, name, event } => Ok(ToAgent::Message { agent, name, event }),
            Request::Reply { agent, call, reply } => Ok(ToAgent::Answer { agent, call, reply }),
            Request::Grant { agent, grant } => Ok(ToAgent::Grant { agent, grant }),
            Request::TurnCredit { agent, read } => Ok(ToAgent::ReadCredit { agent, read }),
            Request::Stop { agent } => Ok(ToAgent::Cancel { agent }),
            other @ (Request::RelayTyped { .. }
            | Request::RelayV2 { .. }
            | Request::AnswerV2 { .. }
            | Request::Turn { .. }
            | Request::DeliverV2 { .. }
            | Request::Answer { .. }
            | Request::Relay { .. }
            | Request::CancelRelay { .. }
            | Request::Bounced { .. }
            | Request::Hosting { .. }
            | Request::Prepare { .. }
            | Request::Abort { .. }
            | Request::DeliverWorkspace { .. }
            | Request::Save { .. }
            | Request::Release { .. }) => Err(other),
        }
    }
}

/// What the core gives the host for one run (domain/hosts.md, section 2).
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct Assignment {
    /// The engine's names for the run and for this attempt at it.
    pub run: Token,
    pub attempt: Token,
    /// Nothing when the run has no workspace items to prepare.
    pub workspace: Option<Workspace>,
    /// Whether the workspace must save unfinished work before release.
    pub save: bool,
    /// What the agent's run is given, passed through.
    pub charter: Box<[u8]>,
    /// The state of a parked run to resume from, passed through.
    pub snapshot: Option<Box<[u8]>>,
    pub grants: Box<[Grant]>,
}

/// An explicit second-version assignment. A snapshot is invalid here.
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct AssignmentV2 {
    pub assignment: Assignment,
    pub transcript: Option<Box<[u8]>>,
}

/// An assignment with the conversation state the agent consumes (hosts.md, 2).
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct AssignmentTyped {
    pub assignment: Assignment,
    /// Ordered committed turn bodies. An empty list starts a fresh conversation.
    pub turns: Box<[Box<[u8]>]>,
    /// Calls settled after the last committed turn, in commit order.
    pub answered: Box<[AnsweredCall]>,
}

/// A call committed after the last turn, to tell the next activation.
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct AnsweredCall {
    /// The run's opaque call name.
    pub name: Box<[u8]>,
    /// The host tool's name as declared by the charter.
    pub tool: Box<[u8]>,
    pub answer: SettledAnswer,
}

/// The committed answer to a host call or workspace delivery.
#[derive(PartialEq, Eq, Hash, Debug)]
pub enum SettledAnswer {
    /// A host tool's answer, including whether it is an error.
    Host { error: bool, body: Box<[u8]> },
    /// A workspace delivery's outcome and opaque, versioned evidence for replay.
    Delivery { outcome: DeliveryOutcome, evidence: Box<[u8]> },
}

/// A turn's cumulative spend and last read message name are opaque accounting.
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct Turn {
    pub turn: u32,
    pub spent: u64,
    pub read: Option<Token>,
    pub body: Box<[u8]>,
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub enum FinishV2 {
    Ended { outcome: Box<[u8]> },
    Parked,
    Failed { failure: RunFailure },
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub struct AnswerV2 {
    pub turns: u32,
    pub spent: u64,
    pub ending: EndingV2,
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub enum EndingV2 {
    Refused(Refusal),
    Ended { outcome: Box<[u8]>, work: Work },
    Parked { work: Work },
    Failed { failure: Failure, detail: Box<[u8]>, work: Work },
}

/// The application's workspace items, held by its root.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Workspace {
    /// The task whose runs may reuse the workspace.
    pub workstream: u64,
    /// The root's token for the application's bounded workspace items.
    pub items: Token,
}

/// A host call of a run.
#[derive(PartialEq, Eq, Hash, Debug)]
pub enum Ask {
    /// A host tool call, carrying its charter name, write flag, input and time left.
    RelayTyped {
        tool: Box<[u8]>,
        writes: bool,
        input: Box<[u8]>,
        deadline: Duration,
    },
    DeliverV2 {
        title: Box<[u8]>,
        body: Box<[u8]>,
    },
    /// Deliver work through the application's workspace.
    Deliver {
        message: Box<[u8]>,
    },
    /// A call relayed to the engine as it is.
    Relay {
        body: Box<[u8]>,
    },
}

/// The answer to a host call.
#[derive(PartialEq, Eq, Hash, Debug)]
pub enum Reply {
    /// The engine's answer to a relayed call, as it is.
    Relayed { answer: Box<[u8]> },
    /// How the delivery went.
    Delivered(Delivery),
    /// The run is cancelled or ending: nothing was done. A delivery in flight as
    /// the run leaves live is waited for, and answered with how it went.
    Unavailable,
    /// The run withdrew the relayed call: nothing more is done for it, and
    /// the engine's answer, if one comes, is dropped.
    Withdrawn,
    /// The run has as many calls in flight as it may, or a delivery in flight
    /// already: nothing was done. Calls answered within the loop's current
    /// iteration keep their slots until its reclaim point, so a busy call may
    /// find room in the next.
    Busy,
}

/// A workspace's delivery outcome and the root's token for what the run left.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Delivery {
    pub outcome: DeliveryOutcome,
    pub left: Token,
    /// Whether the workspace changed even if the delivery reported a failure.
    pub changed: bool,
}

/// The result of making a delivery through an application's workspace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DeliveryOutcome {
    Delivered,
    Nothing,
    Stale,
    Refused,
    Failed,
}

/// How the run finishes, as it says.
#[derive(PartialEq, Eq, Hash, Debug)]
pub enum Finish {
    /// It ended with `outcome`, its declared outcome, passed through.
    Ended { outcome: Box<[u8]> },
    /// It parked, handing over `snapshot` if it has one.
    Parked { snapshot: Option<Box<[u8]>> },
    /// It failed, for `failure`.
    Failed { failure: RunFailure },
}

/// Why an inbound event was not passed on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bounce {
    /// It holds more bytes than the limits allow.
    TooLarge,
    /// The run is not live yet, and holds as many events as it may.
    Full,
    /// The run is ending.
    Ending,
}

/// A hosted run, for the reconnect report.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Hosting {
    pub run: Token,
    pub attempt: Token,
    pub phase: Phase,
}

/// Where a hosted run is in its lifecycle (hosts.md, section 6).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Phase {
    Preparing,
    Starting,
    /// Its agent is at work.
    Active,
    /// It yielded, and waits for its next inbound event.
    Waiting,
    /// How it ends is decided: it is stopping, saving, or waiting for what is
    /// in flight to settle, and answers next.
    Ending,
}

/// The answer to an `Assign`.
#[derive(PartialEq, Eq, Hash, Debug)]
pub enum Answer {
    /// Refused at the entrance: nothing was done.
    Refused(Refusal),
    /// The run ended with `outcome`.
    Ended { outcome: Box<[u8]>, work: Work },
    /// The run parked, with its snapshot if it had one.
    Parked { snapshot: Option<Box<[u8]>>, work: Work },
    /// The run failed, for `failure`; `detail` is for operators, never for an
    /// LLM.
    Failed { failure: Failure, detail: Box<[u8]>, work: Work },
}

/// The root's tokens for what a run delivered and saved on its workspace.
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct Work {
    pub left: Option<Token>,
    pub saved: Option<Token>,
}

/// Why an assignment was refused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Refusal {
    /// Every slot is taken (by a run hosted, or one whose answer the engine
    /// has yet to acknowledge), the run is hosted already under another
    /// attempt that has not answered yet, or the host is shutting down. A
    /// later retry, on this host or another, may find room.
    Busy,
    /// The assignment does not fit the limits.
    Invalid(Invalid),
}

/// What about an assignment does not fit the limits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Invalid {
    /// The charter holds more bytes than a run may.
    Charter,
    /// The snapshot holds more bytes than a run may.
    Snapshot,
    Transcript,
    Version,
    Grants,
}

/// Why a hosted run failed (hosts.md, section 6): what the engine acts on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Failure {
    /// Its workspace could not be prepared.
    Unprepared(Preparation),
    /// The run failed, as it reports it.
    Run(RunFailure),
    /// Its agent failed.
    Agent(AgentFailure),
    /// It was cancelled.
    Cancelled(Reason),
}

/// Why a workspace could not be prepared.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Preparation {
    /// A later attempt may work.
    Transient,
    /// A resource needs a change before another attempt can work.
    Permanent { resource: Option<Token> },
}

/// Why a run failed, as it reports it (hosts.md, section 6).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RunFailure {
    /// The LLM could not do the work.
    Model,
    /// The run's budget ran out.
    Budget,
    /// The LLM did not keep to the run's rules.
    Policy,
    /// The run was cancelled.
    Cancelled,
    /// The run's target changed since it started.
    Stale,
    /// Provider quota was exhausted; a later attempt may succeed.
    Exhausted,
}

/// How an agent failed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AgentFailure {
    /// It could not be started.
    Unstarted,
    /// It exited without saying how its run finishes.
    Exited,
    /// It broke the channel's rules, or said more than the limits allow.
    Rules,
    /// The watchdog stopped it: no progress.
    NoProgress,
    /// The watchdog stopped it: past its wall time.
    WallTime,
}

/// Why a run was cancelled.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Reason {
    /// The engine cancelled it.
    Engine,
    /// A worker root lost contact with the core for longer than the grace.
    Contact,
    /// The root is shutting down.
    Shutdown,
}

/// Names of credential grants; their values belong to the protocol layer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Grant {
    pub account: u32,
    pub generation: u64,
    pub valid: Duration,
}
