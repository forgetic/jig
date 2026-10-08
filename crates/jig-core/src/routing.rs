//! Child entry points and their requests in the core's vocabulary
//! (`domain/engine.md`, sections 3 and 4). A root receives a whole child
//! request batch and routes it within the same decision.

use alloc::boxed::Box;
use jig_core_accounts as accounts;
use jig_core_authority as authority;
use jig_core_brief as brief;
use jig_core_fleet as fleet;
use jig_core_notes as notes;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_views as views;
use skein_lib::{Env, JournalRoom, List, Queue, ReplyTo, Token};

use crate::{
    CallKey, CallPart, CallRecord, Core, CoreKey, CoreRecord, Delegate, Dependency, EscalationDecisionRecord,
    GoalRoute, Key, PersonProposalRoute, PersonTaskRoute, ProcedureAction, Record, RoutedCall, RunCharter, RunProof,
    SentRoute,
};

impl Core {
    /// Retain a numbered connector call until its translated answer arrives.
    #[must_use]
    pub fn reserve_connector_call(&mut self, key: CallKey) -> bool {
        self.pending_calls.insert(key, true) == Ok(None)
    }

    /// Retain a connector subscription's task-hub continuation with its named call.
    #[must_use]
    pub fn reserve_connector_subscription(&mut self, token: Token, key: CallKey, subscription: u64) -> bool {
        if !self.reserve_connector_call(key) {
            return false;
        }
        self.routing_calls.insert(token, RoutedCall::Subscribe { key, subscription }) == Ok(None)
    }
}

/// A request from the core to its application root. Its variant fixes the
/// commit relationship before the root translates or wraps it.
#[derive(Debug)]
pub enum Request {
    /// Store mutation in the current decision.
    Write(Write),
    /// Synchronous connector handoff within this decision.
    Ask { connector: u16, ask: Ask },
    /// Output held until this decision is durable.
    Held(Box<Held>),
    /// Read answer or refusal that decided nothing.
    Now(Box<Now>),
    /// All synchronous handoffs for this decision have completed.
    Decided,
}

/// A question or retained-section command for a numbered connector.
#[derive(Debug)]
pub enum Ask {
    /// A core-selected effect handoff completed inside this decision.
    Effect(crate::connector::Ask),
    /// Tell the owning connector that this task took a write hold.
    Hold { task: u64, resource: tasks::Name },
    /// Tell the owning connector to end a task's topic interest.
    EndTopic { task: u64, subscription: u64 },
    /// Gather a section within the brief's budget.
    Gather { section: Token, budget: u32 },
    /// Cut a retained section to a smaller bound.
    CutTo { section: Token, size: u32 },
    /// Release a retained section.
    Drop { section: Token },
    /// Offer an authorized party's resource to its owning connector.
    Adopt { request: Token, project: u32, adoption: people::Adoption },
    /// Settle a closing task's connector effects before its final release.
    Close { task: u64, root: u64, ending: tasks::Ending },
    /// Release a closing task's connector holds through a numbered outbox entry.
    Release { task: u64, root: u64, ending: tasks::Ending, entry: u64 },
    /// Tell a connector that a fenced host attempt was lost.
    Lost { task: u64, attempt: u64 },
    /// Ask this connector which bounded task holdings its resources require.
    TaskHoldings { request: Token, project: u32, root: u64, number: u64, executor: tasks::Executor, spec: tasks::Spec },
    /// Ask one connector for the holdings of every member of a named delegate batch.
    DelegateHoldings { request: Token, from: u64, members: Box<[HoldingsNeed]> },
    /// Ask one connector for a procedure's delegated batch holdings.
    ProcedureHoldings { task: u64, step: u64, members: Box<[HoldingsNeed]> },
    /// Project a tracked goal in this connector's own terms after its task save.
    ProjectGoal { goal: Box<tasks::TaskRecord> },
    /// Complete a connector-owned subscription after the task hub accepts it.
    SubscriptionDone { request: Token, key: CallKey, subscription: u64 },
    /// Complete a connector-owned unsubscription after the task hub accepts it.
    UnsubscriptionDone { request: Token, key: CallKey },
    /// Complete a prepared claim through the connector that owns its writes.
    ClaimDone { task: u64, attempt: u64 },
    /// Discard one rejected connector subscription flight.
    DropSubscription { request: Token },
    /// Ask a connector whether a refused repair belongs to a held task.
    RepairRefused { repair: Option<u64> },
    /// Let a connector settle a refused procedure delegation.
    DelegateRefused { task: Option<u64> },
    /// Start one admitted procedure through its numbered connector.
    StartProcedure { context: Box<tasks::RunContext>, step: u64 },
}

/// Store mutation issued by a child of the core.
#[derive(Debug)]
pub enum Write {
    /// Save one authentic core or child record.
    Save(Record),
    /// Erase one authentic core or child key.
    Erase(Key),
}

/// Core output that must follow the decision's commit.
#[derive(Debug)]
pub enum Held {
    /// A connector may make this entry only after its keeping decision commits.
    MakeEffect { connector: u16, entry: u64 },
    /// A rendered answer and all opaque evidence have reached the store.
    SettledCall { to: ReplyTo, key: CallKey, call: crate::SettledCall },
    /// A claimed host receives the parent's typed conversation references.
    AssignTyped { channel: Token, run: Token, attempt: Token, activation: u64, assignment: fleet::TypedAssignment },
    /// The host receives an answer under its original opaque call name.
    RelayedTyped { channel: Token, run: Token, attempt: Token, call: Box<[u8]>, answer: Token },
    /// The host receives the same named label and words the root supplied.
    InboundTyped { channel: Token, run: Token, attempt: Token, message: fleet::TypedMessage },
    /// A saved word relayed to its fenced live attempt after commit.
    Relay { task: u64, attempt: u64, previous: Option<u64>, word: tasks::Word },
    /// A party's keyed terminal, released after its decision is durable.
    PeopleReply { to: ReplyTo, sign_in: Option<u64>, reply: people::Reply },
    /// A named core call answer, assembled and delivered after its decision commits.
    CallAnswer { to: ReplyTo, key: CallKey, part: CallPart },
    /// A claimed run begins on a host after its assignment is committed.
    Assign { channel: Token, run: Token, attempt: Token },
    /// A started run becomes visible after the same claim commits.
    ViewStart { run: Token, attempt: Token },
    /// A finished task becomes visible after its terminal commits.
    ViewFinished { task: u64 },
    /// A task phase changed for current live watchers.
    ViewTaskPhase { task: Token, trees: Box<[Token]>, project: u32, phase: u32, priority: Option<u32> },
    /// An ended task's result is delivered to its person requester.
    Result { person: u64, task: u64, words: Box<[u8]> },
    /// A host may forget its terminal only after the task decision commits.
    Acknowledge { channel: Token, run: Token, attempt: Token },
    /// The fleet may forget an accepted task terminal after its decision commits.
    TaskTerminalAcknowledged { task: u64, attempt: u64 },
    /// The fleet may forget one accepted turn after its decision commits.
    TaskTurnKept { task: u64, attempt: u64, turn: u32 },
    /// A newly accepted turn becomes visible after commit.
    ViewTurn { task: u64, attempt: u64, turn: u32 },
    /// A host may forget its turn only after that turn commits.
    AcknowledgeTurn { channel: Token, run: Token, attempt: Token, turn: u32 },
    /// Fence and cancel one hosted attempt after the decision commits.
    Cancel { channel: Token, run: Token, attempt: Token },
    /// Refuse one host request after the decision commits.
    Refuse { channel: Token },
    /// Ask the host to retain and retry a turn that did not enter a decision.
    TurnBusy { channel: Token, run: Token, attempt: Token, turn: u32 },
    /// Deliver a named call answer retained by the application's root.
    Relayed { channel: Token, run: Token, attempt: Token, call: Token, answer: Token },
    /// A committed inbox word is handed to the current fenced host attempt.
    Inbound { channel: Token, run: Token, attempt: Token, word: tasks::Word },
    /// Cancel the current fenced run after its task decision commits.
    StopRun { task: u64, attempt: u64 },
    /// Store page requested by the notes child after earlier commits.
    NotesLoad { owner: Token, range: notes::Range },
    /// A note write's accepted revision.
    NotesWritten { owner: Token, name: u64, revision: u32 },
    /// A note deletion's accepted name.
    NotesDeleted { owner: Token, name: u64 },
}

/// Core output that follows no mutation.
#[derive(Debug)]
pub enum Now {
    /// An effect wait or capacity refusal changed no durable state.
    EffectAnswer { to: ReplyTo, key: CallKey, part: CallPart },
    /// A settled answer exceeded its bounded record or no retained call owns it.
    SettledCallRefused { to: ReplyTo, key: CallKey, name: Box<[u8]>, tool: Box<[u8]> },
    /// The protocol layer decodes this attested tool and input at the root boundary.
    CallTyped { to: ReplyTo, run: Token, attempt: Token, call: fleet::TypedCall },
    /// A fenced or over-capacity typed call changed nothing.
    DropTyped { call: fleet::TypedCall },
    /// The host did not take this typed message; its durable inbox word remains unread.
    UndeliveredTyped { run: Token, attempt: Token, message: fleet::TypedMessage },
    /// A sign-in whose core-owned person or session counter is exhausted.
    SignInRefused { to: ReplyTo },
    /// A volatile watch refused before any durable decision.
    WatchRefused { watcher: Token, refusal: people::Refusal },
    /// Secret-free account protocol output with no store decision.
    Account(accounts::Request),
    /// A live view observation or terminal with no durable mutation.
    View(views::Request),
    /// One page of scope index lines.
    NotesIndexed { owner: Token, lines: List<notes::Line>, more: u32 },
    /// One page of recalled entries.
    NotesRecalled { owner: Token, entries: List<notes::Entry>, more: bool },
    /// A note request refused before changing state.
    NotesRefused { owner: Token, why: notes::Refusal },
    /// The notes child could not serve this named call yet; the host retries its name.
    NoteBusy { to: ReplyTo, key: CallKey },
    /// Forget a root-owned host payload that the fleet no longer needs.
    DropPayload { payload: Token },
    /// Forget the application's assignment for a run the host never started.
    DropAssignment { task: u64 },
    /// Read the root-owned turn payload before routing the charged turn.
    TurnPayload { run: Token, attempt: Token, turn: u32, body: Token },
    /// Read and translate the root-owned answer payload before the task terminal.
    AnswerPayload { run: Token, attempt: Token, payload: Token },
    /// Read the root-owned transcript after the task accepts its charge.
    AcceptedTurn { payload: Token, task: u64, attempt: u64, turn: u32, accepted: tasks::Accepted },
    /// Inspect a root-owned host payload after the task child refuses it.
    RefusedPayload { request: Token, problem: tasks::Problem },
    /// Ask the root for its current readiness before a due activation route.
    Activate { context: Box<tasks::RunContext> },
    /// Reserve the root's transcript read slot for an admitted agent run.
    PrepareAgent { context: Box<tasks::RunContext> },
    /// Start task preparation and assemble its first store load.
    StartPreparation { task: u64, transcript_waiter: Option<Token> },
    /// Load one proposal decision that has left live task memory.
    HistoricalProposal { request: Token, person: u64, project: u32, proposer: u64, proposal: u64 },
    /// Assemble connector-held sections for a still-current brief.
    CompleteBrief { brief: Token, order: Box<[brief::GatherPlaced]> },
    /// Core-owned sections are ready; the root may append its connector sections.
    BriefCorePlanned { task: u64, parent: Option<u64>, sections: List<brief::Planned> },
    /// The root assembles a numbered connector workspace for this core-selected attempt.
    WorkspaceRequest { task: u64, attempt: u64, context: Box<tasks::RunContext> },
    /// Core admission and claim preparation failed; discard root-owned assembly parts.
    RunPreparationFailed { task: u64 },
    /// Core-owned run parts are ready for root assembly with the connector workspace.
    RunPrepared {
        task: u64,
        attempt: u64,
        charter: u32,
        run: Box<RunCharter>,
        inbox: Box<[tasks::Word]>,
        saved: Box<[tasks::SavedResource]>,
        transcript: Box<[Box<[u8]>]>,
        answered: Box<[CallKey]>,
        grant: accounts::Grant,
    },
    /// Load one held-chat decision that has left live task memory.
    HistoricalEscalation { request: Token, person: u64, project: u32, task: u64, revision: u64 },
    /// Translate a task inspection through the root's authenticated read slot.
    EscalationInspection { waiter: Token, context: Option<Box<tasks::EscalationContext>> },
    /// Return a visible held chat to its authenticated reader.
    EscalationReply { to: ReplyTo, person: u64, context: Box<tasks::EscalationContext> },
    /// Refuse an authenticated held-chat read.
    EscalationRefused { to: ReplyTo, why: people::Refusal },
    /// Translate a root-owned named-call payload after the fleet admits its relay.
    CallPayload { to: ReplyTo, run: Token, attempt: Token, body: Token },
    /// The first child admitted from a connector procedure's batch, if any.
    ProcedureDelegateOutcome { task: u64, step: u64, child: Option<u64> },
    /// A restored task row was refused; the root must fail startup.
    RestoreRefused,
}

/// Limits of the core's child routes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// Number of numbered connectors retained by the core.
    pub connectors: u32,
    /// Bytes retained for one task's resumable transcript tail.
    pub resume_bytes: u32,
    /// Owned bytes of the configured run policy and its model alternatives.
    pub run_bytes: u32,
    /// Maximum encoded mutable policy row the application journal accepts.
    pub policy_bytes: u64,
    /// Bound on an archived person escalation rejection reason.
    pub escalation_reason_bytes: u32,
    /// Historical result load slots.
    pub load_slots: u32,
    /// Named call replay and in-flight route slots.
    pub call_records: u32,
    /// Total bytes of a retained opaque call name, tool name and settled answer.
    pub call_answer_bytes: u32,
    /// The task hub's finite work.
    pub tasks: tasks::Limits,
    /// Policy tables and authority payload bounds owned by the core.
    pub authority: authority::Limits,
    /// Parties and their requests.
    pub people: people::Limits,
    /// Host slots and attempts.
    pub fleet: fleet::Limits,
    /// Brief gathering.
    pub brief: brief::Limits,
    /// Maximum fragments when rendering a core-owned brief section.
    pub brief_parts: u32,
    /// Core-owned section budgets; connector budgets stay with their owner.
    pub brief_core_budgets: CoreBriefBudgets,
    /// Secret-free account lifetimes.
    pub accounts: accounts::Limits,
    /// Scoped notes and their pages.
    pub notes: notes::Limits,
    /// Live view streams.
    pub views: views::Limits,
}

/// Budgets for the core's own brief sections.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CoreBriefBudgets {
    pub task: u32,
    pub dependencies: u32,
    pub attempts: u32,
    pub plan: u32,
    /// Maximum note index text in one brief.
    pub notes: u32,
}

/// One event routed to a child of the core.
#[derive(Debug)]
pub enum Event {
    /// A protocol-rendered answer, with opaque workspace evidence preserved verbatim.
    SettledCall { to: ReplyTo, key: CallKey, call: crate::SettledCall },
    /// A connector owns the decoded effect under this transient owner.
    EffectStart { owner: Token, connector: u16, origin: crate::EffectOrigin },
    /// Connector handoffs and outbox outcomes in the core vocabulary.
    EffectConnector(crate::connector::Event),
    /// Release effect answers whose absolute answer deadline has passed.
    EffectDeadline,
    /// Allocate the person and session named by a new party sign-in.
    SignIn { reply_to: ReplyTo, identity: people::Identity },
    /// Admit and open a volatile watch across the people and views children.
    Watch { watcher: Token, sign_in: u64, key: [u8; 16], project: u32, subject: people::WatchSubject, ready: bool },
    /// Task lifecycle or hub work.
    Tasks(tasks::Event),
    /// An application asks for one admitted recurring core procedure.
    StartRecurring { project: u32, authority: tasks::Authority, template: tasks::RecurringTemplate },
    /// Begin core-owned brief planning for one preparing task.
    StartBrief { task: u64 },
    /// The notes child supplied the brief's bounded index before connector gathering.
    BriefNotes { task: u64, lines: List<notes::Line>, more: u32 },
    /// The root assembled all selected connector sections, or found a missing owner.
    BriefAssembled { task: u64, ready: bool },
    /// The root translated the selected connector's workspace writes.
    WorkspacePrepared { task: u64, attempt: u64, writes: Option<Box<[authority::Write]>> },
    /// The root returned translated workspace names after assembling the run assignment.
    ClaimPrepared { task: u64, attempt: u64, budget: u64, writes: Box<[tasks::Name]> },
    /// A connector refused the held names of a pending task claim.
    ClaimRefused { task: u64 },
    /// The root translated the holdings for a core-authorized connector repair.
    ConnectorRepair {
        seed: Box<crate::connector::RepairSeed>,
        executor: tasks::Executor,
        spec: tasks::Spec,
        contract: tasks::Contract,
        holdings: Box<[tasks::Holding]>,
    },
    /// A new deployment funding period, checked against core policy.
    Period { project: u32, period: u64, budget: u64 },
    /// A required preparation read or connector section could not be assembled.
    PreparationFailed { task: u64 },
    /// A task continuation for a person's pending proposal.
    PersonProposal(tasks::Event),
    /// A task continuation for a person's pending escalation choice.
    PersonEscalation(tasks::Event),
    /// A task continuation for a named escalation call.
    TaskEscalation(tasks::Event),
    /// Party identity or request.
    People(people::Event),
    /// Host, run or attempt.
    Fleet(fleet::Event),
    /// Root-translated contents of a host turn payload.
    TurnPayload { run: Token, attempt: Token, turn: u32, body: Token, read: Option<u64>, cumulative: u64 },
    /// Root-translated contents of a host answer payload.
    AnswerPayload {
        run: Token,
        attempt: Token,
        payload: Token,
        cumulative: u64,
        end: tasks::End,
        saved: Option<Box<[tasks::SavedResource]>>,
        invalid_saved: bool,
    },
    /// Root-translated accepted turn and its owned transcript.
    AcceptedTurn {
        task: u64,
        attempt: u64,
        turn: u32,
        accepted: tasks::Accepted,
        cumulative: u64,
        read: Option<u64>,
        transcript: Box<[u8]>,
    },
    /// Root-translated host payload for a refused task request.
    RefusedPayload { request: Token, problem: tasks::Problem, payload: Option<PayloadRefusal> },
    /// An activation with the root's current journal and connector readiness.
    Activate { context: Box<tasks::RunContext>, ready: bool },
    /// The root's transcript slot reservation for an admitted agent run.
    PreparedAgent { context: Box<tasks::RunContext>, transcript_waiter: Option<Token>, busy: bool },
    /// One archived proposal decision, translated from the root's store row.
    HistoricalProposal {
        request: Token,
        person: u64,
        project: u32,
        proposer: u64,
        proposal: u64,
        row: Option<crate::ProposalDecisionRecord>,
        busy: bool,
    },
    /// One archived held-chat decision, translated from the root's store row.
    HistoricalEscalation {
        request: Token,
        person: u64,
        project: u32,
        task: u64,
        revision: u64,
        row: Option<EscalationDecisionRecord>,
        busy: bool,
    },
    /// The root's authenticated held-chat reader and the task inspection.
    EscalationRead { to: ReplyTo, person: u64, task: u64, context: Option<Box<tasks::EscalationContext>> },
    /// One root-translated named-call answer that needs a durable core decision.
    NamedAnswer { to: ReplyTo, key: CallKey, part: CallPart },
    /// One generic named-call action translated from the root's host vocabulary.
    NamedAction { to: ReplyTo, key: CallKey, action: NamedAction },
    /// Historical inputs have been loaded and validated by the root's store transport.
    DelegateValidated { to: ReplyTo, key: CallKey, batch: Box<[Delegate]>, stubs: Box<[tasks::Stub]> },
    /// One historical input load failed validation before named delegation.
    DelegateInputFailed { to: ReplyTo, key: CallKey },
    /// One connector's batch holdings, in the same order as the core's request.
    DelegateHoldings { request: Token, connector: u16, holdings: Option<Box<[Box<[tasks::Holding]>]>> },
    /// One connector's procedure batch holdings in requested order.
    ProcedureHoldings { task: u64, step: u64, connector: u16, holdings: Option<Box<[Box<[tasks::Holding]>]>> },
    /// A numbered connector's procedure choice in the core's vocabulary.
    ProcedureStep { task: u64, step: u64, connector: u16, code: u32, action: ProcedureAction },
    /// A gathered section.
    Brief(brief::GatherEvent),
    /// One numbered connector's answer to a task-holdings handoff.
    Holdings { request: Token, connector: u16, holdings: Option<Box<[tasks::Holding]>> },
    /// An account refresh or grant.
    Account(accounts::Event),
    /// A live view.
    View(views::Event),
    /// A scoped note.
    Notes(notes::Event),
}

/// Person choice retained only while the task child decides its exact revision.
#[derive(PartialEq, Eq, Debug)]
pub(crate) struct PersonEscalation {
    request: Token,
    requester: u64,
    person: u64,
    project: u32,
    task: u64,
    revision: u64,
    decision: people::EscalationDecision,
}

#[derive(Debug)]
enum CreationKind {
    Goal { person: u64, direct: bool },
    Chat { person: u64 },
}

/// One person task pending all numbered connector holdings in this decision.
#[derive(Debug)]
pub(crate) struct Creation {
    kind: CreationKind,
    new: tasks::New,
    remaining: u32,
    holdings: List<tasks::Holding>,
    answered: List<u16>,
    failed: bool,
}

/// One member's concrete identity passed to a connector for its own holdings.
#[derive(Clone, Debug)]
pub struct HoldingsNeed {
    pub project: u32,
    pub root: u64,
    pub number: u64,
    pub executor: tasks::Executor,
    pub spec: tasks::Spec,
}

#[derive(Debug)]
pub(crate) struct DelegateCreation {
    key: CallKey,
    kind: BatchKind,
    members: Box<[tasks::New]>,
    remaining: u32,
    answered: List<u16>,
    failed: bool,
}

#[derive(Debug)]
enum BatchKind {
    Delegation { stubs: Box<[tasks::Stub]> },
    Proposal { reason: Box<[u8]>, as_holder: bool },
}

#[derive(Debug)]
pub(crate) struct ProcedureCreation {
    members: Box<[tasks::New]>,
    remaining: u32,
    answered: List<u16>,
    failed: bool,
}

/// One child's complete bounded request batch, for the root to route before
/// accepting its journal decision.
#[derive(Debug)]
pub enum Requests {
    /// Fully routed core requests, each bearing its commit mark.
    Out(Queue<Request>),
}

/// Connector-free shape of a root-owned host payload after refusal.
#[derive(Debug)]
pub enum PayloadRefusal {
    /// One rejected host turn.
    Turn { task: u64, attempt: u64, turn: u32 },
    /// One rejected host answer.
    Answer { task: u64, attempt: u64 },
}

/// Connector-free work a named host call asks the core to route.
#[derive(Debug)]
pub enum NamedAction {
    Message {
        target: u64,
        kind: tasks::MessageKind,
        words: Box<[u8]>,
    },
    Introduce {
        left: u64,
        right: u64,
    },
    Subscribe {
        kind: tasks::SubscriptionKind,
    },
    Unsubscribe {
        subscription: u64,
    },
    Control {
        target: u64,
        control: tasks::Control,
    },
    Amend {
        target: u64,
        amendment: tasks::Amendment,
    },
    DecideEscalation {
        task: u64,
        revision: u64,
        choice: EscalationChoice,
    },
    WithdrawProposal {
        proposal: u64,
    },
    DecideProposal {
        proposer: u64,
        proposal: u64,
        choice: ProposalChoice,
    },
    Propose {
        action: tasks::ProposalAction,
        reason: Box<[u8]>,
        as_holder: bool,
    },
    ProposeBatch {
        batch: Box<[Delegate]>,
        reason: Box<[u8]>,
        as_holder: bool,
    },
    /// Save one scoped note under a revision seen by the run.
    Note {
        entry: notes::New,
        recalled: Option<u32>,
    },
    /// Recall one page of notes by name or description search.
    Recall {
        by: notes::Recall,
        page: u32,
    },
}

/// One engine-tool family and any condition the authority check needs.
#[derive(Debug)]
pub enum ToolKind {
    Delegate,
    Message { target: u64 },
    Control,
    Decide,
    Propose,
    Subscribe,
    Effect,
    Note { scope: notes::Scope },
    Recall { by: notes::Recall },
    Read,
}

impl Core {
    /// Check a named run call against its live task and policy before routing it.
    #[must_use]
    pub fn authorize_tool(&self, key: CallKey, kind: ToolKind) -> Option<CallPart> {
        let Some(task) = self.tasks.task(key.task) else {
            return Some(CallPart::ToolDenied {
                answer: authority::Answer::Refuse,
                findings: Box::new([authority::Finding::UnknownProject]),
            });
        };
        let family = match &kind {
            ToolKind::Delegate => self.settings.tools.delegate,
            ToolKind::Message { .. } => self.settings.tools.message,
            ToolKind::Control => self.settings.tools.control,
            ToolKind::Decide => self.settings.tools.decide,
            ToolKind::Propose => self.settings.tools.propose,
            ToolKind::Subscribe => self.settings.tools.subscribe,
            ToolKind::Effect => self.settings.tools.effect,
            ToolKind::Note { .. } => self.settings.tools.note,
            ToolKind::Recall { .. } => self.settings.tools.recall,
            ToolKind::Read => self.settings.tools.read,
        };
        let call = match kind {
            ToolKind::Message { target } => authority::Call::Message {
                referenced: task.requester == tasks::Party::Task(target)
                    || task.delegates.contains(&target)
                    || task.references.contains(&target),
            },
            ToolKind::Note { scope } => {
                let scope = match scope {
                    notes::Scope::Deployment => authority::NoteScope::Deployment,
                    notes::Scope::Project { project } if project == task.project => authority::NoteScope::Project,
                    notes::Scope::Goal { project, goal } if project == task.project && goal == task.root => {
                        authority::NoteScope::Goal
                    }
                    notes::Scope::Resources { project, connector, pattern } if project == task.project => {
                        let last = match pattern.last {
                            notes::Last::Exact(value) => authority::Last::Exact(value),
                            notes::Last::Open(value) => authority::Last::Open(value),
                        };
                        authority::NoteScope::Resources(authority::ResourceScope {
                            connector,
                            pattern: authority::Pattern { segments: pattern.segments, last },
                        })
                    }
                    notes::Scope::Project { .. } | notes::Scope::Goal { .. } | notes::Scope::Resources { .. } => {
                        return Some(CallPart::ToolDenied {
                            answer: authority::Answer::Refuse,
                            findings: Box::new([authority::Finding::Scope { source: authority::Source::Task }]),
                        });
                    }
                };
                authority::Call::Note(scope)
            }
            ToolKind::Recall { by } => {
                let readable = match by {
                    notes::Recall::Name { .. } => true,
                    notes::Recall::Search { scopes, .. } => {
                        let mut readable = true;
                        for scope in &scopes {
                            if !self.note_readable(key.task, scope) {
                                readable = false;
                            }
                        }
                        readable
                    }
                };
                if !readable {
                    return Some(CallPart::ToolDenied {
                        answer: authority::Answer::Refuse,
                        findings: Box::new([authority::Finding::Scope { source: authority::Source::Task }]),
                    });
                }
                authority::Call::Tool
            }
            ToolKind::Delegate
            | ToolKind::Control
            | ToolKind::Decide
            | ToolKind::Propose
            | ToolKind::Subscribe
            | ToolKind::Effect
            | ToolKind::Read => authority::Call::Tool,
        };
        let mut why = Queue::with_capacity(authority::max_out(self.authority.limits()).expect("bounded findings"));
        let answer = authority::check_call(
            &self.authority,
            &authority::CallAsk {
                project: task.project,
                authority: crate::translate::authority_value(&task.authority),
                family,
                call,
            },
            &mut why,
        );
        if answer == authority::Answer::Allow {
            return None;
        }
        let mut findings = List::with_capacity(why.capacity());
        for _ in 0..why.len() {
            findings.push(why.pop().expect("finding count")).expect("finding room");
        }
        Some(CallPart::ToolDenied { answer, findings: findings.into_boxed() })
    }

    /// A named recall may read only notes in this task's project and goal, or deployment notes.
    #[must_use]
    pub fn note_readable(&self, task: u64, scope: &notes::Scope) -> bool {
        let Some(row) = self.tasks.task(task) else { return false };
        match scope {
            notes::Scope::Deployment => true,
            notes::Scope::Project { project } | notes::Scope::Resources { project, .. } => *project == row.project,
            notes::Scope::Goal { project, goal } => *project == row.project && *goal == row.root,
        }
    }
}

/// The parent caller held while the notes child reads its store pages.
#[derive(Debug)]
pub(crate) enum NoteRoute {
    Call { to: ReplyTo, key: CallKey },
    Person { request: Token },
    Brief { task: u64 },
}

impl Core {
    fn begin_note_route(&mut self, route: NoteRoute) -> Token {
        let owner = Token::new(self.next_note_owner);
        self.next_note_owner = self.next_note_owner.checked_add(1).expect("transient note owner space");
        let previous = self.note_routes.insert(owner, route).expect("notes keeps one pending route plus a refusal");
        assert!(previous.is_none(), "transient note owners are unique");
        owner
    }
}

/// A task holder's choice on one pending proposal.
#[derive(Debug)]
pub enum ProposalChoice {
    Accept,
    Reject { reason: Box<[u8]> },
    Pass,
}

/// A task holder's choice on one waiting escalation.
#[derive(Debug)]
pub enum EscalationChoice {
    Release,
    Reject { reason: Box<[u8]> },
    Pass,
}

/// One due child timer. The caller reserves journal room before a timer whose
/// route may change durable state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Timer {
    /// Task deadlines and standing work.
    Tasks,
    /// Host, call and attempt deadlines.
    Fleet,
    /// Brief gathering deadline.
    Brief,
    /// Account refresh or retry.
    Account,
    /// Live view deadline.
    View,
}

#[expect(clippy::too_many_lines, reason = "one exhaustive task vocabulary routes its bounded child decision")]
fn tag_tasks(
    core: &mut Core,
    env: &Env<Limits>,
    mut child: Queue<tasks::Request>,
    room: u32,
    person_proposal: bool,
    task_escalation: bool,
    person_escalation: bool,
) -> Requests {
    let task_room = room.checked_mul(8).expect("bounded task routes");
    let party_room = people::max_out(&env.limits.people).checked_mul(4).expect("bounded party routes");
    let route_room = task_room.checked_add(party_room).expect("bounded task and party routes");
    let mut pending = Queue::with_capacity(route_room);
    for _ in 0..child.len() {
        pending.push(child.pop().expect("task output count"));
    }
    let mut out = Queue::with_capacity(route_room.checked_add(1).expect("task output room"));
    for _ in 0..route_room {
        let Some(next) = pending.pop() else { break };
        let request = match next {
            tasks::Request::Erase { key } => Request::Write(Write::Erase(Key::Tasks(key))),
            tasks::Request::Relay { task, attempt, previous, word } => {
                let previous = core.relay_previous(task, previous, &word.kind);
                Request::Held(Box::new(Held::Relay { task, attempt, previous, word }))
            }
            tasks::Request::PersonProposed { reply_to, proposal } => {
                let event = core.person_proposed(reply_to.into_token(), proposal);
                append_people(core, env, event, &mut out);
                continue;
            }
            tasks::Request::PersonProposalDecided { reply_to, proposer, number, outcome } => {
                let event = core.person_proposal_decided(reply_to.into_token(), proposer, number, outcome);
                append_people(core, env, event, &mut out);
                continue;
            }
            tasks::Request::EscalationDecided { reply_to, task, revision, outcome } if person_escalation => {
                let token = reply_to.into_token();
                let flight = core.person_escalations.remove(&token).expect("person held choice has a flight");
                assert!(flight.task == task && flight.revision == revision, "exact held choice terminal");
                let choice = match outcome {
                    tasks::EscalationOutcome::Released => people::EscalationChoice::Released,
                    tasks::EscalationOutcome::Rejected => people::EscalationChoice::Rejected,
                    tasks::EscalationOutcome::Passed { .. } => people::EscalationChoice::Passed,
                    tasks::EscalationOutcome::NoFurther => {
                        append_people(
                            core,
                            env,
                            people::Event::Decided {
                                request: flight.request,
                                outcome: people::Outcome::Refused(people::Refusal::NoFurther),
                            },
                            &mut out,
                        );
                        continue;
                    }
                    tasks::EscalationOutcome::Limit => {
                        append_people(
                            core,
                            env,
                            people::Event::Decided {
                                request: flight.request,
                                outcome: people::Outcome::Refused(people::Refusal::Limit),
                            },
                            &mut out,
                        );
                        continue;
                    }
                    tasks::EscalationOutcome::Stale => {
                        append_people(
                            core,
                            env,
                            people::Event::Decided {
                                request: flight.request,
                                outcome: people::Outcome::Refused(people::Refusal::Unknown),
                            },
                            &mut out,
                        );
                        continue;
                    }
                };
                out.push(Request::Write(Write::Save(Record::Core(CoreRecord::EscalationDecision(
                    EscalationDecisionRecord {
                        project: flight.project,
                        requester: flight.requester,
                        task,
                        revision,
                        by: flight.person,
                        decision: flight.decision,
                    },
                )))));
                append_people(
                    core,
                    env,
                    people::Event::Decided {
                        request: flight.request,
                        outcome: people::Outcome::EscalationDecided { task, revision, by: flight.person, choice },
                    },
                    &mut out,
                );
                continue;
            }
            tasks::Request::Sent { reply_to, task, word } => {
                let request = reply_to.into_token();
                match core.sent(request, task, &word, env.limits.tasks.inbox_messages, env.limits.tasks.tasks) {
                    SentRoute::Call { key, message } => {
                        call_decision(core, &mut out, ReplyTo::new(request), key, CallPart::Sent { message });
                    }
                    SentRoute::Person(event) => append_people(core, env, *event, &mut out),
                }
                continue;
            }
            tasks::Request::RecurringDue { task, period, members } => {
                let Some(context) = core.tasks.delegation(task) else { continue };
                let Some(template) = core.tasks.recurring_template(task) else { continue };
                if members != u32::try_from(template.batch.len()).expect("bounded template") {
                    continue;
                }
                let mut asked = List::with_capacity(env.limits.tasks.batch);
                for member in &template.batch {
                    let executor = match member.executor {
                        tasks::Executor::Agent { charter } => authority::Executor::Charter(charter),
                        tasks::Executor::Procedure { code, .. } => authority::Executor::Procedure(code),
                        tasks::Executor::Person(_) => unreachable!("person template refused at admission"),
                    };
                    asked
                        .push(authority::Delegate {
                            executor,
                            authority: crate::translate::authority_value(&member.authority),
                            symbolic: Box::new([]),
                        })
                        .expect("bounded template");
                }
                let checked = core.connector_batch_admit(
                    context.project,
                    &context.authority,
                    tasks::Numbers { budget: context.authority.budget.spend, spent: 0, spent_below: 0, reserved: 0 },
                    context.tasks_left,
                    asked.into_boxed(),
                    &mut Queue::with_capacity(
                        authority::max_out(core.authority.limits()).expect("bounded authority findings"),
                    ),
                );
                if checked.answer != authority::Answer::Allow {
                    continue;
                }
                let mut numbers = List::with_capacity(env.limits.tasks.batch);
                for _ in 0..members {
                    let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else { break };
                    numbers.push(number).expect("bounded recurring batch");
                }
                if numbers.len() == members {
                    let mut follow = Queue::with_capacity(room);
                    tasks::step(
                        &mut core.tasks,
                        &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                        tasks::Event::RecurringBatch { task, period, numbers: numbers.into_boxed() },
                        &mut follow,
                    );
                    for _ in 0..follow.len() {
                        pending.push(follow.pop().expect("recurring output count"));
                    }
                }
                continue;
            }
            tasks::Request::Adopt { task, attempt, kept } => {
                core.adopt(env, task, attempt, kept);
                continue;
            }
            tasks::Request::Stop { task, attempt } => Request::Held(Box::new(Held::StopRun { task, attempt })),
            tasks::Request::RestoreRefused { .. } => Request::Now(Box::new(Now::RestoreRefused)),
            tasks::Request::Waiting { .. }
            | tasks::Request::WriterWaiting { task: None, .. }
            | tasks::Request::EscalationsInspected { .. }
            | tasks::Request::EscalationsRechecked { .. } => continue,
            tasks::Request::Taken { task, holdings } => {
                for holding in holdings {
                    match holding {
                        tasks::Holding::Write { resource, .. } => {
                            out.push(Request::Ask { connector: resource.connector, ask: Ask::Hold { task, resource } });
                        }
                        tasks::Holding::Slot { .. } => {}
                    }
                }
                continue;
            }
            tasks::Request::EndTopic { task, subscription, connector } => {
                Request::Ask { connector, ask: Ask::EndTopic { task, subscription } }
            }
            tasks::Request::WriterWaiting { task: Some(task), .. } => {
                if core.claiming.remove(&task).is_some() {
                    drop(core.proofs.remove(&task));
                    out.push(Request::Now(Box::new(Now::DropAssignment { task })));
                    let mut follow = Queue::with_capacity(room);
                    tasks::step(
                        &mut core.tasks,
                        &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                        tasks::Event::PreparationFailed { task },
                        &mut follow,
                    );
                    for _ in 0..follow.len() {
                        pending.push(follow.pop().expect("writer wait output count"));
                    }
                }
                continue;
            }
            tasks::Request::EscalationNeeded { context } => {
                let role = core.people.role(context.requester, context.project);
                let Some(holder) = core.escalation_recipient(&env.limits, &context, role) else {
                    out.push(Request::Now(Box::new(Now::RestoreRefused)));
                    continue;
                };
                match context.escalation {
                    tasks::Escalation::Waiting { holder: old, .. } if old == holder => continue,
                    tasks::Escalation::Waiting { revision, holder: old, .. }
                        if old != holder && revision == u64::MAX =>
                    {
                        out.push(Request::Now(Box::new(Now::RestoreRefused)));
                        continue;
                    }
                    tasks::Escalation::Unheld { .. }
                    | tasks::Escalation::Routing { .. }
                    | tasks::Escalation::Waiting { .. }
                    | tasks::Escalation::Rejected { .. } => {}
                }
                let Some(entry) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                    out.push(Request::Now(Box::new(Now::RestoreRefused)));
                    continue;
                };
                let mut follow = Queue::with_capacity(room);
                tasks::step(
                    &mut core.tasks,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                    tasks::Event::RoutedEscalation {
                        task: context.task,
                        revision: context.escalation.revision(),
                        holder,
                        entry,
                    },
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("escalation output count"));
                }
                continue;
            }
            tasks::Request::EscalationStalled { task, revision, holder } => {
                let Some(context) = core.tasks.escalation(task) else { continue };
                let tasks::Escalation::Waiting { revision: current, holder: old, .. } = context.escalation else {
                    continue;
                };
                if current != revision || old != holder {
                    continue;
                }
                let role = core.people.role(context.requester, context.project);
                let Some(next) = core.escalation_recipient_after(&env.limits, &context, role, Some(holder)) else {
                    continue;
                };
                let Some(entry) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                    out.push(Request::Now(Box::new(Now::RestoreRefused)));
                    continue;
                };
                let mut follow = Queue::with_capacity(room);
                tasks::step(
                    &mut core.tasks,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                    tasks::Event::RoutedEscalation { task, revision, holder: next, entry },
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("stalled escalation output count"));
                }
                continue;
            }
            tasks::Request::ProposalStalled { proposer, proposal, holder } => {
                let Some(row) = core.tasks.proposal(proposer, proposal) else { continue };
                let tasks::ProposalState::Pending { holder: current, .. } = row.state else { continue };
                if current != holder {
                    continue;
                }
                let Some(next) = core.proposal_holder(&env.limits, proposer, &row.action, Some(holder)) else {
                    continue;
                };
                let revision = core.tasks.task(proposer).expect("pending proposer remains live").revision;
                let mut follow = Queue::with_capacity(room);
                tasks::step(
                    &mut core.tasks,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                    tasks::Event::StalledProposal { proposer, proposal, from: holder, revision, holder: next },
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("stalled proposal output count"));
                }
                continue;
            }
            tasks::Request::ProposalRerouteNeeded { proposer, proposal } => {
                let Some(row) = core.tasks.proposal(proposer, proposal) else { continue };
                let tasks::ProposalState::Pending { holder: current, .. } = row.state else { continue };
                let Some(next) = core.proposal_holder(&env.limits, proposer, &row.action, None) else {
                    continue;
                };
                if current == next {
                    continue;
                }
                let revision = core.tasks.task(proposer).expect("pending proposer remains live").revision;
                let mut follow = Queue::with_capacity(room);
                tasks::step(
                    &mut core.tasks,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                    tasks::Event::StalledProposal { proposer, proposal, from: current, revision, holder: next },
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("rerouted proposal output count"));
                }
                continue;
            }
            tasks::Request::Made { reply_to, tasks } => {
                let request = reply_to.into_token();
                match core.made(request, tasks, person_proposal) {
                    crate::MadeRoute::Internal => {}
                    crate::MadeRoute::Tasks(event) | crate::MadeRoute::PersonProposal(event) => {
                        let mut follow = Queue::with_capacity(room);
                        tasks::step(
                            &mut core.tasks,
                            &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                            event,
                            &mut follow,
                        );
                        for _ in 0..follow.len() {
                            pending.push(follow.pop().expect("made output count"));
                        }
                    }
                    crate::MadeRoute::Person(event) => append_people(core, env, *event, &mut out),
                    crate::MadeRoute::Delegated { key, tasks, stubs } => {
                        for stub in stubs {
                            let mut follow = Queue::with_capacity(room);
                            tasks::step(
                                &mut core.tasks,
                                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                                tasks::Event::RememberStub { stub },
                                &mut follow,
                            );
                            for _ in 0..follow.len() {
                                pending.push(follow.pop().expect("stub output count"));
                            }
                        }
                        call_decision(core, &mut out, ReplyTo::new(request), key, CallPart::Delegated(tasks));
                    }
                }
                continue;
            }
            tasks::Request::ProposalDecided { reply_to, proposer, number, outcome } => {
                let token = reply_to.into_token();
                if let Some(event) =
                    core.task_proposal_decided_for_person(token, proposer, number, outcome, person_proposal)
                {
                    append_people(core, env, event, &mut out);
                    continue;
                }
                let route = core.routing_calls.remove(&token).expect("pending proposal decision route");
                let key = match route {
                    RoutedCall::Decide { key, proposal } | RoutedCall::Withdraw { key, proposal }
                        if proposal == number =>
                    {
                        key
                    }
                    RoutedCall::Propose { .. }
                    | RoutedCall::Accepting { .. }
                    | RoutedCall::Decide { .. }
                    | RoutedCall::Escalation { .. }
                    | RoutedCall::Withdraw { .. }
                    | RoutedCall::Message(_)
                    | RoutedCall::Introduce(_)
                    | RoutedCall::Subscribe { .. }
                    | RoutedCall::Unsubscribe(_)
                    | RoutedCall::Control(_) => unreachable!("matching proposal decision"),
                };
                assert!(key.task == proposer || outcome != tasks::ProposalOutcome::Withdrawn, "withdrawal by proposer");
                call_decision(
                    core,
                    &mut out,
                    ReplyTo::new(token),
                    key,
                    CallPart::ProposalDecided { proposal: number, outcome },
                );
                continue;
            }
            tasks::Request::EscalationDecided { reply_to, task, revision, outcome } if task_escalation => {
                let token = reply_to.into_token();
                let route = core.routing_calls.remove(&token).expect("task escalation decision route");
                let (key, named, current) = match route {
                    RoutedCall::Escalation { key, task, revision } => (key, task, revision),
                    RoutedCall::Propose { .. }
                    | RoutedCall::Decide { .. }
                    | RoutedCall::Withdraw { .. }
                    | RoutedCall::Accepting { .. }
                    | RoutedCall::Message(_)
                    | RoutedCall::Introduce(_)
                    | RoutedCall::Subscribe { .. }
                    | RoutedCall::Unsubscribe(_)
                    | RoutedCall::Control(_) => unreachable!("task escalation route kind"),
                };
                assert!(task == named && revision == current, "exact task escalation terminal");
                call_decision(
                    core,
                    &mut out,
                    ReplyTo::new(token),
                    key,
                    CallPart::EscalationDecided { task, revision, outcome },
                );
                continue;
            }
            tasks::Request::Done { reply_to } => {
                let token = reply_to.into_token();
                if let Some(route) = core.person_tasks.remove(&token) {
                    let outcome = match route {
                        PersonTaskRoute::Take(task) => people::Outcome::PersonTaken { task },
                        PersonTaskRoute::PoolSet { project, person } => people::Outcome::PoolSet { project, person },
                        PersonTaskRoute::HandBack(task) => people::Outcome::PersonHandedBack { task },
                        PersonTaskRoute::Answer(task) => people::Outcome::PersonAnswered { task },
                        PersonTaskRoute::Cancel(task) => people::Outcome::Cancelled { task },
                        PersonTaskRoute::Release(task) => people::Outcome::Released { task },
                        PersonTaskRoute::Prioritised(project) => people::Outcome::Prioritised { project },
                        PersonTaskRoute::Amended(task) => people::Outcome::Amended { task },
                        PersonTaskRoute::AmendProposed { task, proposal } => {
                            people::Outcome::AmendProposed { task, proposal }
                        }
                    };
                    append_people(core, env, people::Event::Decided { request: token, outcome }, &mut out);
                    continue;
                }
                if let Some(moved) = core.moving.remove(&token) {
                    append_people(
                        core,
                        env,
                        people::Event::Decided { request: token, outcome: people::Outcome::Moved { task: moved } },
                        &mut out,
                    );
                    continue;
                }
                let person_route = if person_proposal { core.routing_people_proposals.remove(&token) } else { None };
                if let Some(PersonProposalRoute::Accepting { request, person, proposer, proposal, message }) =
                    person_route
                {
                    core.routing_people_proposals
                        .insert(request, PersonProposalRoute::Deciding { request, proposer, proposal, by: person })
                        .expect("person acceptance route room");
                    let mut follow = Queue::with_capacity(room);
                    tasks::step(
                        &mut core.tasks,
                        &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                        tasks::Event::DecideProposal {
                            reply_to: ReplyTo::new(request),
                            proposer,
                            proposal,
                            message: Some(message),
                            by: tasks::Party::Person(person),
                            decision: tasks::ProposalDecision::Accept,
                        },
                        &mut follow,
                    );
                    for _ in 0..follow.len() {
                        pending.push(follow.pop().expect("person acceptance output count"));
                    }
                    continue;
                }
                if let Some(route) = core.routing_calls.get(&token).copied() {
                    let answer = match route {
                        RoutedCall::Propose { key, proposal } => {
                            if let Some((connector, owner)) = core.proposing_effects.remove(&token) {
                                out.push(Request::Ask {
                                    connector,
                                    ask: Ask::Effect(crate::connector::Ask::KeepProposal {
                                        owner,
                                        proposal,
                                        task: key.task,
                                    }),
                                });
                            }
                            Some((key, CallPart::Proposed { proposal }))
                        }
                        RoutedCall::Introduce(key) => Some((key, CallPart::Introduced)),
                        RoutedCall::Control(key) => Some((key, CallPart::Controlled)),
                        RoutedCall::Accepting { key, proposer, proposal, message } => {
                            let _previous = core.routing_calls.remove(&token).expect("accepted proposal route");
                            core.routing_calls
                                .insert(token, RoutedCall::Decide { key, proposal })
                                .expect("acceptance route room");
                            let mut follow = Queue::with_capacity(room);
                            tasks::step(
                                &mut core.tasks,
                                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                                tasks::Event::DecideProposal {
                                    reply_to: ReplyTo::new(token),
                                    proposer,
                                    proposal,
                                    message: Some(message),
                                    by: tasks::Party::Task(key.task),
                                    decision: tasks::ProposalDecision::Accept,
                                },
                                &mut follow,
                            );
                            for _ in 0..follow.len() {
                                pending.push(follow.pop().expect("task acceptance output count"));
                            }
                            continue;
                        }
                        RoutedCall::Subscribe { .. } | RoutedCall::Unsubscribe(_) => None,
                        RoutedCall::Message(_)
                        | RoutedCall::Decide { .. }
                        | RoutedCall::Withdraw { .. }
                        | RoutedCall::Escalation { .. } => unreachable!("these calls have another terminal"),
                    };
                    if let Some((key, part)) = answer {
                        let _previous = core.routing_calls.remove(&token).expect("named call route");
                        call_decision(core, &mut out, ReplyTo::new(token), key, part);
                        continue;
                    }
                }
                if let Some(route) = core.routing_calls.remove(&token) {
                    let ask = match route {
                        RoutedCall::Subscribe { key, subscription } => {
                            Ask::SubscriptionDone { request: token, key, subscription }
                        }
                        RoutedCall::Unsubscribe(key) => Ask::UnsubscriptionDone { request: token, key },
                        RoutedCall::Propose { .. }
                        | RoutedCall::Accepting { .. }
                        | RoutedCall::Introduce(_)
                        | RoutedCall::Control(_)
                        | RoutedCall::Message(_)
                        | RoutedCall::Decide { .. }
                        | RoutedCall::Withdraw { .. }
                        | RoutedCall::Escalation { .. } => unreachable!("generic named calls finish in the core"),
                    };
                    for &connector in core.connectors.as_ref() {
                        out.push(Request::Ask {
                            connector,
                            ask: match &ask {
                                Ask::SubscriptionDone { request, key, subscription } => {
                                    Ask::SubscriptionDone { request: *request, key: *key, subscription: *subscription }
                                }
                                Ask::UnsubscriptionDone { request, key } => {
                                    Ask::UnsubscriptionDone { request: *request, key: *key }
                                }
                                Ask::Hold { .. }
                                | Ask::EndTopic { .. }
                                | Ask::Gather { .. }
                                | Ask::CutTo { .. }
                                | Ask::Drop { .. }
                                | Ask::Adopt { .. }
                                | Ask::Close { .. }
                                | Ask::Release { .. }
                                | Ask::Lost { .. }
                                | Ask::TaskHoldings { .. }
                                | Ask::DelegateHoldings { .. }
                                | Ask::ProcedureHoldings { .. }
                                | Ask::ProjectGoal { .. }
                                | Ask::ClaimDone { .. }
                                | Ask::DropSubscription { .. }
                                | Ask::RepairRefused { .. }
                                | Ask::DelegateRefused { .. }
                                | Ask::StartProcedure { .. }
                                | Ask::Effect(_) => unreachable!("subscription handoff"),
                            },
                        });
                    }
                    continue;
                }
                if let Some(attempt) = core.claiming.remove(&token.raw()) {
                    let proof = core.proofs.get(&token.raw()).expect("claim proof pre-reserved");
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::RunProof(proof.clone())))));
                    for &connector in core.connectors.as_ref() {
                        out.push(Request::Ask { connector, ask: Ask::ClaimDone { task: token.raw(), attempt } });
                    }
                }
                continue;
            }
            tasks::Request::Save { record } => {
                if let Some(archive) = crate::proposals::decision_record(&record) {
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::ProposalDecision(archive)))));
                }
                let record = match record {
                    tasks::Stored::Ended(mut task) => {
                        let position = crate::fresh(&mut core.counters, crate::Family::Message)
                            .expect("ending position preflighted before mutation");
                        task.result_position = position;
                        assert!(core.ending_positions.insert(task.number, position) == Ok(None), "one ending position");
                        match task.requester {
                            tasks::Party::Person(person) => {
                                core.people.remember_result(
                                    &env.limits.people,
                                    person,
                                    people::ResultRef { task: task.number, position },
                                );
                            }
                            tasks::Party::Task(_) | tasks::Party::Deployment { .. } => {}
                        }
                        tasks::Stored::Ended(task)
                    }
                    tasks::Stored::History(ref row) => {
                        if let Some(proposal) = &row.proposal {
                            let terminal = match proposal.state {
                                tasks::ProposalState::Pending { .. } => false,
                                tasks::ProposalState::Accepted { .. }
                                | tasks::ProposalState::Rejected { .. }
                                | tasks::ProposalState::Withdrawn => true,
                            };
                            if terminal {
                                match proposal.action {
                                    tasks::ProposalAction::Effect { connector, .. } => out.push(Request::Ask {
                                        connector,
                                        ask: Ask::Effect(crate::connector::Ask::DropProposal {
                                            proposal: proposal.number,
                                        }),
                                    }),
                                    tasks::ProposalAction::Batch(_)
                                    | tasks::ProposalAction::Amend { .. }
                                    | tasks::ProposalAction::Widen { .. }
                                    | tasks::ProposalAction::Release { .. } => {}
                                }
                            }
                        }
                        record
                    }
                    tasks::Stored::Live(_)
                    | tasks::Stored::Writer(_)
                    | tasks::Stored::Pool(_)
                    | tasks::Stored::Ledger(_)
                    | tasks::Stored::Stub(_)
                    | tasks::Stored::PersonProposal(_) => record,
                };
                if let tasks::Stored::Live(task) | tasks::Stored::Ended(task) = &record {
                    let stale = match core.contexts.get(&task.number) {
                        Some(context) => {
                            task.phase != tasks::Phase::Active(tasks::Active::Preparing)
                                || task.last_message != context.last_message
                        }
                        None => false,
                    };
                    if stale {
                        drop(core.contexts.remove(&task.number));
                        drop(core.dependency_results.remove(&task.number));
                        drop(core.transcripts.remove(&task.number));
                        let mut abandoned = Queue::with_capacity(brief::gather_max_out(&env.limits.brief));
                        brief::gather_step(
                            &mut core.brief,
                            &Env { now: env.now, wall: env.wall, limits: env.limits.brief },
                            brief::GatherEvent::Abandon { brief: Token::new(task.number) },
                            &mut abandoned,
                        );
                        for _ in 0..abandoned.len() {
                            match abandoned.pop().expect("abandoned brief output count") {
                                brief::GatherRequest::Drop { connector, section } => {
                                    out.push(Request::Ask { connector, ask: Ask::Drop { section } });
                                }
                                brief::GatherRequest::Gather { .. }
                                | brief::GatherRequest::CutTo { .. }
                                | brief::GatherRequest::Complete { .. }
                                | brief::GatherRequest::Failed { .. }
                                | brief::GatherRequest::Refused { .. } => {
                                    unreachable!("abandon only releases connector sections")
                                }
                            }
                        }
                        if task.phase == tasks::Phase::Active(tasks::Active::Preparing) {
                            let mut follow = Queue::with_capacity(room);
                            tasks::step(
                                &mut core.tasks,
                                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                                tasks::Event::PreparationFailed { task: task.number },
                                &mut follow,
                            );
                            for _ in 0..follow.len() {
                                pending.push(follow.pop().expect("stale preparation output count"));
                            }
                        }
                    }
                    task_saved_view(core, env, task, &mut out);
                }
                let waiting = match &record {
                    tasks::Stored::Live(task) | tasks::Stored::Ended(task) => {
                        Some((task.number, core.waiting_entries(&env.limits, task)))
                    }
                    tasks::Stored::PersonProposal(row) => Some((row.goal.number, core.person_proposal_entries(row))),
                    tasks::Stored::Ledger(_)
                    | tasks::Stored::Writer(_)
                    | tasks::Stored::Pool(_)
                    | tasks::Stored::History(_)
                    | tasks::Stored::Stub(_) => None,
                };
                let projection = match &record {
                    tasks::Stored::Live(task) | tasks::Stored::Ended(task) if task.tracked.is_some() => {
                        Some(task.clone())
                    }
                    tasks::Stored::Live(_)
                    | tasks::Stored::Ended(_)
                    | tasks::Stored::PersonProposal(_)
                    | tasks::Stored::Ledger(_)
                    | tasks::Stored::Writer(_)
                    | tasks::Stored::Pool(_)
                    | tasks::Stored::History(_)
                    | tasks::Stored::Stub(_) => None,
                };
                out.push(Request::Write(Write::Save(Record::Tasks(record))));
                if let Some(goal) = projection {
                    for &connector in core.connectors.as_ref() {
                        out.push(Request::Ask { connector, ask: Ask::ProjectGoal { goal: goal.clone() } });
                    }
                }
                if let Some((task, entries)) = waiting {
                    append_people(core, env, people::Event::Waiting { task, entries }, &mut out);
                }
                continue;
            }
            tasks::Request::Acknowledged { reply_to, task, attempt, .. } => {
                let token = reply_to.into_token();
                if token.raw() != u64::MAX {
                    out.push(Request::Now(Box::new(Now::DropPayload { payload: token })));
                }
                if let Some(proof) = core.proofs.get(&task) {
                    if let Some(terminal) = &proof.terminal {
                        out.push(Request::Write(Write::Save(Record::Core(CoreRecord::Terminal(terminal.clone())))));
                    }
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::RunProof(proof.clone())))));
                }
                out.push(Request::Held(Box::new(Held::TaskTerminalAcknowledged { task, attempt })));
                for &connector in core.connectors.as_ref() {
                    out.push(Request::Ask { connector, ask: Ask::Lost { task, attempt } });
                }
                continue;
            }
            tasks::Request::TurnAcknowledged { reply_to, task, attempt, turn, accepted } => {
                out.push(Request::Now(Box::new(Now::AcceptedTurn {
                    payload: reply_to.into_token(),
                    task,
                    attempt,
                    turn,
                    accepted,
                })));
                continue;
            }
            tasks::Request::Ended { task, requester, ending } => {
                let position = core.ending_positions.remove(&task).expect("ended row assigned its result position");
                out.push(Request::Held(Box::new(Held::ViewFinished { task })));
                for key in core.retire_calls(task, u64::MAX, u32::MAX, env.limits.call_records) {
                    out.push(Request::Write(Write::Erase(Key::Core(CoreKey::Call(key)))));
                }
                drop(core.proofs.remove(&task));
                out.push(Request::Write(Write::Erase(Key::Core(CoreKey::RunProof(task)))));
                match requester {
                    tasks::Party::Person(person) => {
                        out.push(Request::Held(Box::new(Held::Result { person, task, words: ending_words(ending) })));
                    }
                    tasks::Party::Task(parent) => {
                        let (kind, words) = result_notice(ending);
                        let mut follow = Queue::with_capacity(room);
                        tasks::step(
                            &mut core.tasks,
                            &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                            tasks::Event::DelegateResult {
                                task: parent,
                                word: tasks::Word {
                                    number: position,
                                    from: tasks::Party::Task(task),
                                    kind: tasks::MessageKind::Result(kind),
                                    words,
                                    at: env.wall,
                                    hits: 1,
                                    eligible: false,
                                },
                            },
                            &mut follow,
                        );
                        for _ in 0..follow.len() {
                            pending.push(follow.pop().expect("delegate result output count"));
                        }
                    }
                    tasks::Party::Deployment { .. } => {}
                }
                continue;
            }
            tasks::Request::Refused { reply_to, problem } => {
                let token = reply_to.into_token();
                if let Some((connector, owner)) = core.proposing_effects.remove(&token) {
                    out.push(Request::Ask {
                        connector,
                        ask: Ask::Effect(crate::connector::Ask::Drop { owner, answer: authority::Answer::Refuse }),
                    });
                }
                if let Some(event) = core.goal_refused(token, problem.why) {
                    append_people(core, env, event, &mut out);
                    continue;
                }
                if core.person_tasks.remove(&token).is_some() {
                    append_people(
                        core,
                        env,
                        people::Event::Decided {
                            request: token,
                            outcome: people::Outcome::Refused(person_task_refusal(problem.why)),
                        },
                        &mut out,
                    );
                    continue;
                }
                if core.moving.remove(&token).is_some() {
                    append_people(
                        core,
                        env,
                        people::Event::Decided {
                            request: token,
                            outcome: people::Outcome::Refused(person_move_refusal(problem.why)),
                        },
                        &mut out,
                    );
                    continue;
                }
                let person_route = if person_proposal { core.routing_people_proposals.remove(&token) } else { None };
                if let Some(route) = person_route {
                    let request = match route {
                        PersonProposalRoute::Deciding { request, .. }
                        | PersonProposalRoute::Accepting { request, .. } => request,
                    };
                    append_people(
                        core,
                        env,
                        people::Event::Decided {
                            request,
                            outcome: people::Outcome::Refused(person_move_refusal(problem.why)),
                        },
                        &mut out,
                    );
                    continue;
                }
                if let Some(route) = core.routing_calls.get(&token).copied() {
                    let answer = match route {
                        RoutedCall::Message(key) | RoutedCall::Introduce(key) => {
                            Some((key, CallPart::MessageRefused(problem.clone())))
                        }
                        RoutedCall::Control(key) => Some((key, CallPart::ControlRefused(problem.clone()))),
                        RoutedCall::Escalation { key, .. } => Some((key, CallPart::EscalationRefused(problem.clone()))),
                        RoutedCall::Propose { key, .. }
                        | RoutedCall::Decide { key, .. }
                        | RoutedCall::Withdraw { key, .. }
                        | RoutedCall::Accepting { key, .. } => Some((key, CallPart::ProposalRefused(problem.clone()))),
                        RoutedCall::Subscribe { key, .. } | RoutedCall::Unsubscribe(key) => {
                            let _previous = core.routing_calls.remove(&token).expect("refused connector call route");
                            call_decision(
                                core,
                                &mut out,
                                ReplyTo::new(token),
                                key,
                                CallPart::SubscriptionRefused(problem.clone()),
                            );
                            for &connector in core.connectors.as_ref() {
                                out.push(Request::Ask { connector, ask: Ask::DropSubscription { request: token } });
                            }
                            continue;
                        }
                    };
                    if let Some((key, part)) = answer {
                        let _previous = core.routing_calls.remove(&token).expect("refused call route");
                        call_decision(core, &mut out, ReplyTo::new(token), key, part);
                        continue;
                    }
                }
                if let Some((key, _stubs)) = core.delegating.remove(&token) {
                    call_decision(core, &mut out, ReplyTo::new(token), key, CallPart::DelegationRefused(problem));
                    continue;
                }
                if core.made.remove(&token).is_some() {
                    append_people(
                        core,
                        env,
                        people::Event::Decided {
                            request: token,
                            outcome: people::Outcome::Refused(people::Refusal::Limit),
                        },
                        &mut out,
                    );
                    continue;
                }
                if core.saying.remove(&token).is_some() {
                    append_people(
                        core,
                        env,
                        people::Event::Decided {
                            request: token,
                            outcome: people::Outcome::Refused(person_message_refusal(problem.why)),
                        },
                        &mut out,
                    );
                    continue;
                }
                if core.claiming.remove(&token.raw()).is_some() {
                    drop(core.proofs.remove(&token.raw()));
                    out.push(Request::Now(Box::new(Now::DropAssignment { task: token.raw() })));
                    let mut follow = Queue::with_capacity(room);
                    tasks::step(
                        &mut core.tasks,
                        &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                        tasks::Event::PreparationFailed { task: token.raw() },
                        &mut follow,
                    );
                    for _ in 0..follow.len() {
                        pending.push(follow.pop().expect("failed preparation output count"));
                    }
                    continue;
                }
                if token.raw() == u64::MAX - 3 {
                    for &connector in core.connectors.as_ref() {
                        out.push(Request::Ask { connector, ask: Ask::RepairRefused { repair: problem.task } });
                    }
                    continue;
                }
                if token.raw() == u64::MAX {
                    for &connector in core.connectors.as_ref() {
                        out.push(Request::Ask { connector, ask: Ask::DelegateRefused { task: problem.task } });
                    }
                    continue;
                }
                Request::Now(Box::new(Now::RefusedPayload { request: token, problem }))
            }
            tasks::Request::Notify { task, subscription, target, state, words } => {
                let event = core.notice(task, subscription, target, state, words, env.wall);
                let mut follow = Queue::with_capacity(room);
                tasks::step(
                    &mut core.tasks,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                    event,
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("notice output count"));
                }
                continue;
            }
            tasks::Request::Timer { task, subscription } => {
                let event = core.notice_timer(task, subscription, env.wall);
                let mut follow = Queue::with_capacity(room);
                tasks::step(
                    &mut core.tasks,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                    event,
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("timer output count"));
                }
                continue;
            }
            tasks::Request::Close { task, ending } => {
                let ancestor_number = core.tasks.root(task).unwrap_or(task);
                for &connector in core.connectors.as_ref() {
                    core.closing_connectors.insert((task, connector), ()).expect("bounded closing connectors");
                    out.push(Request::Ask {
                        connector,
                        ask: Ask::Close { task, root: ancestor_number, ending: ending.clone() },
                    });
                }
                continue;
            }
            tasks::Request::Release { task, ending } => {
                let ancestor_number = core.tasks.root(task).unwrap_or(task);
                let mut entries =
                    List::with_capacity(u32::try_from(core.connectors.len()).expect("connector count fits u32"));
                for &connector in core.connectors.as_ref() {
                    let Some(entry) = crate::fresh(&mut core.counters, crate::Family::ConnectorRow) else {
                        let mut follow = Queue::with_capacity(room);
                        tasks::step(
                            &mut core.tasks,
                            &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                            tasks::Event::Hold { task, why: tasks::Hold::Budget },
                            &mut follow,
                        );
                        for _ in 0..follow.len() {
                            pending.push(follow.pop().expect("release budget output count"));
                        }
                        break;
                    };
                    entries.push((connector, entry)).expect("one outbox entry per connector");
                }
                if entries.len() != u32::try_from(core.connectors.len()).expect("connector count fits u32") {
                    continue;
                }
                for &(connector, entry) in &entries {
                    out.push(Request::Ask {
                        connector,
                        ask: Ask::Release { task, root: ancestor_number, ending: ending.clone(), entry },
                    });
                }
                continue;
            }
            tasks::Request::EscalationInspected { reply_to, context } => {
                Request::Now(Box::new(Now::EscalationInspection { waiter: reply_to.into_token(), context }))
            }
            tasks::Request::EscalationDecided { .. } => {
                unreachable!("named and person escalation terminals are routed in the core")
            }
            tasks::Request::Activate { context } => Request::Now(Box::new(Now::Activate { context })),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn person_task_refusal(why: tasks::Refusal) -> people::Refusal {
    match why {
        tasks::Refusal::Busy | tasks::Refusal::NotReady => people::Refusal::Busy,
        tasks::Refusal::Unknown => people::Refusal::Ended,
        tasks::Refusal::State | tasks::Refusal::Executor | tasks::Refusal::Reference => people::Refusal::Standing,
        tasks::Refusal::Funding | tasks::Refusal::AuthorityShape => people::Refusal::Authority,
        tasks::Refusal::Duplicate
        | tasks::Refusal::Empty
        | tasks::Refusal::Batch
        | tasks::Refusal::Live
        | tasks::Refusal::Project
        | tasks::Refusal::Tree
        | tasks::Refusal::Depth
        | tasks::Refusal::Delegates
        | tasks::Refusal::Subscription
        | tasks::Refusal::Dependencies
        | tasks::Refusal::Cycle
        | tasks::Refusal::Spec
        | tasks::Refusal::Contract
        | tasks::Refusal::Inputs
        | tasks::Refusal::Attempt
        | tasks::Refusal::LiveDelegates
        | tasks::Refusal::Restore
        | tasks::Refusal::Read
        | tasks::Refusal::Turn
        | tasks::Refusal::HoldKind
        | tasks::Refusal::HoldTaken
        | tasks::Refusal::Holds => people::Refusal::Limit,
    }
}

fn pool_refused(work: &mut Queue<Event>, request: Token, why: people::Refusal) {
    work.push(Event::People(people::Event::Decided { request, outcome: people::Outcome::Refused(why) }));
}

#[expect(clippy::too_many_arguments, reason = "a keyed pool edit carries its owner, beneficiary and budget")]
fn pool_route(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    owner: u64,
    project: u32,
    person: u64,
    budget: u64,
) {
    if let Err(why) = core.role_allowed(owner, project, env.limits.tasks.tree_tasks) {
        pool_refused(work, request, why);
        return;
    }
    if core.person_tasks.len() >= env.limits.tasks.tasks || work.room() < 2 {
        pool_refused(work, request, people::Refusal::Busy);
        return;
    }
    let Some(role) = core.people.role(person, project) else {
        pool_refused(work, request, people::Refusal::Unknown);
        return;
    };
    let Some(allowed) = core.authority.role(project, role.number()) else {
        pool_refused(work, request, people::Refusal::Unknown);
        return;
    };
    if budget > allowed.period_spend {
        pool_refused(work, request, people::Refusal::Authority);
        return;
    }
    let Some(policy) = core.authority.policy(project) else {
        pool_refused(work, request, people::Refusal::Unknown);
        return;
    };
    if core.settings.period_budget > policy.period_spend {
        pool_refused(work, request, people::Refusal::Authority);
        return;
    }
    let period = tasks::Funder::Period { project, period: core.settings.period };
    let funder = tasks::Funder::Pool { project, person, period: core.settings.period };
    let needed =
        u32::from(core.tasks.funding(period).is_none()).saturating_add(u32::from(core.tasks.funding(funder).is_none()));
    if core.tasks.funding_room() < needed {
        pool_refused(work, request, people::Refusal::Busy);
        return;
    }
    if core.tasks.funding(period).is_none() && budget > core.settings.period_budget {
        pool_refused(work, request, people::Refusal::Authority);
        return;
    }
    if core.tasks.funding(period).is_none() {
        work.push(Event::Tasks(tasks::Event::OpenPeriod {
            reply_to: ReplyTo::new(Token::new(0)),
            project,
            period: core.settings.period,
            budget: core.settings.period_budget,
        }));
    }
    assert!(
        core.person_tasks.insert(request, PersonTaskRoute::PoolSet { project, person }) == Ok(None),
        "one admitted pool route"
    );
    if core.tasks.funding(funder).is_some() {
        work.push(Event::Tasks(tasks::Event::ResizePool {
            reply_to: ReplyTo::new(request),
            project,
            person,
            period: core.settings.period,
            budget,
        }));
    } else {
        work.push(Event::Tasks(tasks::Event::CarvePool {
            reply_to: ReplyTo::new(request),
            project,
            person,
            period: core.settings.period,
            budget,
        }));
    }
}

#[expect(clippy::too_many_arguments, reason = "an exact pending goal carries the keyed holder decision")]
fn goal_decide(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    role: Option<people::Role>,
    project: u32,
    proposer: u64,
    number: u64,
    choice: people::ProposalDecision,
) {
    let Some(proposal) = core.tasks.person_proposal(proposer, number) else {
        pool_refused(work, request, people::Refusal::Ended);
        return;
    };
    if let Err(why) = core.goal_standing(&proposal, project, role) {
        pool_refused(work, request, why);
        return;
    }
    let event = match choice {
        people::ProposalDecision::Accept => {
            let source = match core.goal_accept_allowed(
                &proposal,
                project,
                person,
                role.expect("checked standing"),
                env.limits.tasks.tree_tasks,
            ) {
                Ok(source) => source,
                Err(why) => {
                    pool_refused(work, request, why);
                    return;
                }
            };
            let (period, pool) = core.goal_open_pool(project, person);
            if let Some(event) = period {
                work.push(Event::Tasks(event));
            }
            if let Some(event) = pool {
                work.push(Event::Tasks(event));
            }
            let mut goal = proposal.goal.clone();
            goal.funder = source;
            assert!(
                core.goal_routes.insert(
                    request,
                    GoalRoute::Accepting { proposer, proposal: number, by: person, task: goal.number },
                ) == Ok(None),
                "one goal acceptance"
            );
            tasks::Event::Make {
                reply_to: ReplyTo::new(request),
                creator: tasks::Party::Person(proposer),
                batch: Box::new([goal]),
            }
        }
        people::ProposalDecision::Reject { reason } => {
            let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                pool_refused(work, request, people::Refusal::Limit);
                return;
            };
            assert!(
                core.goal_routes.insert(request, GoalRoute::Deciding { proposer, proposal: number, by: person })
                    == Ok(None),
                "one goal decision"
            );
            tasks::Event::DecidePersonProposal {
                reply_to: ReplyTo::new(request),
                proposer,
                proposal: number,
                by: tasks::Party::Person(person),
                message: Some(message),
                decision: tasks::ProposalDecision::Reject { reason },
            }
        }
        people::ProposalDecision::Pass => {
            pool_refused(work, request, people::Refusal::NoFurther);
            return;
        }
    };
    work.push(Event::Tasks(event));
}

#[expect(
    clippy::too_many_arguments,
    reason = "one authenticated goal carries its task specification and funding request"
)]
fn begin_goal_creation(
    core: &mut Core,
    env: &Env<Limits>,
    out: &mut Queue<Request>,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    project: u32,
    role: people::Role,
    spec: Box<[u8]>,
    charter: u32,
    budget: u64,
    priority: u32,
) {
    let admitted = match core.goal_start(project, person, role, charter, budget, env.limits.tasks.tree_tasks) {
        Ok(admitted) => admitted,
        Err(why) => {
            pool_refused(work, request, why);
            return;
        }
    };
    let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    };
    let new = tasks::New {
        number,
        project,
        executor: tasks::Executor::Agent { charter },
        spec: tasks::Spec { words: spec, parameters: Box::new([]), inputs: Box::new([]) },
        contract: tasks::Contract::Report { words: env.limits.tasks.result_bytes },
        authority: crate::translate::task_authority(&admitted.authority),
        numbers: tasks::Numbers { budget, spent: 0, spent_below: 0, reserved: 0 },
        funder: admitted.source,
        dependencies: Box::new([]),
        holdings: Box::new([]),
        wake: tasks::WakePolicy::DEFAULT,
        recurring: None,
        tracked: Some(priority),
    };
    if !tasks::valid_spec(&env.limits.tasks, &new.spec) || !tasks::valid_authority(&env.limits.tasks, &new.authority) {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    }
    begin_creation(core, env, out, work, request, CreationKind::Goal { person, direct: admitted.direct }, new);
}

#[expect(clippy::too_many_arguments, reason = "one authenticated chat carries its membership and opening words")]
fn begin_chat_creation(
    core: &mut Core,
    env: &Env<Limits>,
    out: &mut Queue<Request>,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    project: u32,
    role: people::Role,
    words: Box<[u8]>,
) {
    if let Err(why) = core.chat_admit(project, person, role, env.limits.tasks.tree_tasks) {
        pool_refused(work, request, why);
        return;
    }
    let (period, pool) = core.goal_open_pool(project, person);
    if let Some(event) = period {
        work.push(Event::Tasks(event));
    }
    if let Some(event) = pool {
        work.push(Event::Tasks(event));
    }
    let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    };
    let new = tasks::New {
        number,
        project,
        executor: tasks::Executor::Agent { charter: core.settings.charter },
        spec: tasks::Spec { words, parameters: Box::new([]), inputs: Box::new([]) },
        contract: tasks::Contract::Report { words: env.limits.tasks.result_bytes },
        authority: crate::translate::task_authority(&core.settings.chat_authority),
        numbers: tasks::Numbers {
            budget: core.settings.chat_authority.budget.spend,
            spent: 0,
            spent_below: 0,
            reserved: 0,
        },
        funder: tasks::Funder::Pool { project, person, period: core.settings.period },
        dependencies: Box::new([]),
        holdings: Box::new([]),
        wake: tasks::WakePolicy::DEFAULT,
        recurring: None,
        tracked: None,
    };
    begin_creation(core, env, out, work, request, CreationKind::Chat { person }, new);
}

fn begin_creation(
    core: &mut Core,
    env: &Env<Limits>,
    out: &mut Queue<Request>,
    work: &mut Queue<Event>,
    request: Token,
    kind: CreationKind,
    new: tasks::New,
) {
    if core.creating.len() == core.creating.capacity() {
        pool_refused(work, request, people::Refusal::Busy);
        return;
    }
    let remaining = u32::try_from(core.connectors.len()).expect("configured connector count fits u32");
    assert!(remaining > 0, "task creation has a numbered connector");
    for &connector in core.connectors.as_ref() {
        out.push(Request::Ask {
            connector,
            ask: Ask::TaskHoldings {
                request,
                project: new.project,
                root: new.number,
                number: new.number,
                executor: new.executor,
                spec: new.spec.clone(),
            },
        });
    }
    let inserted = core.creating.insert(
        request,
        Creation {
            kind,
            new,
            remaining,
            holdings: List::with_capacity(env.limits.tasks.holdings),
            answered: List::with_capacity(remaining),
            failed: false,
        },
    );
    match inserted {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => unreachable!("one keyed task creation"),
    }
}

fn created_holdings(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    connector: u16,
    holdings: Option<Box<[tasks::Holding]>>,
) {
    let Some(creating) = core.creating.get_mut(&request) else { return };
    assert!(creating.remaining > 0, "one holdings answer per connector");
    assert!(
        core.connectors.contains(&connector) && !creating.answered.as_slice().contains(&connector),
        "one answer from each numbered connector"
    );
    creating.answered.push(connector).expect("one answer per configured connector");
    creating.remaining = creating.remaining.checked_sub(1).expect("one holdings answer per connector");
    match holdings {
        Some(holdings) => {
            for holding in holdings {
                if creating.holdings.push(holding).is_err() {
                    creating.failed = true;
                }
            }
        }
        None => creating.failed = true,
    }
    if creating.remaining != 0 {
        return;
    }
    let creating = core.creating.remove(&request).expect("completed creation flight");
    if creating.failed {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    }
    let mut new = creating.new;
    new.holdings = creating.holdings.into_boxed();
    match creating.kind {
        CreationKind::Goal { person, direct: true } => {
            let (period, pool) = core.goal_open_pool(new.project, person);
            if let Some(event) = period {
                work.push(Event::Tasks(event));
            }
            if let Some(event) = pool {
                work.push(Event::Tasks(event));
            }
            assert!(core.made.insert(request, (new.number, true)) == Ok(None), "one goal make flight");
            work.push(Event::Tasks(tasks::Event::Make {
                reply_to: ReplyTo::new(request),
                creator: tasks::Party::Person(person),
                batch: Box::new([new]),
            }));
        }
        CreationKind::Goal { person, direct: false } => {
            let Some(proposal) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                pool_refused(work, request, people::Refusal::Limit);
                return;
            };
            assert!(
                core.goal_routes.insert(request, GoalRoute::Proposing { proposal }) == Ok(None),
                "one goal proposal flight"
            );
            work.push(Event::Tasks(tasks::Event::ProposePerson {
                reply_to: ReplyTo::new(request),
                proposal: tasks::PersonProposal {
                    number: proposal,
                    proposer: person,
                    project: new.project,
                    goal: new,
                    state: tasks::PersonProposalState::Pending { since: env.wall },
                },
            }));
        }
        CreationKind::Chat { person } => {
            assert!(core.made.insert(request, (new.number, false)) == Ok(None), "one chat make flight");
            work.push(Event::Tasks(tasks::Event::Make {
                reply_to: ReplyTo::new(request),
                creator: tasks::Party::Person(person),
                batch: Box::new([new]),
            }));
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "a keyed proposal decision carries its holder and exact live proposal")]
fn person_proposal_decide(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    role: Option<people::Role>,
    project: u32,
    proposer: u64,
    number: u64,
    choice: people::ProposalDecision,
) {
    let Some(proposal) = core.tasks.proposal(proposer, number) else {
        pool_refused(work, request, people::Refusal::Ended);
        return;
    };
    if proposal.project != project {
        pool_refused(work, request, people::Refusal::Standing);
        return;
    }
    let current = match proposal.state {
        tasks::ProposalState::Pending { holder, .. } => holder,
        tasks::ProposalState::Accepted { .. }
        | tasks::ProposalState::Rejected { .. }
        | tasks::ProposalState::Withdrawn => {
            pool_refused(work, request, people::Refusal::Ended);
            return;
        }
    };
    let (_, proposal_kind) = Core::proposal_kind(&proposal.action);
    let standing = match current {
        tasks::ProposalHolder::Person(holder) => holder == person,
        tasks::ProposalHolder::Policy { project: named, .. } => {
            named == project && core.proposal_policy_standing(person, project, proposal_kind)
        }
        tasks::ProposalHolder::Task(_) => false,
    };
    if !standing || role.is_none() {
        pool_refused(work, request, people::Refusal::Standing);
        return;
    }
    let next = match choice {
        people::ProposalDecision::Accept => {
            let Some(action) = Core::proposal_action_for_check(&proposal.action) else {
                pool_refused(work, request, people::Refusal::Authority);
                return;
            };
            if !core.proposal_covers_person(
                &env.limits,
                &authority::needs(&action).expect("admitted proposal needs"),
                person,
                project,
                proposal_kind,
            ) {
                pool_refused(work, request, people::Refusal::Authority);
                return;
            }
            person_proposal_accept(core, work, request, person, proposal);
            return;
        }
        people::ProposalDecision::Reject { reason } => tasks::ProposalDecision::Reject { reason },
        people::ProposalDecision::Pass => match current {
            tasks::ProposalHolder::Person(_) => tasks::ProposalDecision::Pass {
                holder: tasks::ProposalHolder::Policy { project, kind: Core::proposal_kind(&proposal.action).0 },
            },
            tasks::ProposalHolder::Policy { .. } => {
                pool_refused(work, request, people::Refusal::NoFurther);
                return;
            }
            tasks::ProposalHolder::Task(_) => unreachable!("person standing checked"),
        },
    };
    let message = match next {
        tasks::ProposalDecision::Reject { .. } => match crate::fresh(&mut core.counters, crate::Family::Message) {
            Some(message) => Some(message),
            None => {
                pool_refused(work, request, people::Refusal::Limit);
                return;
            }
        },
        tasks::ProposalDecision::Pass { .. } => None,
        tasks::ProposalDecision::Accept => unreachable!("accept routed separately"),
    };
    assert!(
        core.routing_people_proposals
            .insert(request, PersonProposalRoute::Deciding { request, proposer, proposal: number, by: person },)
            == Ok(None),
        "one person proposal route"
    );
    work.push(Event::PersonProposal(tasks::Event::DecideProposal {
        reply_to: ReplyTo::new(request),
        proposer,
        proposal: number,
        message,
        by: tasks::Party::Person(person),
        decision: next,
    }));
}

#[expect(clippy::too_many_lines, reason = "one exhaustive authenticated person proposal dispatcher")]
fn person_proposal_accept(
    core: &mut Core,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    proposal: tasks::Proposal,
) {
    let result_followups = core.tasks.result_proposal(proposal.proposer, proposal.number);
    match proposal.action {
        tasks::ProposalAction::Effect { connector, .. } => {
            work.push(Event::EffectStart {
                owner: request,
                connector,
                origin: crate::EffectOrigin::Accept {
                    to: ReplyTo::new(request),
                    key: None,
                    proposal: Box::new(proposal),
                    by: tasks::Party::Person(person),
                },
            });
            return;
        }
        tasks::ProposalAction::Batch(_)
        | tasks::ProposalAction::Amend { .. }
        | tasks::ProposalAction::Widen { .. }
        | tasks::ProposalAction::Release { .. } => {}
    }
    let event = match proposal.action {
        tasks::ProposalAction::Effect { .. } => unreachable!("effect accepted through its connector"),
        tasks::ProposalAction::Batch(mut batch) => {
            let creator =
                if proposal.as_holder { tasks::Party::Person(person) } else { tasks::Party::Task(proposal.proposer) };
            let (period, pool) = core.goal_open_pool(proposal.project, person);
            if let Some(event) = period {
                work.push(Event::Tasks(event));
            }
            if let Some(event) = pool {
                work.push(Event::Tasks(event));
            }
            for member in &mut batch {
                member.funder = tasks::Funder::Pool { project: proposal.project, person, period: core.settings.period };
            }
            if result_followups {
                tasks::Event::MakeResultFollowups {
                    reply_to: ReplyTo::new(request),
                    proposer: proposal.proposer,
                    proposal: proposal.number,
                    batch,
                }
            } else {
                tasks::Event::Make { reply_to: ReplyTo::new(request), creator, batch }
            }
        }
        tasks::ProposalAction::Amend { task, amendment } => {
            let Some(current) = core.tasks.delegation(task) else {
                pool_refused(work, request, people::Refusal::Ended);
                return;
            };
            let Some(amend_message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                pool_refused(work, request, people::Refusal::Limit);
                return;
            };
            let after = crate::translate::authority_value(amendment.authority.as_ref().expect("admitted authority"));
            let before = crate::translate::authority_value(&current.authority);
            let stop_run = !authority::at_most(&before, &after, &core.authority.rules().implies);
            tasks::Event::Amend {
                reply_to: ReplyTo::new(request),
                by: tasks::Party::Person(person),
                task,
                message: amend_message,
                stop_run,
                amendment,
            }
        }
        tasks::ProposalAction::Widen { task, authority } => {
            let Some(amend_message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                pool_refused(work, request, people::Refusal::Limit);
                return;
            };
            tasks::Event::Amend {
                reply_to: ReplyTo::new(request),
                by: tasks::Party::Person(person),
                task,
                message: amend_message,
                stop_run: false,
                amendment: tasks::Amendment {
                    spec: None,
                    wake: None,
                    dependencies: None,
                    authority: Some(authority),
                    reason: proposal.reason,
                },
            }
        }
        tasks::ProposalAction::Release { task } => tasks::Event::Control {
            reply_to: ReplyTo::new(request),
            by: tasks::Party::Person(person),
            task,
            control: tasks::Control::Release,
        },
    };
    let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    };
    assert!(
        core.routing_people_proposals.insert(
            request,
            PersonProposalRoute::Accepting {
                request,
                person,
                proposer: proposal.proposer,
                proposal: proposal.number,
                message,
            },
        ) == Ok(None),
        "one person acceptance route"
    );
    work.push(Event::PersonProposal(event));
}

#[expect(clippy::too_many_arguments, reason = "one keyed held choice carries its person and exact semantic revision")]
fn person_escalation_decide(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    role: Option<people::Role>,
    project: u32,
    task: u64,
    revision: u64,
    decision: people::EscalationDecision,
) {
    if let people::EscalationDecision::Reject { reason } = &decision
        && reason.len() > usize::try_from(env.limits.escalation_reason_bytes).expect("reason bound fits usize")
    {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    }
    let Some(context) = core.tasks.escalation(task) else {
        pool_refused(work, request, people::Refusal::Unknown);
        return;
    };
    if context.project != project || !core.escalation_visible(&context, person, role) {
        pool_refused(work, request, people::Refusal::Standing);
        return;
    }
    let pending = match context.escalation {
        tasks::Escalation::Waiting { revision: current, .. } => current == revision,
        tasks::Escalation::Unheld { .. } | tasks::Escalation::Routing { .. } | tasks::Escalation::Rejected { .. } => {
            false
        }
    };
    if !pending {
        pool_refused(work, request, people::Refusal::Unknown);
        return;
    }
    if !Core::escalation_standing(&context, person, role) {
        pool_refused(work, request, people::Refusal::Standing);
        return;
    }
    let semantic = match &decision {
        people::EscalationDecision::Release => {
            if !core.escalation_covers(&env.limits, &context, person, role)
                || !core.escalation_release_allowed(&context, person, role, env.limits.tasks.tree_tasks)
            {
                pool_refused(work, request, people::Refusal::Authority);
                return;
            }
            tasks::EscalationDecision::Release
        }
        people::EscalationDecision::Reject { reason } => tasks::EscalationDecision::Reject { reason: reason.clone() },
        people::EscalationDecision::Pass => {
            let Some(holder) = core.escalation_fallback(project) else {
                pool_refused(work, request, people::Refusal::Authority);
                return;
            };
            tasks::EscalationDecision::Pass { holder }
        }
    };
    let entry = match semantic {
        tasks::EscalationDecision::Pass { .. } => match crate::fresh(&mut core.counters, crate::Family::Message) {
            Some(entry) => Some(entry),
            None => {
                pool_refused(work, request, people::Refusal::Limit);
                return;
            }
        },
        tasks::EscalationDecision::Release | tasks::EscalationDecision::Reject { .. } => None,
    };
    assert!(
        core.person_escalations.insert(
            request,
            PersonEscalation { request, requester: context.requester, person, project, task, revision, decision },
        ) == Ok(None),
        "one keyed escalation choice"
    );
    work.push(Event::PersonEscalation(tasks::Event::DecideEscalation {
        reply_to: ReplyTo::new(request),
        task,
        revision,
        by: person,
        entry,
        decision: semantic,
    }));
}

fn current_person_escalation(core: &Core, task: u64, revision: u64) -> bool {
    let Some(context) = core.tasks.escalation(task) else { return false };
    match context.escalation {
        tasks::Escalation::Waiting { revision: current, .. } => current == revision,
        tasks::Escalation::Unheld { .. } | tasks::Escalation::Routing { .. } | tasks::Escalation::Rejected { .. } => {
            false
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "one keyed role edit carries the candidate roster and its owner")]
fn roles_route(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    out: &mut Queue<Request>,
    request: Token,
    person: u64,
    project: u32,
    holdings: Box<[people::Holding]>,
) {
    if let Err(why) = core.role_allowed(person, project, env.limits.tasks.tree_tasks) {
        pool_refused(work, request, why);
        return;
    }
    for holding in &holdings {
        match holding.role {
            people::Role::Policy { role } => {
                if core.authority.role(project, role).is_none() {
                    pool_refused(work, request, people::Refusal::Unknown);
                    return;
                }
            }
            people::Role::Owner | people::Role::Maintainer | people::Role::Member | people::Role::Observer => {}
        }
    }
    let contexts = match core.tasks.project_escalations(&env.limits.tasks, project) {
        Ok(contexts) => contexts,
        Err(refusal) => {
            assert!(refusal == tasks::Refusal::NotReady, "project inspection refusal is readiness");
            pool_refused(work, request, people::Refusal::NotReady);
            return;
        }
    };
    let changed = match core.roles_preflight(&env.limits, &contexts, &holdings) {
        Ok(changed) => changed,
        Err(why) => {
            pool_refused(work, request, why);
            return;
        }
    };
    let handoffs = u32::try_from(contexts.len()).expect("bounded Waiting count").checked_add(1).expect("recheck room");
    drop(contexts);
    drop(holdings);
    if work.room() < handoffs.checked_add(env.limits.tasks.tasks).expect("validated recheck bound")
        || out.room() < changed.checked_add(2).expect("validated role cohort bound")
    {
        pool_refused(work, request, people::Refusal::Busy);
        return;
    }
    if let Err(why) = core.role_allowed(person, project, env.limits.tasks.tree_tasks) {
        pool_refused(work, request, why);
        return;
    }
    let mut applied = Queue::with_capacity(people::max_out(&env.limits.people));
    people::step(
        &mut core.people,
        &Env { now: env.now, wall: env.wall, limits: env.limits.people },
        people::Event::ApplyRoles { reply_to: ReplyTo::new(request), request },
        &mut applied,
    );
    let mut terminal = None;
    for _ in 0..applied.len() {
        match applied.pop().expect("role application output count") {
            people::Request::Save { record } => out.push(Request::Write(Write::Save(Record::People(record)))),
            people::Request::RolesApplied { reply_to, request: actual, result } => {
                assert!(
                    reply_to.into_token() == request && actual == request && terminal.is_none(),
                    "one role terminal"
                );
                terminal = Some(result);
            }
            people::Request::Erase { .. }
            | people::Request::Route { .. }
            | people::Request::Reply { .. }
            | people::Request::RolesRefused { .. }
            | people::Request::ServiceMade { .. }
            | people::Request::RestoreRefused { .. } => unreachable!("role application has one save and terminal"),
        }
    }
    if let Err(why) = terminal.expect("role application terminal") {
        pool_refused(work, request, why);
        return;
    }
    work.push(Event::Tasks(tasks::Event::RecheckEscalations { reply_to: ReplyTo::new(request), project }));
    let mut reroutes = core.proposal_recheck_project(&env.limits, project);
    for _ in 0..reroutes.len() {
        work.push(Event::Tasks(reroutes.pop().expect("bounded proposal recheck")));
    }
    work.push(Event::People(people::Event::Decided { request, outcome: people::Outcome::RolesSet { project } }));
}

#[expect(clippy::too_many_arguments, reason = "a keyed amendment carries its person, project and task")]
fn amend_route(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    role: Option<people::Role>,
    project: u32,
    task: u64,
    amendment: people::Amendment,
) {
    let Some(amendment) = crate::amendments::translate(amendment, &env.limits.tasks) else {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    };
    let (stop_run, propose) =
        match core.amend_admit(person, role, project, task, &amendment, env.limits.tasks.depth, &env.limits.tasks) {
            Ok(admitted) => admitted,
            Err(why) => {
                pool_refused(work, request, why);
                return;
            }
        };
    if propose {
        let Some(proposal) = crate::fresh(&mut core.counters, crate::Family::Message) else {
            pool_refused(work, request, people::Refusal::Limit);
            return;
        };
        assert!(
            core.person_tasks.insert(request, PersonTaskRoute::AmendProposed { task, proposal }) == Ok(None),
            "one amendment proposal"
        );
        work.push(Event::Tasks(tasks::Event::Propose {
            reply_to: ReplyTo::new(request),
            proposal: tasks::Proposal {
                number: proposal,
                proposer: task,
                project,
                reason: amendment.reason.clone(),
                as_holder: false,
                action: tasks::ProposalAction::Amend { task, amendment },
                state: tasks::ProposalState::Pending {
                    holder: tasks::ProposalHolder::Policy { project, kind: tasks::ProposalKind::Amend },
                    since: env.wall,
                },
            },
        }));
        return;
    }
    let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
        pool_refused(work, request, people::Refusal::Limit);
        return;
    };
    assert!(core.person_tasks.insert(request, PersonTaskRoute::Amended(task)) == Ok(None), "one person amendment");
    work.push(Event::Tasks(tasks::Event::Amend {
        reply_to: ReplyTo::new(request),
        by: tasks::Party::Person(person),
        task,
        message,
        stop_run,
        amendment,
    }));
}

fn person_move_refusal(why: tasks::Refusal) -> people::Refusal {
    match why {
        tasks::Refusal::Busy | tasks::Refusal::NotReady => people::Refusal::Busy,
        tasks::Refusal::Unknown => people::Refusal::Ended,
        tasks::Refusal::Reference | tasks::Refusal::State => people::Refusal::Standing,
        tasks::Refusal::Funding => people::Refusal::Authority,
        tasks::Refusal::Duplicate
        | tasks::Refusal::Empty
        | tasks::Refusal::Batch
        | tasks::Refusal::Live
        | tasks::Refusal::Project
        | tasks::Refusal::Tree
        | tasks::Refusal::Depth
        | tasks::Refusal::Delegates
        | tasks::Refusal::Subscription
        | tasks::Refusal::Dependencies
        | tasks::Refusal::Cycle
        | tasks::Refusal::Executor
        | tasks::Refusal::Spec
        | tasks::Refusal::Contract
        | tasks::Refusal::AuthorityShape
        | tasks::Refusal::Inputs
        | tasks::Refusal::Attempt
        | tasks::Refusal::LiveDelegates
        | tasks::Refusal::Restore
        | tasks::Refusal::Read
        | tasks::Refusal::Turn
        | tasks::Refusal::HoldKind
        | tasks::Refusal::HoldTaken
        | tasks::Refusal::Holds => people::Refusal::Limit,
    }
}

fn person_message_refusal(why: tasks::Refusal) -> people::Refusal {
    match why {
        tasks::Refusal::Busy | tasks::Refusal::NotReady => people::Refusal::Busy,
        tasks::Refusal::Unknown => people::Refusal::Unknown,
        tasks::Refusal::State => people::Refusal::Standing,
        tasks::Refusal::Duplicate
        | tasks::Refusal::Empty
        | tasks::Refusal::Batch
        | tasks::Refusal::Live
        | tasks::Refusal::Project
        | tasks::Refusal::Tree
        | tasks::Refusal::Depth
        | tasks::Refusal::Delegates
        | tasks::Refusal::Reference
        | tasks::Refusal::Subscription
        | tasks::Refusal::Dependencies
        | tasks::Refusal::Cycle
        | tasks::Refusal::Executor
        | tasks::Refusal::Spec
        | tasks::Refusal::Contract
        | tasks::Refusal::AuthorityShape
        | tasks::Refusal::Inputs
        | tasks::Refusal::Attempt
        | tasks::Refusal::LiveDelegates
        | tasks::Refusal::Restore
        | tasks::Refusal::Read
        | tasks::Refusal::Turn
        | tasks::Refusal::Funding
        | tasks::Refusal::HoldKind
        | tasks::Refusal::HoldTaken
        | tasks::Refusal::Holds => people::Refusal::Limit,
    }
}

fn result_words(result: tasks::TaskResult) -> Box<[u8]> {
    match result {
        tasks::TaskResult::Report { words }
        | tasks::TaskResult::Verdict { words, .. }
        | tasks::TaskResult::Change { words, .. } => words,
        tasks::TaskResult::Failure { reason } => reason,
    }
}

fn ending_words(ending: tasks::Ending) -> Box<[u8]> {
    match ending {
        tasks::Ending::Done(result) => result_words(result),
        tasks::Ending::Failed { reason } => reason,
        tasks::Ending::Cancelled { reason, result } => match result {
            Some(result) => result_words(result),
            None => reason,
        },
    }
}

fn result_notice(ending: tasks::Ending) -> (tasks::ResultKind, Box<[u8]>) {
    match ending {
        tasks::Ending::Done(result) => match result {
            tasks::TaskResult::Report { words } => (tasks::ResultKind::Report, words),
            tasks::TaskResult::Verdict { code, words } => (tasks::ResultKind::Verdict { code }, words),
            tasks::TaskResult::Change { connector, kind, resource, words } => {
                (tasks::ResultKind::Change { connector, kind, resource }, words)
            }
            tasks::TaskResult::Failure { reason } => (tasks::ResultKind::Failed, reason),
        },
        tasks::Ending::Failed { reason } => (tasks::ResultKind::Failed, reason),
        tasks::Ending::Cancelled { reason, .. } => (tasks::ResultKind::Cancelled, reason),
    }
}

fn task_saved_view(core: &mut Core, env: &Env<Limits>, task: &tasks::TaskRecord, out: &mut Queue<Request>) {
    let phase = tasks::view_phase(&task.phase);
    let current = (phase, task.tracked);
    let changed = match core.view_phases.get(&task.number) {
        Some(previous) => *previous != current,
        None => true,
    };
    if phase == 4 {
        core.view_phases.remove(&task.number);
    } else {
        core.view_phases.insert(task.number, current).expect("one live phase per task");
    }
    if !changed || core.watching.is_empty() {
        return;
    }
    let mut trees = List::with_capacity(env.limits.tasks.depth.saturating_add(1));
    trees.push(Token::new(task.number)).expect("self is in its own bounded tree");
    let mut requester = task.requester;
    for _ in 0..env.limits.tasks.depth {
        requester = match requester {
            tasks::Party::Task(parent) => {
                trees.push(Token::new(parent)).expect("bounded ancestor depth");
                match core.tasks.delegation(parent) {
                    Some(context) => context.requester,
                    None => break,
                }
            }
            tasks::Party::Person(_) | tasks::Party::Deployment { .. } => break,
        };
    }
    out.push(Request::Held(Box::new(Held::ViewTaskPhase {
        task: Token::new(task.number),
        trees: trees.into_boxed(),
        project: task.project,
        phase,
        priority: task.tracked,
    })));
}

fn call_decision(core: &mut Core, out: &mut Queue<Request>, to: ReplyTo, key: CallKey, part: CallPart) {
    if core.decide_named_call(key, part.clone()) {
        out.push(Request::Write(Write::Save(Record::Core(CoreRecord::Call(CallRecord {
            key,
            part: part.clone(),
            settled: core.call_settled.get(&key).cloned(),
        })))));
    }
    out.push(Request::Held(Box::new(Held::CallAnswer { to, key, part })));
}

fn escalation_refused(core: &mut Core, to: ReplyTo, key: CallKey, task: u64, why: tasks::Refusal) -> Requests {
    let mut out = Queue::with_capacity(3);
    call_decision(
        core,
        &mut out,
        to,
        key,
        CallPart::EscalationRefused(tasks::Problem { task: Some(task), why, blocked_by: None }),
    );
    out.push(Request::Decided);
    Requests::Out(out)
}

fn proposal_refused(core: &mut Core, to: ReplyTo, key: CallKey, why: tasks::Refusal) -> Requests {
    let mut out = Queue::with_capacity(3);
    call_decision(
        core,
        &mut out,
        to,
        key,
        CallPart::ProposalRefused(tasks::Problem { task: Some(key.task), why, blocked_by: None }),
    );
    out.push(Request::Decided);
    Requests::Out(out)
}

fn delegate_refused(core: &mut Core, to: ReplyTo, key: CallKey, task: Option<u64>, why: tasks::Refusal) -> Requests {
    let mut out = Queue::with_capacity(3);
    call_decision(core, &mut out, to, key, CallPart::DelegationRefused(tasks::Problem { task, why, blocked_by: None }));
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(
    clippy::too_many_lines,
    reason = "one bounded delegated batch receives task identities and connector handoffs"
)]
fn delegate_validated(
    core: &mut Core,
    env: &Env<Limits>,
    to: ReplyTo,
    key: CallKey,
    batch: Box<[Delegate]>,
    stubs: Box<[tasks::Stub]>,
) -> Requests {
    if let Some(part) = core.authorize_tool(key, ToolKind::Delegate) {
        let mut out = Queue::with_capacity(3);
        call_decision(core, &mut out, to, key, part);
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    if let Err(part) = core.delegate_preflight(&env.limits.tasks, key, &batch) {
        let mut out = Queue::with_capacity(3);
        call_decision(core, &mut out, to, key, part);
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let context = core.tasks.delegation(key.task).expect("preflighted delegator");
    let root = core.tasks.root(key.task).expect("delegator live");
    let mut numbers = List::with_capacity(env.limits.tasks.batch);
    for _ in &batch {
        let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else {
            return delegate_refused(core, to, key, None, tasks::Refusal::Live);
        };
        numbers.push(number).expect("bounded delegation IDs");
    }
    let mut created = List::with_capacity(env.limits.tasks.batch);
    for (index, member) in batch.into_iter().enumerate() {
        let index = u32::try_from(index).expect("batch length fits u32");
        let mut dependencies = List::with_capacity(env.limits.tasks.dependencies);
        for dependency in member.dependencies {
            let number = match dependency {
                Dependency::Batch(at) => match numbers.get(at) {
                    Some(number) => *number,
                    None => {
                        return delegate_refused(
                            core,
                            to,
                            key,
                            numbers.get(index).copied(),
                            tasks::Refusal::Dependencies,
                        );
                    }
                },
                Dependency::Existing(number) => number,
            };
            if dependencies.push(number).is_err() {
                return delegate_refused(core, to, key, numbers.get(index).copied(), tasks::Refusal::Dependencies);
            }
        }
        let number = *numbers.get(index).expect("one ID per member");
        let Some(authority) = crate::resolved_delegate_authority(
            &member.authority,
            &member.symbolic_grants,
            number,
            core.authority.limits(),
        ) else {
            return delegate_refused(core, to, key, Some(number), tasks::Refusal::AuthorityShape);
        };
        created
            .push(tasks::New {
                number,
                project: context.project,
                executor: member.executor,
                spec: member.spec,
                contract: member.contract,
                authority,
                numbers: tasks::Numbers {
                    budget: member.authority.budget.spend,
                    spent: 0,
                    spent_below: 0,
                    reserved: 0,
                },
                funder: tasks::Funder::Task(key.task),
                dependencies: dependencies.into_boxed(),
                holdings: Box::new([]),
                wake: member.wake,
                recurring: None,
                tracked: None,
            })
            .expect("bounded delegation batch");
    }
    let request = to.into_token();
    let members = created.into_boxed();
    if !core.pending_calls.contains_key(&key) {
        assert!(core.pending_calls.insert(key, true) == Ok(None), "delegation call record room reserved");
    }
    let remaining = u32::try_from(core.connectors.len()).expect("configured connector count fits u32");
    assert!(remaining > 0, "delegation has a numbered connector");
    let mut asked = List::with_capacity(u32::try_from(members.len()).expect("bounded delegation batch"));
    for member in &members {
        asked
            .push(HoldingsNeed {
                project: member.project,
                root,
                number: member.number,
                executor: member.executor,
                spec: member.spec.clone(),
            })
            .expect("one holdings request per member");
    }
    assert!(
        core.creating_delegates
            .insert(
                request,
                DelegateCreation {
                    key,
                    kind: BatchKind::Delegation { stubs },
                    members,
                    remaining,
                    answered: List::with_capacity(remaining),
                    failed: false,
                }
            )
            .is_ok(),
        "one named delegation flight"
    );
    let mut out = Queue::with_capacity(remaining.checked_add(1).expect("connector handoff room"));
    for &connector in core.connectors.as_ref() {
        out.push(Request::Ask {
            connector,
            ask: Ask::DelegateHoldings { request, from: key.task, members: asked.as_slice().into() },
        });
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn delegate_holdings(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    request: Token,
    connector: u16,
    holdings: Option<Box<[Box<[tasks::Holding]>]>>,
) -> Requests {
    let Some(flight) = core.creating_delegates.get_mut(&request) else {
        let mut out = Queue::with_capacity(1);
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    assert!(flight.remaining > 0, "one holdings answer per connector");
    assert!(
        core.connectors.contains(&connector) && !flight.answered.as_slice().contains(&connector),
        "one answer per connector"
    );
    flight.answered.push(connector).expect("configured connector bound");
    flight.remaining = flight.remaining.checked_sub(1).expect("one connector answer");
    match holdings {
        Some(holdings) if holdings.len() == flight.members.len() => {
            for (index, added) in holdings.into_iter().enumerate() {
                let member = flight.members.get_mut(index).expect("one answer per member");
                let mut whole = List::with_capacity(env.limits.tasks.holdings);
                for holding in member.holdings.iter().chain(added.iter()) {
                    if whole.push(holding.clone()).is_err() {
                        flight.failed = true;
                    }
                }
                member.holdings = whole.into_boxed();
            }
        }
        Some(_) | None => flight.failed = true,
    }
    if flight.remaining > 0 {
        let mut out = Queue::with_capacity(1);
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let flight = core.creating_delegates.remove(&request).expect("completed delegate holdings");
    if flight.failed {
        return match flight.kind {
            BatchKind::Delegation { .. } => {
                delegate_refused(core, ReplyTo::new(request), flight.key, None, tasks::Refusal::Holds)
            }
            BatchKind::Proposal { .. } => {
                proposal_refused(core, ReplyTo::new(request), flight.key, tasks::Refusal::Holds)
            }
        };
    }
    match flight.kind {
        BatchKind::Delegation { stubs } => {
            assert!(core.delegating.insert(request, (flight.key, stubs)) == Ok(None), "one pending delegated call");
            work.push(Event::Tasks(tasks::Event::Make {
                reply_to: ReplyTo::new(request),
                creator: tasks::Party::Task(flight.key.task),
                batch: flight.members,
            }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        BatchKind::Proposal { reason, as_holder } => named_propose(
            core,
            env,
            work,
            ReplyTo::new(request),
            flight.key,
            tasks::ProposalAction::Batch(flight.members),
            reason,
            as_holder,
        ),
    }
}

fn propose_batch(
    core: &mut Core,
    env: &Env<Limits>,
    to: ReplyTo,
    key: CallKey,
    batch: Box<[Delegate]>,
    reason: Box<[u8]>,
    as_holder: bool,
) -> Requests {
    if reason.len() > usize::try_from(env.limits.tasks.message_bytes).expect("u32 fits usize") {
        return proposal_refused(core, to, key, tasks::Refusal::Read);
    }
    let Some(context) = core.tasks.delegation(key.task) else {
        return proposal_refused(core, to, key, tasks::Refusal::Unknown);
    };
    if batch.is_empty() || batch.len() > usize::try_from(env.limits.tasks.batch).expect("u32 fits usize") {
        return proposal_refused(core, to, key, tasks::Refusal::Batch);
    }
    let root = core.tasks.root(key.task).expect("proposer live");
    let mut numbers = List::with_capacity(env.limits.tasks.batch);
    for _ in &batch {
        let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else {
            return proposal_refused(core, to, key, tasks::Refusal::Live);
        };
        numbers.push(number).expect("bounded proposal task IDs");
    }
    let mut created = List::with_capacity(env.limits.tasks.batch);
    for (index, member) in batch.into_iter().enumerate() {
        let mut dependencies = List::with_capacity(env.limits.tasks.dependencies);
        for dependency in member.dependencies {
            let number = match dependency {
                Dependency::Batch(at) => match numbers.get(at) {
                    Some(number) => *number,
                    None => return proposal_refused(core, to, key, tasks::Refusal::Dependencies),
                },
                Dependency::Existing(number) => number,
            };
            if dependencies.push(number).is_err() {
                return proposal_refused(core, to, key, tasks::Refusal::Dependencies);
            }
        }
        let number = *numbers.get(u32::try_from(index).expect("batch index")).expect("one ID per member");
        created
            .push(tasks::New {
                number,
                project: context.project,
                executor: member.executor,
                spec: member.spec,
                contract: member.contract,
                numbers: tasks::Numbers {
                    budget: member.authority.budget.spend,
                    spent: 0,
                    spent_below: 0,
                    reserved: 0,
                },
                authority: member.authority,
                funder: tasks::Funder::Task(key.task),
                dependencies: dependencies.into_boxed(),
                holdings: Box::new([]),
                wake: member.wake,
                recurring: None,
                tracked: None,
            })
            .expect("bounded proposal batch");
    }
    let request = to.into_token();
    let members = created.into_boxed();
    let remaining = u32::try_from(core.connectors.len()).expect("configured connector count fits u32");
    assert!(remaining > 0, "proposal has a numbered connector");
    let mut asked = List::with_capacity(u32::try_from(members.len()).expect("bounded proposal batch"));
    for member in &members {
        asked
            .push(HoldingsNeed {
                project: member.project,
                root,
                number: member.number,
                executor: member.executor,
                spec: member.spec.clone(),
            })
            .expect("one holdings request per member");
    }
    assert!(
        core.creating_delegates
            .insert(
                request,
                DelegateCreation {
                    key,
                    kind: BatchKind::Proposal { reason, as_holder },
                    members,
                    remaining,
                    answered: List::with_capacity(remaining),
                    failed: false,
                }
            )
            .is_ok(),
        "one proposal holdings flight"
    );
    let mut out = Queue::with_capacity(remaining.checked_add(1).expect("connector handoff room"));
    for &connector in core.connectors.as_ref() {
        out.push(Request::Ask {
            connector,
            ask: Ask::DelegateHoldings { request, from: key.task, members: asked.as_slice().into() },
        });
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn procedure_refused(task: u64, step: u64) -> Requests {
    let mut out = Queue::with_capacity(2);
    out.push(Request::Now(Box::new(Now::ProcedureDelegateOutcome { task, step, child: None })));
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "one procedure batch carries its owner and bounded connector handoffs"
)]
fn procedure_step(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    task: u64,
    step: u64,
    connector: u16,
    procedure_code: u32,
    action: ProcedureAction,
) -> Requests {
    let mut out = Queue::with_capacity(
        u32::try_from(core.connectors.len())
            .expect("bounded connectors")
            .checked_add(1)
            .expect("procedure handoff room"),
    );
    if core.tasks.procedure_due(task) != Some((connector, procedure_code, step)) {
        if match &action {
            ProcedureAction::Delegate(_) => true,
            ProcedureAction::Result(_) | ProcedureAction::Hold(_) | ProcedureAction::Wait => false,
        } {
            return procedure_refused(task, step);
        }
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    match action {
        ProcedureAction::Delegate(batch) => {
            if !core.procedure_batch_admit(&env.limits.tasks, task, &batch) {
                return procedure_refused(task, step);
            }
            let context = core.tasks.delegation(task).expect("admitted procedure owner");
            let root = core.tasks.root(task).expect("procedure task live");
            let mut numbers = List::with_capacity(env.limits.tasks.batch);
            for _ in &batch {
                let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else {
                    return procedure_refused(task, step);
                };
                numbers.push(number).expect("bounded procedure task IDs");
            }
            let mut created = List::with_capacity(env.limits.tasks.batch);
            for (index, member) in batch.into_iter().enumerate() {
                let at = u32::try_from(index).expect("bounded batch index");
                let mut dependencies = List::with_capacity(env.limits.tasks.dependencies);
                for dependency in member.dependencies {
                    let number = match dependency {
                        Dependency::Batch(index) => match numbers.get(index) {
                            Some(number) => *number,
                            None => {
                                return procedure_refused(task, step);
                            }
                        },
                        Dependency::Existing(number) => number,
                    };
                    if dependencies.push(number).is_err() {
                        return procedure_refused(task, step);
                    }
                }
                let number = *numbers.get(at).expect("one ID per member");
                let Some(authority) = crate::resolved_delegate_authority(
                    &member.authority,
                    &member.symbolic_grants,
                    number,
                    core.authority.limits(),
                ) else {
                    return procedure_refused(task, step);
                };
                created
                    .push(tasks::New {
                        number,
                        project: context.project,
                        executor: member.executor,
                        spec: member.spec,
                        contract: member.contract,
                        authority,
                        numbers: tasks::Numbers {
                            budget: member.authority.budget.spend,
                            spent: 0,
                            spent_below: 0,
                            reserved: 0,
                        },
                        funder: tasks::Funder::Task(task),
                        dependencies: dependencies.into_boxed(),
                        holdings: Box::new([]),
                        wake: member.wake,
                        recurring: None,
                        tracked: None,
                    })
                    .expect("bounded procedure batch");
            }
            let members = created.into_boxed();
            let remaining = u32::try_from(core.connectors.len()).expect("configured connector count fits u32");
            assert!(remaining > 0, "procedure has a numbered connector");
            let mut asked = List::with_capacity(u32::try_from(members.len()).expect("bounded procedure batch"));
            for member in &members {
                asked
                    .push(HoldingsNeed {
                        project: member.project,
                        root,
                        number: member.number,
                        executor: member.executor,
                        spec: member.spec.clone(),
                    })
                    .expect("one holdings request per member");
            }
            assert!(
                core.creating_procedures
                    .insert(
                        (task, step),
                        ProcedureCreation {
                            members,
                            remaining,
                            answered: List::with_capacity(remaining),
                            failed: false,
                        }
                    )
                    .is_ok(),
                "one procedure holdings flight"
            );
            for &connector in core.connectors.as_ref() {
                out.push(Request::Ask {
                    connector,
                    ask: Ask::ProcedureHoldings { task, step, members: asked.as_slice().into() },
                });
            }
        }
        ProcedureAction::Result(result) => work.push(Event::Tasks(tasks::Event::Procedure {
            reply_to: ReplyTo::new(Token::new(u64::MAX)),
            task,
            step,
            decision: tasks::ProcedureDecision::Result(result),
        })),
        ProcedureAction::Hold(why) => work.push(Event::Tasks(tasks::Event::Procedure {
            reply_to: ReplyTo::new(Token::new(u64::MAX)),
            task,
            step,
            decision: tasks::ProcedureDecision::Hold(why),
        })),
        ProcedureAction::Wait => work.push(Event::Tasks(tasks::Event::Procedure {
            reply_to: ReplyTo::new(Token::new(u64::MAX)),
            task,
            step,
            decision: tasks::ProcedureDecision::Wait,
        })),
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(clippy::manual_map, reason = "step code does not use closures")]
fn procedure_holdings(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    task: u64,
    step: u64,
    connector: u16,
    holdings: Option<Box<[Box<[tasks::Holding]>]>>,
) -> Requests {
    let Some(flight) = core.creating_procedures.get_mut(&(task, step)) else {
        let mut out = Queue::with_capacity(1);
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    assert!(flight.remaining > 0, "one procedure answer per connector");
    assert!(
        core.connectors.contains(&connector) && !flight.answered.as_slice().contains(&connector),
        "one answer per connector"
    );
    flight.answered.push(connector).expect("configured connector bound");
    flight.remaining = flight.remaining.checked_sub(1).expect("one connector answer");
    match holdings {
        Some(holdings) if holdings.len() == flight.members.len() => {
            for (index, added) in holdings.into_iter().enumerate() {
                let member = flight.members.get_mut(index).expect("one answer per member");
                let mut whole = List::with_capacity(env.limits.tasks.holdings);
                for holding in member.holdings.iter().chain(added.iter()) {
                    if whole.push(holding.clone()).is_err() {
                        flight.failed = true;
                    }
                }
                member.holdings = whole.into_boxed();
            }
        }
        Some(_) | None => flight.failed = true,
    }
    let mut out = Queue::with_capacity(2);
    if flight.remaining == 0 {
        let flight = core.creating_procedures.remove(&(task, step)).expect("completed procedure holdings");
        let child = if flight.failed {
            None
        } else {
            match flight.members.first() {
                Some(member) => Some(member.number),
                None => None,
            }
        };
        out.push(Request::Now(Box::new(Now::ProcedureDelegateOutcome { task, step, child })));
        if !flight.failed {
            work.push(Event::Tasks(tasks::Event::Procedure {
                reply_to: ReplyTo::new(Token::new(u64::MAX)),
                task,
                step,
                decision: tasks::ProcedureDecision::Delegate(flight.members),
            }));
        }
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn brief_assembled(core: &mut Core, work: &mut Queue<Event>, task: u64, ready: bool) -> Requests {
    let mut out = Queue::with_capacity(2);
    if !ready {
        work.push(Event::PreparationFailed { task });
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let Some(context) = core.contexts.get(&task).cloned() else {
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    let Some(attempt) = crate::fresh(&mut core.counters, crate::Family::Run) else {
        work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    assert!(core.workspace_pending.insert(task, attempt) == Ok(None), "one workspace flight per task");
    out.push(Request::Now(Box::new(Now::WorkspaceRequest { task, attempt, context })));
    out.push(Request::Decided);
    Requests::Out(out)
}

fn workspace_prepared(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    task: u64,
    attempt: u64,
    writes: Option<Box<[authority::Write]>>,
) -> Requests {
    let mut out = Queue::with_capacity(2);
    if core.workspace_pending.remove(&task) != Some(attempt) {
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let Some(context) = core.contexts.remove(&task) else {
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    drop(core.dependency_results.remove(&task));
    let transcript = core.transcripts.remove(&task).expect("rendered task owns loaded transcript");
    let Some(writes) = writes else {
        work.push(Event::Tasks(tasks::Event::Hold { task, why: tasks::Hold::Effects }));
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    if core.run_admission(&context, env.wall, writes) != crate::RunAdmission::Allow {
        work.push(Event::Tasks(tasks::Event::Hold { task, why: tasks::Hold::Effects }));
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let Some(grant) = core.accounts.grant(core.settings.account, env.now) else {
        work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    if !core.proofs.contains_key(&task) && core.proofs.len() == core.proofs.capacity() {
        work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
        out.push(Request::Now(Box::new(Now::RunPreparationFailed { task })));
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let resume = core.settings.run.resume
        && context.tries.transient == 0
        && transcript.bytes <= u64::from(core.settings.resume_bytes);
    let transcript_from = if resume { transcript.from } else { attempt };
    let offered = if context.last_message == 0 { None } else { Some(context.last_message) };
    assert!(
        core.proofs.insert(task, RunProof::claimed(task, attempt, transcript_from, offered)).is_ok(),
        "claim proof reserved before child mutation"
    );
    let turns = if resume { core.resumed_turns(transcript) } else { Box::new([]) };
    let mut answered = List::with_capacity(env.limits.call_records);
    let mut after = None;
    for _ in 0..core.call_parts.len() {
        let mut next = None;
        for (&key, _) in &core.call_parts {
            if key.task == task
                && key.attempt < attempt
                && match after {
                    Some(previous) => key > previous,
                    None => true,
                }
                && match next {
                    Some(previous) => key < previous,
                    None => true,
                }
            {
                next = Some(key);
            }
        }
        let Some(key) = next else { break };
        answered.push(key).expect("retained call bound");
        after = Some(key);
    }
    let mut policy = core.settings.run.clone();
    policy.resume = resume;
    let run = RunCharter {
        policy,
        contract: context.contract,
        authority: context.authority,
        budget: authority::left(crate::translate::authority_numbers(context.numbers))
            .min(core.authority.rules().maximum_run_spend),
    };
    assert!(core.claiming.insert(task, attempt) == Ok(None), "one pending claim per task");
    out.push(Request::Now(Box::new(Now::RunPrepared {
        task,
        attempt,
        charter: core.settings.charter,
        run: Box::new(run),
        inbox: context.inbox,
        saved: context.saved,
        transcript: turns,
        answered: answered.into_boxed(),
        grant,
    })));
    out.push(Request::Decided);
    Requests::Out(out)
}

fn start_brief(core: &mut Core, env: &Env<Limits>, work: &mut Queue<Event>, task: u64) -> Requests {
    let mut out = Queue::with_capacity(1);
    let Some(context) = core.contexts.get(&task) else {
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    let Some(current) = core.tasks.task(task) else {
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    if current.phase != tasks::Phase::Active(tasks::Active::Preparing) || current.last_message != context.last_message {
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    if core.notes.busy() {
        core.pending_note_briefs.insert(task, true).expect("brief count bounds waiting notes");
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let mut scopes = List::with_capacity(env.limits.notes.scopes);
    scopes.push(notes::Scope::Goal { project: context.project, goal: current.root }).expect("goal scope room");
    scopes.push(notes::Scope::Project { project: context.project }).expect("project scope room");
    scopes.push(notes::Scope::Deployment).expect("deployment scope room");
    for resource in &context.authority.note_resources {
        let last = match &resource.pattern.last {
            tasks::Last::Exact(value) => notes::Last::Exact(value.clone()),
            tasks::Last::Open(value) => notes::Last::Open(value.clone()),
        };
        scopes
            .push(notes::Scope::Resources {
                project: context.project,
                connector: resource.connector,
                pattern: notes::Pattern { segments: resource.pattern.segments.clone(), last },
            })
            .expect("admitted note resource scope room");
    }
    let owner = core.begin_note_route(NoteRoute::Brief { task });
    work.push(Event::Notes(notes::Event::Index { owner, scopes, most: env.limits.notes.lines }));
    out.push(Request::Decided);
    Requests::Out(out)
}

fn plan_brief(
    core: &Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    task: u64,
    lines: List<notes::Line>,
    more: u32,
) -> Requests {
    let mut out = Queue::with_capacity(2);
    let Some(context) = core.contexts.get(&task) else {
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    let Some(current) = core.tasks.task(task) else {
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    if current.phase != tasks::Phase::Active(tasks::Active::Preparing) || current.last_message != context.last_message {
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let text_limits = crate::BriefTextLimits {
        parts: env.limits.brief_parts,
        read_bytes: env.limits.brief.read_bytes,
        brief_bytes: env.limits.brief.brief_bytes,
    };
    let mut wanted = List::with_capacity(env.limits.brief.sections);
    let Some(task_text) = crate::read_brief_part(core, text_limits, task, crate::BriefPart::Spec) else {
        work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
        out.push(Request::Decided);
        return Requests::Out(out);
    };
    wanted
        .push(brief::Planned::Core {
            kind: brief::Core::Task,
            text: task_text,
            limit: env.limits.brief_core_budgets.task,
            priority: 0,
            required: true,
        })
        .expect("task brief room");
    if !context.dependencies.is_empty() || !context.spec.inputs.is_empty() {
        let Some(text) = crate::read_brief_part(core, text_limits, task, crate::BriefPart::Dependencies) else {
            work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
            out.push(Request::Decided);
            return Requests::Out(out);
        };
        wanted
            .push(brief::Planned::Core {
                kind: brief::Core::Results,
                text,
                limit: env.limits.brief_core_budgets.dependencies,
                priority: 1,
                required: true,
            })
            .expect("dependency result section room");
    }
    if core.transcript_oversized(task) || context.tries.transient != 0 || !core.settings.run.resume {
        let Some(text) = crate::read_brief_part(core, text_limits, task, crate::BriefPart::TranscriptTail) else {
            work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
            out.push(Request::Decided);
            return Requests::Out(out);
        };
        wanted
            .push(brief::Planned::Core {
                kind: brief::Core::TranscriptTail,
                text,
                limit: env.limits.brief_core_budgets.task,
                priority: 2,
                required: true,
            })
            .expect("tail brief room");
    }
    plan_note_index(&mut wanted, &env.limits, &lines, more);
    if !context.delegates.is_empty()
        && wanted.room() > 0
        && let Some(text) = crate::read_brief_part(core, text_limits, task, crate::BriefPart::Delegates)
    {
        wanted
            .push(brief::Planned::Core {
                kind: brief::Core::Plan,
                text,
                limit: env.limits.brief_core_budgets.plan,
                priority: 3,
                required: false,
            })
            .expect("delegate section room");
    }
    if context.tries != tasks::Tries::NONE
        && wanted.room() > 0
        && let Some(text) = crate::read_brief_part(core, text_limits, task, crate::BriefPart::Attempts)
    {
        wanted
            .push(brief::Planned::Core {
                kind: brief::Core::Attempts,
                text,
                limit: env.limits.brief_core_budgets.attempts,
                priority: 4,
                required: false,
            })
            .expect("attempt brief room");
    }
    let parent = brief_parent(context.requester);
    out.push(Request::Now(Box::new(Now::BriefCorePlanned { task, parent, sections: wanted })));
    out.push(Request::Decided);
    Requests::Out(out)
}

fn brief_parent(requester: tasks::Party) -> Option<u64> {
    match requester {
        tasks::Party::Task(parent) => Some(parent),
        tasks::Party::Person(_) | tasks::Party::Deployment { .. } => None,
    }
}

fn plan_note_index(wanted: &mut List<brief::Planned>, limits: &Limits, lines: &List<notes::Line>, more: u32) {
    if wanted.room() > 0 {
        wanted
            .push(brief::Planned::Core {
                kind: brief::Core::NotesIndex,
                text: crate::note_index_text(lines, more, limits.brief.read_bytes.min(limits.brief_core_budgets.notes)),
                limit: limits.brief_core_budgets.notes,
                priority: 4,
                required: false,
            })
            .expect("note index section room");
    }
}

fn start_recurring(
    core: &mut Core,
    work: &mut Queue<Event>,
    project: u32,
    authority: tasks::Authority,
    template: tasks::RecurringTemplate,
) {
    if !core.recurring_admit(project, &authority, &template) {
        return;
    }
    let period = core.settings.period;
    let to = ReplyTo::new(Token::new(u64::MAX - 2));
    if core.tasks.funding(tasks::Funder::Period { project, period }).is_none() {
        work.push(Event::Tasks(tasks::Event::OpenPeriod {
            reply_to: ReplyTo::new(Token::new(u64::MAX - 2)),
            project,
            period,
            budget: core.settings.period_budget,
        }));
    }
    let Some(number) = crate::fresh(&mut core.counters, crate::Family::Task) else { return };
    work.push(Event::Tasks(tasks::Event::Make {
        reply_to: to,
        creator: tasks::Party::Deployment { project },
        batch: Box::new([tasks::New {
            number,
            project,
            executor: tasks::Executor::Procedure { connector: core.settings.recurring_connector, code: 1 },
            spec: tasks::Spec { words: b"recurring".as_slice().into(), parameters: Box::new([]), inputs: Box::new([]) },
            contract: tasks::Contract::Report { words: 0 },
            numbers: tasks::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 },
            authority,
            funder: tasks::Funder::Period { project, period },
            dependencies: Box::new([]),
            holdings: Box::new([]),
            wake: tasks::WakePolicy::DEFAULT,
            recurring: Some(Box::new(template)),
            tracked: None,
        }]),
    }));
    work.push(Event::Tasks(tasks::Event::TickRecurring { task: number, period }));
}

#[expect(clippy::too_many_lines, reason = "one exhaustive authenticated task proposal dispatcher")]
fn named_proposal_accept(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    to: ReplyTo,
    key: CallKey,
    proposal: tasks::Proposal,
) -> Requests {
    let result_followups = core.tasks.result_proposal(proposal.proposer, proposal.number);
    if let Err(why) =
        core.proposal_accept_allowed(&env.limits, proposal.proposer, key.task, proposal.project, &proposal.action)
    {
        return proposal_refused(core, to, key, why);
    }
    match proposal.action {
        tasks::ProposalAction::Effect { connector, .. } => {
            assert!(core.pending_calls.insert(key, true).is_ok(), "effect acceptance call room");
            let owner = to.into_token();
            return crate::effects::start(
                core,
                env,
                owner,
                connector,
                crate::EffectOrigin::Accept {
                    to: ReplyTo::new(owner),
                    key: Some(key),
                    proposal: Box::new(proposal),
                    by: tasks::Party::Task(key.task),
                },
            );
        }
        tasks::ProposalAction::Batch(_)
        | tasks::ProposalAction::Amend { .. }
        | tasks::ProposalAction::Widen { .. }
        | tasks::ProposalAction::Release { .. } => {}
    }
    let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
        return proposal_refused(core, to, key, tasks::Refusal::Busy);
    };
    let token = to.into_token();
    let event = match proposal.action {
        tasks::ProposalAction::Effect { .. } => unreachable!("effect accepted through its connector"),
        tasks::ProposalAction::Batch(mut batch) => {
            let creator =
                if proposal.as_holder { tasks::Party::Task(key.task) } else { tasks::Party::Task(proposal.proposer) };
            for member in &mut batch {
                member.funder = tasks::Funder::Task(key.task);
            }
            if result_followups {
                tasks::Event::MakeResultFollowups {
                    reply_to: ReplyTo::new(token),
                    proposer: proposal.proposer,
                    proposal: proposal.number,
                    batch,
                }
            } else {
                tasks::Event::Make { reply_to: ReplyTo::new(token), creator, batch }
            }
        }
        tasks::ProposalAction::Amend { task, amendment } => {
            let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                return proposal_refused(core, ReplyTo::new(token), key, tasks::Refusal::Busy);
            };
            let Some(current) = core.tasks.delegation(task) else {
                return proposal_refused(core, ReplyTo::new(token), key, tasks::Refusal::Unknown);
            };
            let after =
                crate::translate::authority_value(amendment.authority.as_ref().expect("proposal amendment authority"));
            let before = crate::translate::authority_value(&current.authority);
            let stop_run = !authority::at_most(&before, &after, &core.authority.rules().implies);
            tasks::Event::Amend {
                reply_to: ReplyTo::new(token),
                by: tasks::Party::Task(key.task),
                task,
                message,
                stop_run,
                amendment,
            }
        }
        tasks::ProposalAction::Widen { task, authority } => {
            let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                return proposal_refused(core, ReplyTo::new(token), key, tasks::Refusal::Busy);
            };
            tasks::Event::Amend {
                reply_to: ReplyTo::new(token),
                by: tasks::Party::Task(key.task),
                task,
                message,
                stop_run: false,
                amendment: tasks::Amendment {
                    spec: None,
                    wake: None,
                    dependencies: None,
                    authority: Some(authority),
                    reason: proposal.reason,
                },
            }
        }
        tasks::ProposalAction::Release { task } => tasks::Event::Control {
            reply_to: ReplyTo::new(token),
            by: tasks::Party::Task(key.task),
            task,
            control: tasks::Control::Release,
        },
    };
    assert!(core.pending_calls.insert(key, true).is_ok(), "proposal acceptance room reserved");
    assert!(
        core.routing_calls.insert(
            token,
            RoutedCall::Accepting { key, proposer: proposal.proposer, proposal: proposal.number, message }
        ) == Ok(None),
        "one acceptance route"
    );
    work.push(Event::Tasks(event));
    let mut out = Queue::with_capacity(1);
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(clippy::too_many_arguments, reason = "one named proposal decision carries its call and proposal identity")]
fn named_proposal_decide(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    to: ReplyTo,
    key: CallKey,
    proposer: u64,
    proposal: u64,
    choice: ProposalChoice,
) -> Requests {
    let Some(pending) = core.tasks.proposal(proposer, proposal) else {
        return proposal_refused(core, to, key, tasks::Refusal::Unknown);
    };
    let current = match pending.state {
        tasks::ProposalState::Pending { holder, .. } => holder,
        tasks::ProposalState::Accepted { .. }
        | tasks::ProposalState::Rejected { .. }
        | tasks::ProposalState::Withdrawn => {
            return proposal_refused(core, to, key, tasks::Refusal::State);
        }
    };
    if current != tasks::ProposalHolder::Task(key.task) {
        return proposal_refused(core, to, key, tasks::Refusal::Reference);
    }
    let next = match choice {
        ProposalChoice::Accept => return named_proposal_accept(core, env, work, to, key, pending),
        ProposalChoice::Reject { reason } => tasks::ProposalDecision::Reject { reason },
        ProposalChoice::Pass => {
            let Some(holder) = core.proposal_holder(&env.limits, proposer, &pending.action, Some(current)) else {
                return proposal_refused(core, to, key, tasks::Refusal::Reference);
            };
            tasks::ProposalDecision::Pass { holder }
        }
    };
    let message = match next {
        tasks::ProposalDecision::Reject { .. } => match crate::fresh(&mut core.counters, crate::Family::Message) {
            Some(message) => Some(message),
            None => return proposal_refused(core, to, key, tasks::Refusal::Busy),
        },
        tasks::ProposalDecision::Pass { .. } => None,
        tasks::ProposalDecision::Accept => unreachable!("accept routed through action"),
    };
    let token = to.into_token();
    assert!(core.pending_calls.insert(key, true).is_ok(), "proposal decision room reserved");
    assert!(core.routing_calls.insert(token, RoutedCall::Decide { key, proposal }) == Ok(None), "one route");
    work.push(Event::Tasks(tasks::Event::DecideProposal {
        reply_to: ReplyTo::new(token),
        proposer,
        proposal,
        message,
        by: tasks::Party::Task(key.task),
        decision: next,
    }));
    let mut out = Queue::with_capacity(1);
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(clippy::too_many_arguments, reason = "one named proposal retains its action and call correlation")]
pub(crate) fn named_propose(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    to: ReplyTo,
    key: CallKey,
    action: tasks::ProposalAction,
    reason: Box<[u8]>,
    as_holder: bool,
) -> Requests {
    if reason.len() > usize::try_from(env.limits.tasks.message_bytes).expect("u32 fits usize") {
        return proposal_refused(core, to, key, tasks::Refusal::Read);
    }
    let Some(context) = core.tasks.delegation(key.task) else {
        return proposal_refused(core, to, key, tasks::Refusal::Unknown);
    };
    if let tasks::ProposalAction::Amend { amendment, .. } = &action
        && amendment.authority.is_none()
    {
        return proposal_refused(core, to, key, tasks::Refusal::AuthorityShape);
    }
    if let Err(why) = core.proposal_admit_action(context.project, &action) {
        return proposal_refused(core, to, key, why);
    }
    let Some(holder) = core.proposal_holder(&env.limits, key.task, &action, None) else {
        return proposal_refused(core, to, key, tasks::Refusal::Reference);
    };
    let Some(number) = crate::fresh(&mut core.counters, crate::Family::Message) else {
        return proposal_refused(core, to, key, tasks::Refusal::Busy);
    };
    let token = to.into_token();
    assert!(core.pending_calls.insert(key, true).is_ok(), "proposal call room reserved");
    assert!(core.routing_calls.insert(token, RoutedCall::Propose { key, proposal: number }) == Ok(None), "one route");
    work.push(Event::Tasks(tasks::Event::Propose {
        reply_to: ReplyTo::new(token),
        proposal: tasks::Proposal {
            number,
            proposer: key.task,
            project: context.project,
            action,
            reason,
            as_holder,
            state: tasks::ProposalState::Pending { holder, since: env.wall },
        },
    }));
    let mut out = Queue::with_capacity(1);
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(clippy::too_many_arguments, reason = "one named holder choice carries its call, task and revision")]
fn named_escalation(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    to: ReplyTo,
    key: CallKey,
    task: u64,
    revision: u64,
    choice: EscalationChoice,
) -> Requests {
    let Some(context) = core.tasks.escalation(task) else {
        return escalation_refused(core, to, key, task, tasks::Refusal::Unknown);
    };
    let (current, holder) = match context.escalation {
        tasks::Escalation::Waiting { revision, holder, .. } => (revision, holder),
        tasks::Escalation::Unheld { .. } | tasks::Escalation::Routing { .. } | tasks::Escalation::Rejected { .. } => {
            return escalation_refused(core, to, key, task, tasks::Refusal::State);
        }
    };
    if current != revision || holder != tasks::EscalationHolder::Task(key.task) {
        return escalation_refused(core, to, key, task, tasks::Refusal::Reference);
    }
    let role = core.people.role(context.requester, context.project);
    let semantic = match choice {
        EscalationChoice::Release => {
            if core.escalation_recipient(&env.limits, &context, role) != Some(holder) {
                return escalation_refused(core, to, key, task, tasks::Refusal::Funding);
            }
            tasks::EscalationDecision::Release
        }
        EscalationChoice::Reject { reason } => tasks::EscalationDecision::Reject { reason },
        EscalationChoice::Pass => {
            let Some(next) = core.escalation_recipient_after(&env.limits, &context, role, Some(holder)) else {
                return escalation_refused(core, to, key, task, tasks::Refusal::Reference);
            };
            tasks::EscalationDecision::Pass { holder: next }
        }
    };
    let entry = match semantic {
        tasks::EscalationDecision::Pass { .. } => match crate::fresh(&mut core.counters, crate::Family::Message) {
            Some(entry) => Some(entry),
            None => return escalation_refused(core, to, key, task, tasks::Refusal::Busy),
        },
        tasks::EscalationDecision::Release | tasks::EscalationDecision::Reject { .. } => None,
    };
    let token = to.into_token();
    assert!(core.pending_calls.insert(key, true).is_ok(), "task escalation call room reserved");
    assert!(core.routing_calls.insert(token, RoutedCall::Escalation { key, task, revision }) == Ok(None), "one route");
    work.push(Event::TaskEscalation(tasks::Event::DecideEscalation {
        reply_to: ReplyTo::new(token),
        task,
        revision,
        by: key.task,
        entry,
        decision: semantic,
    }));
    let mut out = Queue::with_capacity(1);
    out.push(Request::Decided);
    Requests::Out(out)
}

#[expect(clippy::too_many_lines, reason = "one generic named-call vocabulary routes its finite task continuations")]
fn named_action(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    to: ReplyTo,
    key: CallKey,
    action: NamedAction,
) -> Requests {
    assert!(core.current_proof(key.task, key.attempt), "named action belongs to current claim");
    if let Some(part) = core.authorize_tool(key, named_tool_kind(&action)) {
        let mut out = Queue::with_capacity(3);
        call_decision(core, &mut out, to, key, part);
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let token = to.into_token();
    let mut out = Queue::with_capacity(3);
    match action {
        NamedAction::Message { target, kind, words } => {
            let Some(context) = core.tasks.delegation(key.task) else {
                call_decision(
                    core,
                    &mut out,
                    ReplyTo::new(token),
                    key,
                    CallPart::MessageRefused(tasks::Problem {
                        task: Some(key.task),
                        why: tasks::Refusal::Unknown,
                        blocked_by: None,
                    }),
                );
                out.push(Request::Decided);
                return Requests::Out(out);
            };
            let project = context.project;
            let Some(number) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                call_decision(
                    core,
                    &mut out,
                    ReplyTo::new(token),
                    key,
                    CallPart::MessageRefused(tasks::Problem {
                        task: Some(target),
                        why: tasks::Refusal::Busy,
                        blocked_by: None,
                    }),
                );
                out.push(Request::Decided);
                return Requests::Out(out);
            };
            assert!(core.pending_calls.insert(key, true).is_ok(), "call record room reserved");
            assert!(core.routing_calls.insert(token, RoutedCall::Message(key)) == Ok(None), "one live routed call");
            work.push(Event::Tasks(tasks::Event::Message {
                reply_to: ReplyTo::new(token),
                project,
                task: target,
                word: tasks::Word {
                    number,
                    from: tasks::Party::Task(key.task),
                    kind,
                    words,
                    at: env.wall,
                    hits: 1,
                    eligible: false,
                },
            }));
        }
        NamedAction::Introduce { left, right } => {
            assert!(core.pending_calls.insert(key, true).is_ok(), "call record room reserved");
            assert!(core.routing_calls.insert(token, RoutedCall::Introduce(key)) == Ok(None), "one live routed call");
            work.push(Event::Tasks(tasks::Event::Introduce {
                reply_to: ReplyTo::new(token),
                by: key.task,
                left,
                right,
            }));
        }
        NamedAction::Subscribe { kind } => {
            let Some(subscription) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                call_decision(
                    core,
                    &mut out,
                    ReplyTo::new(token),
                    key,
                    CallPart::SubscriptionRefused(tasks::Problem {
                        task: Some(key.task),
                        why: tasks::Refusal::Busy,
                        blocked_by: None,
                    }),
                );
                out.push(Request::Decided);
                return Requests::Out(out);
            };
            assert!(core.pending_calls.insert(key, true).is_ok(), "call record room reserved");
            assert!(
                core.routing_calls.insert(token, RoutedCall::Subscribe { key, subscription }) == Ok(None),
                "one live routed call"
            );
            work.push(Event::Tasks(tasks::Event::Subscribe {
                reply_to: ReplyTo::new(token),
                task: key.task,
                subscription: tasks::Subscription { number: subscription, kind },
            }));
        }
        NamedAction::Unsubscribe { subscription } => {
            assert!(core.pending_calls.insert(key, true).is_ok(), "call record room reserved");
            assert!(core.routing_calls.insert(token, RoutedCall::Unsubscribe(key)) == Ok(None), "one live routed call");
            work.push(Event::Tasks(tasks::Event::Unsubscribe {
                reply_to: ReplyTo::new(token),
                task: key.task,
                subscription,
            }));
        }
        NamedAction::Control { target, control } => {
            assert!(core.pending_calls.insert(key, true).is_ok(), "call record room reserved");
            assert!(core.routing_calls.insert(token, RoutedCall::Control(key)) == Ok(None), "one live routed call");
            work.push(Event::Tasks(tasks::Event::Control {
                reply_to: ReplyTo::new(token),
                by: tasks::Party::Task(key.task),
                task: target,
                control,
            }));
        }
        NamedAction::Amend { target, amendment } => {
            let stop_run = match core.task_amend_admit(key.task, target, &amendment, env.limits.tasks.depth) {
                Ok(stop_run) => stop_run,
                Err(crate::TaskAmendDenied::Refused { task, why }) => {
                    call_decision(
                        core,
                        &mut out,
                        ReplyTo::new(token),
                        key,
                        CallPart::ControlRefused(tasks::Problem { task: Some(task), why, blocked_by: None }),
                    );
                    out.push(Request::Decided);
                    return Requests::Out(out);
                }
                Err(crate::TaskAmendDenied::Denied(answer)) => {
                    call_decision(core, &mut out, ReplyTo::new(token), key, CallPart::ControlDenied { answer });
                    out.push(Request::Decided);
                    return Requests::Out(out);
                }
            };
            let Some(message) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                call_decision(
                    core,
                    &mut out,
                    ReplyTo::new(token),
                    key,
                    CallPart::ControlRefused(tasks::Problem {
                        task: Some(target),
                        why: tasks::Refusal::Busy,
                        blocked_by: None,
                    }),
                );
                out.push(Request::Decided);
                return Requests::Out(out);
            };
            assert!(core.pending_calls.insert(key, true).is_ok(), "call record room reserved");
            assert!(core.routing_calls.insert(token, RoutedCall::Control(key)) == Ok(None), "one amendment route");
            work.push(Event::Tasks(tasks::Event::Amend {
                reply_to: ReplyTo::new(token),
                by: tasks::Party::Task(key.task),
                task: target,
                message,
                stop_run,
                amendment,
            }));
        }
        NamedAction::DecideEscalation { task, revision, choice } => {
            return named_escalation(core, env, work, ReplyTo::new(token), key, task, revision, choice);
        }
        NamedAction::WithdrawProposal { proposal } => {
            if core.tasks.proposal(key.task, proposal).is_none() {
                return proposal_refused(core, ReplyTo::new(token), key, tasks::Refusal::Unknown);
            }
            assert!(core.pending_calls.insert(key, true).is_ok(), "proposal call room reserved");
            assert!(core.routing_calls.insert(token, RoutedCall::Withdraw { key, proposal }) == Ok(None), "one route");
            work.push(Event::Tasks(tasks::Event::WithdrawProposal {
                reply_to: ReplyTo::new(token),
                proposer: key.task,
                proposal,
            }));
        }
        NamedAction::DecideProposal { proposer, proposal, choice } => {
            return named_proposal_decide(core, env, work, ReplyTo::new(token), key, proposer, proposal, choice);
        }
        NamedAction::Propose { action, reason, as_holder } => {
            return named_propose(core, env, work, ReplyTo::new(token), key, action, reason, as_holder);
        }
        NamedAction::ProposeBatch { batch, reason, as_holder } => {
            return propose_batch(core, env, ReplyTo::new(token), key, batch, reason, as_holder);
        }
        NamedAction::Note { mut entry, recalled } => {
            if recalled.is_none() && entry.name != 0 {
                call_decision(core, &mut out, ReplyTo::new(token), key, CallPart::NoteRefused(notes::Refusal::Exists));
                out.push(Request::Decided);
                return Requests::Out(out);
            }
            if recalled.is_some() && entry.name == 0 {
                call_decision(core, &mut out, ReplyTo::new(token), key, CallPart::NoteRefused(notes::Refusal::Missing));
                out.push(Request::Decided);
                return Requests::Out(out);
            }
            if entry.name == 0 {
                let Some(name) = crate::fresh(&mut core.counters, crate::Family::Message) else {
                    call_decision(
                        core,
                        &mut out,
                        ReplyTo::new(token),
                        key,
                        CallPart::NoteRefused(notes::Refusal::Full),
                    );
                    out.push(Request::Decided);
                    return Requests::Out(out);
                };
                entry.name = name;
            }
            entry.author = notes::Author::Task { task: key.task, attempt: key.attempt };
            assert!(core.pending_calls.insert(key, true) == Ok(None), "note call record room reserved");
            let owner = core.begin_note_route(NoteRoute::Call { to: ReplyTo::new(token), key });
            work.push(Event::Notes(notes::Event::Write { owner, entry, recalled }));
        }
        NamedAction::Recall { by, page } => {
            assert!(core.pending_calls.insert(key, true) == Ok(None), "recall call record room reserved");
            let owner = core.begin_note_route(NoteRoute::Call { to: ReplyTo::new(token), key });
            work.push(Event::Notes(notes::Event::Recall { owner, by, page }));
        }
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn named_tool_kind(action: &NamedAction) -> ToolKind {
    match action {
        NamedAction::Message { target, .. } => ToolKind::Message { target: *target },
        NamedAction::Introduce { left, .. } => ToolKind::Message { target: *left },
        NamedAction::Subscribe { .. } | NamedAction::Unsubscribe { .. } => ToolKind::Subscribe,
        NamedAction::Control { .. } | NamedAction::Amend { .. } => ToolKind::Control,
        NamedAction::DecideEscalation { .. }
        | NamedAction::WithdrawProposal { .. }
        | NamedAction::DecideProposal { .. } => ToolKind::Decide,
        NamedAction::Propose { .. } | NamedAction::ProposeBatch { .. } => ToolKind::Propose,
        NamedAction::Note { entry, .. } => ToolKind::Note { scope: entry.scope.clone() },
        NamedAction::Recall { by, .. } => ToolKind::Recall { by: by.clone() },
    }
}

#[expect(clippy::too_many_lines, reason = "one exhaustive party vocabulary routes its bounded child decision")]
fn tag_people(
    core: &mut Core,
    env: &Env<Limits>,
    mut child: Queue<people::Request>,
    work: &mut Queue<Event>,
) -> Requests {
    let room = people::max_out(&env.limits.people).checked_mul(4).expect("bounded party routes");
    let mut pending = Queue::with_capacity(room);
    for _ in 0..child.len() {
        pending.push(child.pop().expect("party output count"));
    }
    let mut out = Queue::with_capacity(room.checked_add(1).expect("party output room"));
    for _ in 0..room {
        let Some(next) = pending.pop() else { break };
        let request = match next {
            people::Request::ServiceMade { request, outcome } => {
                let mut follow = Queue::with_capacity(people::max_out(&env.limits.people));
                people::step(
                    &mut core.people,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.people },
                    people::Event::Decided { request, outcome },
                    &mut follow,
                );
                for _ in 0..follow.len() {
                    pending.push(follow.pop().expect("service output count"));
                }
                continue;
            }
            people::Request::Save { record } => Request::Write(Write::Save(Record::People(record))),
            people::Request::Erase { key } => Request::Write(Write::Erase(Key::People(key))),
            people::Request::Reply { to, reply } => {
                Request::Held(Box::new(Held::PeopleReply { to, sign_in: core.signing_in, reply }))
            }
            people::Request::RolesApplied { .. } => continue,
            people::Request::RolesRefused { .. } | people::Request::RestoreRefused { .. } => {
                Request::Now(Box::new(Now::RestoreRefused))
            }
            people::Request::Route { request, person, project, role, ask } => {
                if let people::Ask::TakePerson { .. }
                | people::Ask::HandBackPerson { .. }
                | people::Ask::AnswerPerson { .. } = &*ask
                {
                    match core.person_task(request, person, project, role, *ask) {
                        Ok(event) => work.push(Event::Tasks(event)),
                        Err(why) => work.push(Event::People(people::Event::Decided {
                            request,
                            outcome: people::Outcome::Refused(why),
                        })),
                    }
                    continue;
                }
                if let people::Ask::Stop { .. } = &*ask {
                    match core.control_admit(
                        &ask,
                        person,
                        project,
                        role,
                        env.limits.tasks.depth,
                        env.limits.tasks.result_bytes,
                    ) {
                        Ok(task) => {
                            work.push(Event::Tasks(tasks::Event::Hold {
                                task,
                                why: tasks::Hold::StoppedBy { party: person },
                            }));
                            work.push(Event::People(people::Event::Decided {
                                request,
                                outcome: people::Outcome::Stopped { task },
                            }));
                        }
                        Err(why) => work.push(Event::People(people::Event::Decided {
                            request,
                            outcome: people::Outcome::Refused(why),
                        })),
                    }
                    continue;
                }
                if let people::Ask::Cancel { .. } = &*ask {
                    let admitted = core.control_admit(
                        &ask,
                        person,
                        project,
                        role,
                        env.limits.tasks.depth,
                        env.limits.tasks.result_bytes,
                    );
                    let people::Ask::Cancel { reason, .. } = *ask else { unreachable!("cancel route retains its ask") };
                    match admitted {
                        Ok(task) => {
                            assert!(
                                core.person_tasks.insert(request, PersonTaskRoute::Cancel(task)) == Ok(None),
                                "one cancel route"
                            );
                            work.push(Event::Tasks(tasks::Event::Control {
                                reply_to: ReplyTo::new(request),
                                by: tasks::Party::Person(person),
                                task,
                                control: tasks::Control::Cancel { reason },
                            }));
                        }
                        Err(why) => work.push(Event::People(people::Event::Decided {
                            request,
                            outcome: people::Outcome::Refused(why),
                        })),
                    }
                    continue;
                }
                if let people::Ask::Release { .. } = &*ask {
                    match core.control_admit(
                        &ask,
                        person,
                        project,
                        role,
                        env.limits.tasks.depth,
                        env.limits.tasks.result_bytes,
                    ) {
                        Ok(task) => {
                            assert!(
                                core.person_tasks.insert(request, PersonTaskRoute::Release(task)) == Ok(None),
                                "one release route"
                            );
                            work.push(Event::Tasks(tasks::Event::Control {
                                reply_to: ReplyTo::new(request),
                                by: tasks::Party::Person(person),
                                task,
                                control: tasks::Control::Release,
                            }));
                        }
                        Err(why) => work.push(Event::People(people::Event::Decided {
                            request,
                            outcome: people::Outcome::Refused(why),
                        })),
                    }
                    continue;
                }
                if let people::Ask::MakeService { role: service_role, .. } = &*ask {
                    let outcome = match core.role_allowed(person, project, env.limits.tasks.tree_tasks) {
                        Ok(()) if core.authority.role(project, service_role.number()).is_some() => {
                            match crate::fresh(&mut core.counters, crate::Family::Person) {
                                Some(candidate) => {
                                    let mut follow = Queue::with_capacity(people::max_out(&env.limits.people));
                                    people::step(
                                        &mut core.people,
                                        &Env { now: env.now, wall: env.wall, limits: env.limits.people },
                                        people::Event::MakeService { request, person: candidate },
                                        &mut follow,
                                    );
                                    for _ in 0..follow.len() {
                                        pending.push(follow.pop().expect("service output count"));
                                    }
                                    None
                                }
                                None => Some(people::Outcome::Refused(people::Refusal::Limit)),
                            }
                        }
                        Ok(()) => Some(people::Outcome::Refused(people::Refusal::Unknown)),
                        Err(refusal) => Some(people::Outcome::Refused(refusal)),
                    };
                    if let Some(outcome) = outcome {
                        let mut follow = Queue::with_capacity(people::max_out(&env.limits.people));
                        people::step(
                            &mut core.people,
                            &Env { now: env.now, wall: env.wall, limits: env.limits.people },
                            people::Event::Decided { request, outcome },
                            &mut follow,
                        );
                        for _ in 0..follow.len() {
                            pending.push(follow.pop().expect("service decision count"));
                        }
                    }
                    continue;
                }
                let Some(ask) = route_person_note(core, work, request, person, project, role, ask) else {
                    continue;
                };
                if let people::Ask::Adopt { .. } = &*ask {
                    match core.role_allowed(person, project, env.limits.tasks.tree_tasks) {
                        Ok(()) => {
                            let people::Ask::Adopt { adoption, .. } = *ask else {
                                unreachable!("adoption route retains its ask")
                            };
                            out.push(Request::Ask {
                                connector: adoption.resource.connector,
                                ask: Ask::Adopt { request, project, adoption },
                            });
                        }
                        Err(why) => work.push(Event::People(people::Event::Decided {
                            request,
                            outcome: people::Outcome::Refused(why),
                        })),
                    }
                    continue;
                }
                if let people::Ask::SetPool { person: beneficiary, budget, .. } = &*ask {
                    pool_route(core, env, work, request, person, project, *beneficiary, *budget);
                    continue;
                }
                if let people::Ask::DecideProposal { proposer, proposal, .. } = &*ask
                    && core.tasks.person_proposal(*proposer, *proposal).is_some()
                {
                    let people::Ask::DecideProposal { proposer, proposal, decision, .. } = *ask else {
                        unreachable!("goal route retains its decision")
                    };
                    goal_decide(core, env, work, request, person, role, project, proposer, proposal, decision);
                    continue;
                }
                if let people::Ask::DecideProposal { proposer, proposal, .. } = &*ask
                    && core.tasks.proposal(*proposer, *proposal).is_some()
                {
                    let people::Ask::DecideProposal { proposer, proposal, decision, .. } = *ask else {
                        unreachable!("proposal route retains its choice")
                    };
                    person_proposal_decide(
                        core, env, work, request, person, role, project, proposer, proposal, decision,
                    );
                    continue;
                }
                if let people::Ask::DecideProposal { proposer, proposal, .. } = &*ask {
                    out.push(Request::Now(Box::new(Now::HistoricalProposal {
                        request,
                        person,
                        project,
                        proposer: *proposer,
                        proposal: *proposal,
                    })));
                    continue;
                }
                if let people::Ask::SetRoles { .. } = &*ask {
                    let people::Ask::SetRoles { holdings, .. } = *ask else {
                        unreachable!("role route retains its roster")
                    };
                    roles_route(core, env, work, &mut out, request, person, project, holdings);
                    continue;
                }
                if let people::Ask::Amend { .. } = &*ask {
                    let people::Ask::Amend { task, amendment, .. } = *ask else {
                        unreachable!("amendment route retains its typed payload")
                    };
                    amend_route(core, env, work, request, person, role, project, task, amendment);
                    continue;
                }
                if let people::Ask::ChangePolicy { .. } = &*ask {
                    let people::Ask::ChangePolicy { change, .. } = *ask else {
                        unreachable!("policy route retains its typed edit")
                    };
                    if work.room() < env.limits.tasks.tasks.checked_add(1).expect("task recheck bound")
                        || out.room() < 2
                    {
                        pool_refused(work, request, people::Refusal::Busy);
                        continue;
                    }
                    match core.change_policy(&env.limits, person, project, change) {
                        Ok(snapshot) => {
                            out.push(Request::Write(Write::Save(Record::People(people::Stored::Policy {
                                project,
                                value: snapshot,
                            }))));
                            let mut reroutes = core.proposal_recheck_project(&env.limits, project);
                            for _ in 0..reroutes.len() {
                                work.push(Event::Tasks(reroutes.pop().expect("policy holder recheck")));
                            }
                            work.push(Event::People(people::Event::Decided {
                                request,
                                outcome: people::Outcome::PolicyChanged { project },
                            }));
                        }
                        Err(why) => pool_refused(work, request, why),
                    }
                    continue;
                }
                if let people::Ask::DecideEscalation { task, revision, .. } = &*ask
                    && current_person_escalation(core, *task, *revision)
                {
                    let people::Ask::DecideEscalation { task, revision, decision, .. } = *ask else {
                        unreachable!("held choice route retains its exact revision")
                    };
                    person_escalation_decide(core, env, work, request, person, role, project, task, revision, decision);
                    continue;
                }
                if let people::Ask::DecideEscalation { .. } = &*ask {
                    let people::Ask::DecideEscalation { task, revision, decision, .. } = *ask else {
                        unreachable!("historical choice retains its exact revision")
                    };
                    if let people::EscalationDecision::Reject { reason } = &decision
                        && reason.len()
                            > usize::try_from(env.limits.escalation_reason_bytes).expect("reason bound fits usize")
                    {
                        pool_refused(work, request, people::Refusal::Limit);
                        continue;
                    }
                    if let Some(context) = core.tasks.escalation(task)
                        && (context.project != project || !core.escalation_visible(&context, person, role))
                    {
                        pool_refused(work, request, people::Refusal::Standing);
                        continue;
                    }
                    out.push(Request::Now(Box::new(Now::HistoricalEscalation {
                        request,
                        person,
                        project,
                        task,
                        revision,
                    })));
                    continue;
                }
                if let people::Ask::SetGoal { .. } = &*ask {
                    let people::Ask::SetGoal { spec, charter, budget, priority, .. } = *ask else {
                        unreachable!("goal route retains its task specification")
                    };
                    begin_goal_creation(
                        core,
                        env,
                        &mut out,
                        work,
                        request,
                        person,
                        project,
                        role.expect("goal membership admitted"),
                        spec,
                        charter,
                        budget,
                        priority,
                    );
                    continue;
                }
                if let people::Ask::StartChat { .. } = &*ask {
                    let people::Ask::StartChat { words, .. } = *ask else {
                        unreachable!("chat route retains its opening words")
                    };
                    begin_chat_creation(
                        core,
                        env,
                        &mut out,
                        work,
                        request,
                        person,
                        project,
                        role.expect("chat membership admitted"),
                        words,
                    );
                    continue;
                }
                match *ask {
                    people::Ask::Say { task, words, .. } => {
                        match core.person_message(crate::PersonMessage {
                            request,
                            person,
                            project,
                            task,
                            question: None,
                            words,
                            at: env.wall,
                        }) {
                            Ok(event) => work.push(Event::Tasks(event)),
                            Err(why) => work.push(Event::People(people::Event::Decided {
                                request,
                                outcome: people::Outcome::Refused(why),
                            })),
                        }
                        continue;
                    }
                    people::Ask::AnswerQuestion { task, question, words, .. } => {
                        match core.person_message(crate::PersonMessage {
                            request,
                            person,
                            project,
                            task,
                            question: Some(question),
                            words,
                            at: env.wall,
                        }) {
                            Ok(event) => work.push(Event::Tasks(event)),
                            Err(why) => work.push(Event::People(people::Event::Decided {
                                request,
                                outcome: people::Outcome::Refused(why),
                            })),
                        }
                        continue;
                    }
                    people::Ask::Prioritise { goals, .. } => {
                        match core.prioritise(request, person, project, role, goals, env.limits.tasks.tasks) {
                            Ok(event) => work.push(Event::Tasks(event)),
                            Err(why) => work.push(Event::People(people::Event::Decided {
                                request,
                                outcome: people::Outcome::Refused(why),
                            })),
                        }
                        continue;
                    }
                    people::Ask::Move { task, reason, .. } => {
                        match core.move_admit(
                            person,
                            role,
                            project,
                            task,
                            env.limits.tasks.tree_tasks,
                            env.limits.tasks.depth,
                        ) {
                            Ok(()) => {
                                assert!(
                                    core.moving.insert(request, task) == Ok(None),
                                    "one move flight per keyed request"
                                );
                                work.push(Event::Tasks(tasks::Event::Move {
                                    reply_to: ReplyTo::new(request),
                                    task,
                                    person,
                                    period: core.settings.period,
                                    pool_budget: core.settings.person_budget,
                                    period_budget: core.settings.period_budget,
                                    reason,
                                }));
                            }
                            Err(why) => work.push(Event::People(people::Event::Decided {
                                request,
                                outcome: people::Outcome::Refused(why),
                            })),
                        }
                        continue;
                    }
                    people::Ask::Watch { .. }
                    | people::Ask::EditNote { .. }
                    | people::Ask::MakeService { .. }
                    | people::Ask::Adopt { .. }
                    | people::Ask::SetGoal { .. }
                    | people::Ask::Stop { .. }
                    | people::Ask::Cancel { .. }
                    | people::Ask::Release { .. }
                    | people::Ask::TakePerson { .. }
                    | people::Ask::HandBackPerson { .. }
                    | people::Ask::AnswerPerson { .. }
                    | people::Ask::DecideProposal { .. }
                    | people::Ask::Amend { .. }
                    | people::Ask::SetRoles { .. }
                    | people::Ask::ChangePolicy { .. }
                    | people::Ask::SetPool { .. }
                    | people::Ask::DecideEscalation { .. }
                    | people::Ask::StartChat { .. } => unreachable!("the core routes every party request"),
                }
            }
        };
        out.push(request);
    }
    assert!(pending.is_empty(), "finite party routes fit their bound");
    out.push(Request::Decided);
    Requests::Out(out)
}

fn route_person_note(
    core: &mut Core,
    work: &mut Queue<Event>,
    request: Token,
    person: u64,
    project: u32,
    role: Option<people::Role>,
    ask: Box<people::Ask>,
) -> Option<Box<people::Ask>> {
    match *ask {
        people::Ask::EditNote { name, scope, change, .. } => {
            if core.note_authorized(project, role, &scope) {
                let owner = core.begin_note_route(NoteRoute::Person { request });
                let scope = person_note_scope(project, *scope);
                let change = person_note_change(*change);
                work.push(Event::Notes(notes::Event::Edit { owner, party: person, name, scope, change }));
            } else {
                work.push(Event::People(people::Event::Decided {
                    request,
                    outcome: people::Outcome::Refused(people::Refusal::Authority),
                }));
            }
            None
        }
        other @ (people::Ask::Watch { .. }
        | people::Ask::MakeService { .. }
        | people::Ask::Adopt { .. }
        | people::Ask::SetGoal { .. }
        | people::Ask::Stop { .. }
        | people::Ask::Cancel { .. }
        | people::Ask::Release { .. }
        | people::Ask::TakePerson { .. }
        | people::Ask::HandBackPerson { .. }
        | people::Ask::AnswerPerson { .. }
        | people::Ask::DecideProposal { .. }
        | people::Ask::Amend { .. }
        | people::Ask::SetRoles { .. }
        | people::Ask::ChangePolicy { .. }
        | people::Ask::SetPool { .. }
        | people::Ask::DecideEscalation { .. }
        | people::Ask::StartChat { .. }
        | people::Ask::Say { .. }
        | people::Ask::AnswerQuestion { .. }
        | people::Ask::Prioritise { .. }
        | people::Ask::Move { .. }) => Some(Box::new(other)),
    }
}

fn person_note_scope(project: u32, scope: people::NoteScope) -> notes::Scope {
    match scope {
        people::NoteScope::Deployment => notes::Scope::Deployment,
        people::NoteScope::Project => notes::Scope::Project { project },
        people::NoteScope::Goal { goal } => notes::Scope::Goal { project, goal },
        people::NoteScope::Resources { connector, pattern } => {
            let mut segments = List::with_capacity(u32::try_from(pattern.segments.len()).expect("bounded pattern"));
            for segment in pattern.segments {
                segments.push(segment).expect("pattern segment room");
            }
            let last = match pattern.last {
                people::Last::Exact(value) => notes::Last::Exact(value),
                people::Last::Open(value) => notes::Last::Open(value),
            };
            notes::Scope::Resources {
                project,
                connector,
                pattern: notes::Pattern { segments: segments.into_boxed(), last },
            }
        }
    }
}

fn person_note_change(change: people::NoteChange) -> notes::Change {
    match change {
        people::NoteChange::Correct { description, body, references, recalled } => {
            let mut kept = List::with_capacity(u32::try_from(references.len()).expect("bounded references"));
            for reference in references {
                kept.push(reference).expect("reference room");
            }
            notes::Change::Correct { description, body, references: kept, recalled }
        }
        people::NoteChange::Delete { recalled } => notes::Change::Delete { recalled },
    }
}

fn note_person_refusal(why: notes::Refusal) -> people::Refusal {
    match why {
        notes::Refusal::Busy => people::Refusal::Busy,
        notes::Refusal::Oversized | notes::Refusal::Full | notes::Refusal::RevisionExhausted => people::Refusal::Limit,
        notes::Refusal::Missing => people::Refusal::NoteMissing,
        notes::Refusal::Moved => people::Refusal::NoteMoved,
        notes::Refusal::Exists => people::Refusal::Unknown,
    }
}

fn tag_notes(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    mut child: Queue<notes::Request>,
) -> Requests {
    let room = notes::max_out(&env.limits.notes).checked_add(3).expect("bounded note route");
    let mut out = Queue::with_capacity(room);
    for _ in 0..child.len() {
        match child.pop().expect("note output count") {
            notes::Request::Save { record } => out.push(Request::Write(Write::Save(Record::Notes(record)))),
            notes::Request::Erase { key } => out.push(Request::Write(Write::Erase(Key::Notes(key)))),
            notes::Request::Load { owner, range } => {
                out.push(Request::Held(Box::new(Held::NotesLoad { owner, range })));
            }
            notes::Request::Written { owner, name, revision } => match core.note_routes.remove(&owner) {
                Some(NoteRoute::Call { to, key }) => {
                    call_decision(core, &mut out, to, key, CallPart::NoteWritten { name, revision });
                }
                Some(NoteRoute::Person { request }) => work.push(Event::People(people::Event::Decided {
                    request,
                    outcome: people::Outcome::NoteEdited { name },
                })),
                Some(NoteRoute::Brief { .. }) => unreachable!("an index cannot write an entry"),
                None => unreachable!("a note write has a waiting caller"),
            },
            notes::Request::Deleted { owner, name } => match core.note_routes.remove(&owner) {
                Some(NoteRoute::Person { request }) => work.push(Event::People(people::Event::Decided {
                    request,
                    outcome: people::Outcome::NoteEdited { name },
                })),
                Some(NoteRoute::Call { .. } | NoteRoute::Brief { .. }) | None => {
                    unreachable!("only a party deletes a note")
                }
            },
            notes::Request::Recalled { owner, entries, more } => match core.note_routes.remove(&owner) {
                Some(NoteRoute::Call { to, key }) => {
                    let part = recalled_part(core, key.task, entries, more);
                    call_decision(core, &mut out, to, key, part);
                }
                Some(NoteRoute::Person { .. } | NoteRoute::Brief { .. }) | None => {
                    unreachable!("a run owns this recall")
                }
            },
            notes::Request::Indexed { owner, lines, more } => match core.note_routes.remove(&owner) {
                Some(NoteRoute::Brief { task }) => work.push(Event::BriefNotes { task, lines, more }),
                Some(NoteRoute::Call { .. } | NoteRoute::Person { .. }) | None => {
                    unreachable!("brief preparation owns note index")
                }
            },
            notes::Request::Refused { owner, why } => match core.note_routes.remove(&owner) {
                Some(NoteRoute::Call { to, key }) => match why {
                    notes::Refusal::Busy => {
                        let _pending = core.pending_calls.remove(&key);
                        out.push(Request::Now(Box::new(Now::NoteBusy { to, key })));
                    }
                    notes::Refusal::Oversized
                    | notes::Refusal::Full
                    | notes::Refusal::Exists
                    | notes::Refusal::Missing
                    | notes::Refusal::Moved
                    | notes::Refusal::RevisionExhausted => {
                        call_decision(core, &mut out, to, key, CallPart::NoteRefused(why));
                    }
                },
                Some(NoteRoute::Person { request }) => work.push(Event::People(people::Event::Decided {
                    request,
                    outcome: people::Outcome::Refused(note_person_refusal(why)),
                })),
                Some(NoteRoute::Brief { task }) => match why {
                    notes::Refusal::Busy => {
                        core.pending_note_briefs.insert(task, true).expect("brief count bounds waiting notes");
                    }
                    notes::Refusal::Oversized
                    | notes::Refusal::Full
                    | notes::Refusal::Exists
                    | notes::Refusal::Missing
                    | notes::Refusal::Moved
                    | notes::Refusal::RevisionExhausted => {
                        work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
                    }
                },
                None => out.push(Request::Now(Box::new(Now::NotesRefused { owner, why }))),
            },
        }
    }
    if !core.notes.busy()
        && let Some((&task, _)) = core.pending_note_briefs.iter().next()
    {
        core.pending_note_briefs.remove(&task);
        work.push(Event::StartBrief { task });
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn recalled_part(core: &Core, task: u64, entries: List<notes::Entry>, more: bool) -> CallPart {
    for entry in &entries {
        if !core.note_readable(task, &entry.scope) {
            return CallPart::ToolDenied {
                answer: authority::Answer::Refuse,
                findings: Box::new([authority::Finding::Scope { source: authority::Source::Task }]),
            };
        }
    }
    CallPart::NoteRecalled { entries: entries.into_boxed(), more }
}

fn append_people(core: &mut Core, env: &Env<Limits>, event: people::Event, out: &mut Queue<Request>) {
    let mut child = Queue::with_capacity(people::max_out(&env.limits.people));
    people::step(&mut core.people, &Env { now: env.now, wall: env.wall, limits: env.limits.people }, event, &mut child);
    let mut work = Queue::with_capacity(0);
    let Requests::Out(mut routed) = tag_people(core, env, child, &mut work);
    assert!(work.is_empty(), "a keyed party terminal does not start a task route");
    for _ in 0..routed.len() {
        match routed.pop().expect("party route count") {
            Request::Decided => {}
            request @ (Request::Write(_) | Request::Ask { .. } | Request::Held(_) | Request::Now(_)) => {
                out.push(request);
            }
        }
    }
}

fn append_tasks(core: &mut Core, env: &Env<Limits>, event: tasks::Event, out: &mut Queue<Request>) {
    let room = tasks::max_out(&env.limits.tasks);
    let mut child = Queue::with_capacity(room);
    tasks::step(&mut core.tasks, &Env { now: env.now, wall: env.wall, limits: env.limits.tasks }, event, &mut child);
    let Requests::Out(mut routed) = tag_tasks(core, env, child, room, false, false, false);
    for _ in 0..routed.len() {
        match routed.pop().expect("task route count") {
            Request::Decided => {}
            request @ (Request::Write(_) | Request::Ask { .. } | Request::Held(_) | Request::Now(_)) => {
                out.push(request);
            }
        }
    }
}

fn tag_brief(core: &mut Core, env: &Env<Limits>, mut child: Queue<brief::GatherRequest>, room: u32) -> Requests {
    let task_room = tasks::max_out(&env.limits.tasks).checked_mul(8).expect("bounded task routes");
    let party_room = people::max_out(&env.limits.people).checked_mul(4).expect("bounded party routes");
    let total = room.checked_add(task_room).expect("brief and task room");
    let total = total.checked_add(party_room).expect("brief output room");
    let mut out = Queue::with_capacity(total.checked_add(1).expect("brief output mark"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("brief output count") {
            brief::GatherRequest::Gather { connector, section, budget } => {
                Request::Ask { connector, ask: Ask::Gather { section, budget } }
            }
            brief::GatherRequest::CutTo { connector, section, size } => {
                Request::Ask { connector, ask: Ask::CutTo { section, size } }
            }
            brief::GatherRequest::Drop { connector, section } => Request::Ask { connector, ask: Ask::Drop { section } },
            brief::GatherRequest::Failed { brief, .. } | brief::GatherRequest::Refused { brief } => {
                let task = brief.raw();
                drop(core.contexts.remove(&task));
                drop(core.dependency_results.remove(&task));
                drop(core.transcripts.remove(&task));
                append_tasks(core, env, tasks::Event::PreparationFailed { task }, &mut out);
                continue;
            }
            brief::GatherRequest::Complete { brief, order } => {
                let task = brief.raw();
                let current = match core.contexts.get(&task) {
                    Some(context) => match core.tasks.task(task) {
                        Some(row) => {
                            row.phase == tasks::Phase::Active(tasks::Active::Preparing)
                                && row.last_message == context.last_message
                        }
                        None => false,
                    },
                    None => false,
                };
                if !current {
                    for placed in order {
                        if let brief::GatherPlaced::Connector { connector, token, .. } = placed {
                            out.push(Request::Ask { connector, ask: Ask::Drop { section: token } });
                        }
                    }
                    continue;
                }
                Request::Now(Box::new(Now::CompleteBrief { brief, order }))
            }
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn record_host(core: &mut Core, out: &mut Queue<Request>, run: Token, attempt: Token, kind: fleet::HostKind) {
    if let Some(proof) = core.proofs.get_mut(&run.raw())
        && proof.attempt == attempt.raw()
        && proof.host != kind
    {
        proof.host = kind;
        out.push(Request::Write(Write::Save(Record::Core(CoreRecord::RunProof(proof.clone())))));
    }
}

#[expect(clippy::too_many_lines, reason = "one exhaustive host vocabulary routes its bounded child decision")]
fn tag_fleet(core: &mut Core, env: &Env<Limits>, mut child: Queue<fleet::Request>, room: u32) -> Requests {
    let marked = room.checked_mul(4).expect("bounded fleet routes");
    let task_room = tasks::max_out(&env.limits.tasks).checked_mul(8).expect("bounded task routes");
    let party_room = people::max_out(&env.limits.people).checked_mul(4).expect("bounded party routes");
    let total = marked.checked_add(task_room).expect("fleet and task route room");
    let total = total.checked_add(party_room).expect("fleet and party route room");
    let mut out = Queue::with_capacity(total.checked_add(1).expect("fleet output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("fleet output count") {
            fleet::Request::Assign { channel, kind, run, attempt } => {
                record_host(core, &mut out, run, attempt, kind);
                out.push(Request::Held(Box::new(Held::ViewStart { run, attempt })));
                Request::Held(Box::new(Held::Assign { channel, run, attempt }))
            }
            fleet::Request::Acknowledge { channel, run, attempt } => {
                Request::Held(Box::new(Held::Acknowledge { channel, run, attempt }))
            }
            fleet::Request::AcknowledgeTurn { channel, run, attempt, turn } => {
                Request::Held(Box::new(Held::AcknowledgeTurn { channel, run, attempt, turn }))
            }
            fleet::Request::Cancel { channel, run, attempt } => {
                Request::Held(Box::new(Held::Cancel { channel, run, attempt }))
            }
            fleet::Request::Refuse { channel } => Request::Held(Box::new(Held::Refuse { channel })),
            fleet::Request::TurnBusy { channel, run, attempt, turn } => {
                Request::Held(Box::new(Held::TurnBusy { channel, run, attempt, turn }))
            }
            fleet::Request::Relayed { channel, run, attempt, call, answer } => {
                Request::Held(Box::new(Held::Relayed { channel, run, attempt, call, answer }))
            }
            fleet::Request::Drop { payload } => Request::Now(Box::new(Now::DropPayload { payload })),
            fleet::Request::Listed { .. } => continue,
            fleet::Request::Placed { run, attempt } => {
                if core.unreported_restored.get(&run.raw()) == Some(&attempt.raw()) {
                    let _: Option<u64> = core.unreported_restored.remove(&run.raw());
                }
                append_tasks(core, env, tasks::Event::Started { task: run.raw(), attempt: attempt.raw() }, &mut out);
                continue;
            }
            fleet::Request::Inbound { channel, run, attempt, event } => {
                let relay = core.relaying.take().expect("fleet inbound follows committed word");
                assert!(event.raw() == relay.word.number, "relay event identifies word");
                if let Some(proof) = core.proofs.get_mut(&run.raw())
                    && proof.attempt == attempt.raw()
                    && proof.offered == relay.previous
                {
                    proof.offered = Some(match proof.offered {
                        Some(previous) => previous.max(relay.word.number),
                        None => relay.word.number,
                    });
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::RunProof(proof.clone())))));
                    out.push(Request::Held(Box::new(Held::Inbound { channel, run, attempt, word: relay.word })));
                }
                continue;
            }
            fleet::Request::Undelivered { .. } => {
                drop(core.relaying.take());
                continue;
            }
            fleet::Request::NotStarted { to, run, attempt }
            | fleet::Request::Withdrawn { to, run, attempt, .. }
            | fleet::Request::Refused { to, run, attempt, .. } => {
                let _answered = to.into_token();
                let _: Option<u64> = core.unreported_restored.remove(&run.raw());
                core.remember_unpriced_terminal(run, attempt, tasks::End::Refused);
                out.push(Request::Now(Box::new(Now::DropAssignment { task: run.raw() })));
                append_tasks(
                    core,
                    env,
                    tasks::Event::Activation {
                        reply_to: ReplyTo::new(Token::new(u64::MAX)),
                        task: run.raw(),
                        attempt: attempt.raw(),
                        end: tasks::End::Refused,
                        saved: None,
                        cause: tasks::Cause::Unpriced,
                    },
                    &mut out,
                );
                continue;
            }
            fleet::Request::Lost { to, run, attempt } => {
                let _answered = to.into_token();
                let proof = core.proofs.get(&run.raw()).expect("lost claim has durable evidence");
                assert!(proof.attempt == attempt.raw(), "lost callback belongs to current proof");
                let never_reported = core.unreported_restored.remove(&run.raw()) == Some(attempt.raw());
                let mut committed_call = false;
                for (key, _) in &core.call_parts {
                    if key.task == run.raw() && key.attempt == attempt.raw() {
                        committed_call = true;
                        break;
                    }
                }
                let end = if never_reported && proof.turn.is_none() && !committed_call {
                    tasks::End::Refused
                } else {
                    tasks::End::Failed(tasks::Class::Lost)
                };
                core.remember_unpriced_terminal(run, attempt, end.clone());
                append_tasks(
                    core,
                    env,
                    tasks::Event::Activation {
                        reply_to: ReplyTo::new(Token::new(u64::MAX)),
                        task: run.raw(),
                        attempt: attempt.raw(),
                        end,
                        saved: None,
                        cause: tasks::Cause::Unpriced,
                    },
                    &mut out,
                );
                for &connector in core.connectors.as_ref() {
                    out.push(Request::Ask { connector, ask: Ask::Lost { task: run.raw(), attempt: attempt.raw() } });
                }
                continue;
            }
            fleet::Request::AssignTyped { channel, kind, run, attempt, activation, assignment } => {
                record_host(core, &mut out, run, attempt, kind);
                out.push(Request::Held(Box::new(Held::ViewStart { run, attempt })));
                Request::Held(Box::new(Held::AssignTyped { channel, run, attempt, activation, assignment }))
            }
            fleet::Request::InboundTyped { channel, run, attempt, message } => {
                if let Some(proof) = core.proofs.get_mut(&run.raw())
                    && proof.attempt == attempt.raw()
                {
                    proof.offered = Some(match proof.offered {
                        Some(previous) => previous.max(message.name.raw()),
                        None => message.name.raw(),
                    });
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::RunProof(proof.clone())))));
                }
                Request::Held(Box::new(Held::InboundTyped { channel, run, attempt, message }))
            }
            fleet::Request::RelayTyped { reply_to, run, attempt, call } => {
                Request::Now(Box::new(Now::CallTyped { to: reply_to, run, attempt, call }))
            }
            fleet::Request::RelayedTyped { channel, run, attempt, call, answer } => {
                Request::Held(Box::new(Held::RelayedTyped { channel, run, attempt, call, answer }))
            }
            fleet::Request::DropTyped { call } => Request::Now(Box::new(Now::DropTyped { call })),
            fleet::Request::UndeliveredTyped { run, attempt, message, undelivered: _ } => {
                Request::Now(Box::new(Now::UndeliveredTyped { run, attempt, message }))
            }
            fleet::Request::Grant { .. }
            | fleet::Request::Rejected { .. }
            | fleet::Request::Exhausted { .. }
            | fleet::Request::Bounced { .. }
            | fleet::Request::Told { .. } => unreachable!("account and diagnostic host routes are handled separately"),
            fleet::Request::Turned { run, attempt, turn, body } => {
                assert!(core.current_proof(run.raw(), attempt.raw()), "actual current turn has reserved proof");
                Request::Now(Box::new(Now::TurnPayload { run, attempt, turn, body }))
            }
            fleet::Request::Answered { run, attempt, payload, to, .. } => {
                let _answered = to.into_token();
                let _: Option<u64> = core.unreported_restored.remove(&run.raw());
                assert!(core.current_proof(run.raw(), attempt.raw()), "actual current answer has reserved proof");
                Request::Now(Box::new(Now::AnswerPayload { run, attempt, payload }))
            }
            fleet::Request::Relay { reply_to, run, attempt, body } => {
                assert!(core.current_proof(run.raw(), attempt.raw()), "fleet only relays a current claim");
                Request::Now(Box::new(Now::CallPayload { to: reply_to, run, attempt, body }))
            }
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn tag_view(mut child: Queue<views::Request>, room: u32) -> Requests {
    let mut out = Queue::with_capacity(room.checked_add(1).expect("view output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("view output count") {
            request @ (views::Request::Watching { .. }
            | views::Request::Refused { .. }
            | views::Request::Deliver { .. }
            | views::Request::Ended { .. }) => Request::Now(Box::new(Now::View(request))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn tag_account(mut child: Queue<accounts::Request>) -> Requests {
    let mut out = Queue::with_capacity(accounts::MAX_OUT.checked_add(1).expect("account output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("account output count") {
            request @ (accounts::Request::Refresh { .. }
            | accounts::Request::Keep { .. }
            | accounts::Request::Cancel { .. }
            | accounts::Request::Granted { .. }
            | accounts::Request::Availability { .. }
            | accounts::Request::Refused { .. }
            | accounts::Request::Closed { .. }) => Request::Now(Box::new(Now::Account(request))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn remember_due(core: &mut Core, context: Box<tasks::RunContext>) {
    for task in &core.due {
        if task.task == context.task {
            return;
        }
    }
    core.due.push(context);
}

fn route_room_checked(limits: &Limits) -> Option<u32> {
    tasks::worst_case(&limits.tasks)?;
    let tasks = tasks::max_out(&limits.tasks).checked_mul(16)?;
    let people = people::max_out(&limits.people).checked_mul(8)?;
    let fleet = fleet::max_out(&limits.fleet).checked_mul(8)?;
    let brief = brief::gather_max_out(&limits.brief).checked_mul(4)?;
    tasks
        .checked_add(people)?
        .checked_add(fleet)?
        .checked_add(brief)?
        .checked_add(limits.people.pending.checked_mul(2)?)?
        .checked_add(limits.tasks.tasks)?
        .checked_add(8)
}

fn route_room(limits: &Limits) -> u32 {
    route_room_checked(limits).expect("validated core route room")
}

/// Largest core decision room, including a sibling callback reached after a
/// connector's own event. Roots use this when their input begins outside the
/// core but may synchronously hand work into it.
#[must_use]
pub fn room_max(limits: &Limits) -> Option<JournalRoom> {
    let bound = route_room_checked(limits)?;
    Some(JournalRoom { writes: bound, held: bound })
}

/// Pre-admission journal room for one core event, including all synchronous
/// sibling continuations and connector handoffs it can cause. The application
/// adds its connectors' writes and held outputs before taking the journal
/// decision (`domain/root.md`, section 10). The shared ceiling is deliberate:
/// a child callback may visit every other child before the decision ends.
#[must_use]
pub fn room(limits: &Limits, event: &Event) -> Option<JournalRoom> {
    let room = room_max(limits)?;
    match event {
        Event::EffectStart { .. }
        | Event::EffectConnector(_)
        | Event::EffectDeadline
        | Event::SignIn { .. }
        | Event::Watch { .. }
        | Event::Tasks(_)
        | Event::StartRecurring { .. }
        | Event::StartBrief { .. }
        | Event::BriefNotes { .. }
        | Event::BriefAssembled { .. }
        | Event::WorkspacePrepared { .. }
        | Event::ClaimPrepared { .. }
        | Event::ClaimRefused { .. }
        | Event::ConnectorRepair { .. }
        | Event::Period { .. }
        | Event::PreparationFailed { .. }
        | Event::PersonProposal(_)
        | Event::PersonEscalation(_)
        | Event::TaskEscalation(_)
        | Event::People(_)
        | Event::Fleet(_)
        | Event::TurnPayload { .. }
        | Event::AnswerPayload { .. }
        | Event::AcceptedTurn { .. }
        | Event::RefusedPayload { .. }
        | Event::Activate { .. }
        | Event::PreparedAgent { .. }
        | Event::HistoricalProposal { .. }
        | Event::HistoricalEscalation { .. }
        | Event::EscalationRead { .. }
        | Event::NamedAnswer { .. }
        | Event::NamedAction { .. }
        | Event::DelegateValidated { .. }
        | Event::DelegateInputFailed { .. }
        | Event::DelegateHoldings { .. }
        | Event::ProcedureHoldings { .. }
        | Event::ProcedureStep { .. }
        | Event::Brief(_)
        | Event::Holdings { .. }
        | Event::Account(_)
        | Event::View(_)
        | Event::Notes(_)
        | Event::SettledCall { .. } => Some(room),
    }
}

/// Route one child event and each sibling continuation within one core decision.
pub fn step(core: &mut Core, env: &Env<Limits>, event: Event) -> Requests {
    let room = route_room(&env.limits);
    let mut work = Queue::with_capacity(room);
    let mut out = Queue::with_capacity(room.checked_add(1).expect("core output mark"));
    work.push(event);
    for _ in 0..room {
        let Some(next) = work.pop() else { break };
        let Requests::Out(mut batch) = step_one(core, env, next, &mut work);
        for _ in 0..batch.len() {
            match batch.pop().expect("core route output count") {
                Request::Decided => {}
                request @ (Request::Write(_) | Request::Ask { .. } | Request::Held(_) | Request::Now(_)) => {
                    out.push(request);
                }
            }
        }
    }
    assert!(work.is_empty(), "finite sibling routes fit their bound");
    out.push(Request::Decided);
    Requests::Out(out)
}

/// Resume the fleet's next held placement or relay through the core's sibling routes.
pub fn resume_fleet(core: &mut Core, env: &Env<Limits>) -> Requests {
    let mut child = Queue::with_capacity(fleet::max_out(&env.limits.fleet));
    fleet::resume(&mut core.fleet, &Env { now: env.now, wall: env.wall, limits: env.limits.fleet }, &mut child);
    tag_fleet(core, env, child, fleet::max_out(&env.limits.fleet))
}

#[expect(clippy::too_many_lines, reason = "each bounded child event has one exhaustive route")]
fn step_one(core: &mut Core, env: &Env<Limits>, event: Event, work: &mut Queue<Event>) -> Requests {
    match event {
        Event::SettledCall { to, key, call } => crate::typed::settle(core, &env.limits, to, key, call),
        Event::EffectStart { owner, connector, origin } => crate::effects::start(core, env, owner, connector, origin),
        Event::EffectConnector(event) => crate::effects::connector(core, env, work, event),
        Event::EffectDeadline => crate::effects::deadline(core, env),
        Event::SignIn { reply_to, identity } => {
            let mut out = Queue::with_capacity(2);
            if core.counters.deployment().people == u64::MAX || core.counters.deployment().sign_ins == u64::MAX {
                out.push(Request::Now(Box::new(Now::SignInRefused { to: reply_to })));
            } else {
                let person = crate::fresh(&mut core.counters, crate::Family::Person).expect("person counter available");
                let sign_in =
                    crate::fresh(&mut core.counters, crate::Family::SignIn).expect("sign-in counter available");
                core.signing_in = Some(sign_in);
                let kind = if identity.key.provider == core.settings.deployment_provider {
                    people::Kind::Service
                } else {
                    people::Kind::Person
                };
                work.push(Event::People(people::Event::SignedIn { reply_to, person, sign_in, identity, kind }));
            }
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::Watch { watcher, sign_in, key, project, subject, ready } => {
            crate::watch::open(core, env, watcher, sign_in, key, project, subject, ready)
        }
        Event::Tasks(event) => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::step(
                &mut core.tasks,
                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                event,
                &mut out,
            );
            tag_tasks(core, env, out, tasks::max_out(&env.limits.tasks), false, false, false)
        }
        Event::StartRecurring { project, authority, template } => {
            start_recurring(core, work, project, authority, template);
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::StartBrief { task } => start_brief(core, env, work, task),
        Event::BriefNotes { task, lines, more } => plan_brief(core, env, work, task, lines, more),
        Event::BriefAssembled { task, ready } => brief_assembled(core, work, task, ready),
        Event::WorkspacePrepared { task, attempt, writes } => {
            workspace_prepared(core, env, work, task, attempt, writes)
        }
        Event::ClaimPrepared { task, attempt, budget, writes } => {
            if core.claiming.get(&task) == Some(&attempt) {
                work.push(Event::Tasks(tasks::Event::Claim {
                    reply_to: ReplyTo::new(Token::new(task)),
                    task,
                    attempt,
                    budget,
                    writes,
                }));
            }
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::ClaimRefused { task } => {
            let mut out = Queue::with_capacity(2);
            if core.claiming.remove(&task).is_some() {
                drop(core.proofs.remove(&task));
                out.push(Request::Now(Box::new(Now::DropAssignment { task })));
                work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
            }
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::ConnectorRepair { seed, executor, spec, contract, holdings } => {
            if seed.open_period {
                work.push(Event::Tasks(tasks::Event::OpenPeriod {
                    reply_to: ReplyTo::new(Token::new(u64::MAX - 2)),
                    project: seed.project,
                    period: seed.period,
                    budget: seed.period_budget,
                }));
            }
            work.push(Event::Tasks(tasks::Event::Make {
                reply_to: ReplyTo::new(Token::new(u64::MAX - 3)),
                creator: tasks::Party::Deployment { project: seed.project },
                batch: Box::new([tasks::New {
                    number: seed.number,
                    project: seed.project,
                    executor,
                    spec,
                    contract,
                    numbers: tasks::Numbers { budget: seed.budget, spent: 0, spent_below: 0, reserved: 0 },
                    authority: seed.authority,
                    funder: tasks::Funder::Period { project: seed.project, period: seed.period },
                    dependencies: Box::new([]),
                    holdings,
                    wake: tasks::WakePolicy::DEFAULT,
                    recurring: None,
                    tracked: None,
                }]),
            }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::Period { project, period, budget } => {
            let allowed = match core.authority.policy(project) {
                Some(policy) => budget <= policy.period_spend,
                None => false,
            };
            if period > 0 && period >= core.settings.period && allowed {
                core.settings.period = period;
                core.settings.period_budget = budget;
                if core.tasks.funding(tasks::Funder::Period { project, period }).is_none() {
                    work.push(Event::Tasks(tasks::Event::OpenPeriod {
                        reply_to: ReplyTo::new(Token::new(u64::MAX - 2)),
                        project,
                        period,
                        budget,
                    }));
                }
                for task in core.tasks.recurring_tasks(project) {
                    work.push(Event::Tasks(tasks::Event::TickRecurring { task, period }));
                }
                for task in core.tasks.standing_tasks(project) {
                    work.push(Event::Tasks(tasks::Event::RenewStanding { task, period }));
                }
            }
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::PreparationFailed { task } => {
            drop(core.contexts.remove(&task));
            drop(core.dependency_results.remove(&task));
            drop(core.transcripts.remove(&task));
            work.push(Event::Tasks(tasks::Event::PreparationFailed { task }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::PersonProposal(event) => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::step(
                &mut core.tasks,
                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                event,
                &mut out,
            );
            tag_tasks(core, env, out, tasks::max_out(&env.limits.tasks), true, false, false)
        }
        Event::TaskEscalation(event) => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::step(
                &mut core.tasks,
                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                event,
                &mut out,
            );
            tag_tasks(core, env, out, tasks::max_out(&env.limits.tasks), false, true, false)
        }
        Event::PersonEscalation(event) => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::step(
                &mut core.tasks,
                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                event,
                &mut out,
            );
            tag_tasks(core, env, out, tasks::max_out(&env.limits.tasks), false, false, true)
        }
        Event::People(event) => {
            let mut child = Queue::with_capacity(people::max_out(&env.limits.people));
            people::step(
                &mut core.people,
                &Env { now: env.now, wall: env.wall, limits: env.limits.people },
                event,
                &mut child,
            );
            tag_people(core, env, child, work)
        }
        Event::Holdings { request, connector, holdings } => {
            created_holdings(core, env, work, request, connector, holdings);
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::Fleet(event) => {
            let mut out = Queue::with_capacity(fleet::max_out(&env.limits.fleet));
            fleet::step(
                &mut core.fleet,
                &Env { now: env.now, wall: env.wall, limits: env.limits.fleet },
                event,
                &mut out,
            );
            tag_fleet(core, env, out, fleet::max_out(&env.limits.fleet))
        }
        Event::TurnPayload { run, attempt, turn, body, read, cumulative } => {
            assert!(core.current_proof(run.raw(), attempt.raw()), "actual current turn has reserved proof");
            let offered = core.proofs.get(&run.raw()).expect("current proof").offered;
            work.push(Event::Tasks(tasks::Event::Turn {
                reply_to: ReplyTo::new(body),
                task: run.raw(),
                attempt: attempt.raw(),
                turn,
                read,
                offered,
                cumulative,
            }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::AnswerPayload { run, attempt, payload, cumulative, end, saved, invalid_saved } => {
            assert!(core.current_proof(run.raw(), attempt.raw()), "actual current answer has reserved proof");
            let (cumulative, end, saved) = match core.committed_terminal(run.raw(), attempt.raw()) {
                Some(terminal) => (terminal.cumulative, terminal.end, None),
                None => {
                    if invalid_saved {
                        (cumulative, tasks::End::Failed(tasks::Class::Invalid), None)
                    } else {
                        (cumulative, end, saved)
                    }
                }
            };
            let proof = core.proofs.get_mut(&run.raw()).expect("terminal proof pre-reserved");
            proof.terminal =
                Some(crate::TerminalRecord { task: run.raw(), attempt: attempt.raw(), cumulative, end: end.clone() });
            work.push(Event::Tasks(tasks::Event::Activation {
                reply_to: ReplyTo::new(payload),
                task: run.raw(),
                attempt: attempt.raw(),
                cause: tasks::Cause::Priced { cumulative },
                end,
                saved,
            }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::AcceptedTurn { task, attempt, turn, accepted, cumulative, read, transcript } => {
            let proof = core.proofs.get_mut(&task).expect("turn proof reserved before child mutation");
            assert!(proof.attempt == attempt, "turn callback retains its actual claim");
            let mut out = Queue::with_capacity(env.limits.call_records.checked_add(6).expect("turn route room"));
            match accepted {
                tasks::Accepted::New => {
                    proof.turn = Some(crate::TurnProof { turn, cumulative, read });
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::RunProof(proof.clone())))));
                    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::Turn(crate::TurnRecord {
                        task,
                        attempt,
                        turn,
                        spent: cumulative,
                        read,
                        at: env.wall,
                        transcript,
                    })))));
                    for key in core.retire_calls(task, attempt, turn, env.limits.call_records) {
                        out.push(Request::Write(Write::Erase(Key::Core(CoreKey::Call(key)))));
                    }
                    out.push(Request::Held(Box::new(Held::ViewTurn { task, attempt, turn })));
                }
                tasks::Accepted::Already => {}
            }
            out.push(Request::Held(Box::new(Held::TaskTurnKept { task, attempt, turn })));
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::RefusedPayload { request, problem, payload } => {
            match payload {
                Some(PayloadRefusal::Turn { task, attempt, turn }) if problem.task == Some(task) => {
                    work.push(Event::Fleet(fleet::Event::TurnBusy {
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                        turn,
                    }));
                }
                Some(PayloadRefusal::Answer { task, attempt })
                    if problem.task == Some(task) && core.current_proof(task, attempt) =>
                {
                    if core.committed_terminal(task, attempt).is_some() {
                        let mut out = Queue::with_capacity(1);
                        out.push(Request::Decided);
                        return Requests::Out(out);
                    }
                    let proof = core.proofs.get_mut(&task).expect("answer proof reserved");
                    proof.terminal = Some(crate::TerminalRecord {
                        task,
                        attempt,
                        cumulative: match proof.turn {
                            Some(turn) => turn.cumulative,
                            None => 0,
                        },
                        end: tasks::End::Failed(tasks::Class::Invalid),
                    });
                    work.push(Event::Tasks(tasks::Event::Activation {
                        reply_to: ReplyTo::new(request),
                        task,
                        attempt,
                        end: tasks::End::Failed(tasks::Class::Invalid),
                        saved: None,
                        cause: tasks::Cause::Unpriced,
                    }));
                }
                Some(PayloadRefusal::Turn { .. } | PayloadRefusal::Answer { .. }) | None => {}
            }
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::Activate { context, ready } => {
            let number = context.task;
            let mut out = Queue::with_capacity(2);
            if !ready || core.restart_step().is_some() || core.restart_failure().is_some() {
                remember_due(core, context);
            } else if core.tasks.recurring_template(number).is_none() {
                match context.executor {
                    tasks::Executor::Person(_) => {}
                    tasks::Executor::Procedure { connector, .. } => {
                        if let Some(step) = context.previous_attempt.checked_add(1) {
                            out.push(Request::Ask { connector, ask: Ask::StartProcedure { context, step } });
                        } else {
                            work.push(Event::Tasks(tasks::Event::Hold { task: number, why: tasks::Hold::Effects }));
                        }
                    }
                    tasks::Executor::Agent { .. } => match core.run_admission(&context, env.wall, Box::new([])) {
                        crate::RunAdmission::Allow => out.push(Request::Now(Box::new(Now::PrepareAgent { context }))),
                        crate::RunAdmission::Account => remember_due(core, context),
                        crate::RunAdmission::Hold(why) => {
                            work.push(Event::Tasks(tasks::Event::Hold { task: number, why }));
                        }
                    },
                }
            }
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::PreparedAgent { context, transcript_waiter, busy } => {
            let mut out = Queue::with_capacity(2);
            if busy {
                remember_due(core, context);
            } else {
                let number = context.task;
                let previous_attempt = context.previous_attempt;
                assert!(core.contexts.insert(number, context).is_ok(), "bounded activation context");
                core.begin_transcript(number, previous_attempt);
                out.push(Request::Now(Box::new(Now::StartPreparation { task: number, transcript_waiter })));
            }
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::HistoricalProposal { request, person, project, proposer, proposal, row, busy } => {
            let outcome = if busy {
                people::Outcome::Refused(people::Refusal::Busy)
            } else if let Some(row) = row {
                let named = row.proposal == proposal
                    && row.project == project
                    && row.proposal <= core.counters.deployment().messages
                    && row.by != 0
                    && row.by <= core.counters.deployment().people
                    && match row.proposer {
                        tasks::Party::Person(number) | tasks::Party::Task(number) => number == proposer,
                        tasks::Party::Deployment { .. } => false,
                    };
                if named {
                    let allowed = person == row.by
                        || core.proposal_policy_standing(
                            person,
                            project,
                            match row.kind {
                                tasks::ProposalKind::Batch => authority::ProposalKind::Batch,
                                tasks::ProposalKind::Effect => authority::ProposalKind::Effect,
                                tasks::ProposalKind::Amend => authority::ProposalKind::Amend,
                                tasks::ProposalKind::Widen => authority::ProposalKind::Widen,
                                tasks::ProposalKind::Release => authority::ProposalKind::Escalation,
                            },
                        );
                    if allowed {
                        people::Outcome::ProposalDecided { proposer, proposal, by: row.by, choice: row.choice }
                    } else {
                        people::Outcome::Refused(people::Refusal::Standing)
                    }
                } else {
                    people::Outcome::Refused(people::Refusal::Unknown)
                }
            } else {
                people::Outcome::Refused(people::Refusal::Unknown)
            };
            work.push(Event::People(people::Event::Decided { request, outcome }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::HistoricalEscalation { request, person, project, task, revision, row, busy } => {
            let outcome = if busy {
                people::Outcome::Refused(people::Refusal::Busy)
            } else if let Some(row) = row {
                let named = row.task == task
                    && row.revision == revision
                    && row.task <= core.counters.deployment().tasks
                    && row.by != 0
                    && row.by <= core.counters.deployment().people;
                if named {
                    let bounded = match &row.decision {
                        people::EscalationDecision::Release | people::EscalationDecision::Pass => true,
                        people::EscalationDecision::Reject { reason } => {
                            reason.len()
                                <= usize::try_from(env.limits.escalation_reason_bytes).expect("reason bound fits usize")
                        }
                    };
                    let private = row.project == project
                        && row.requester != 0
                        && row.requester <= core.counters.deployment().people
                        && (row.requester == person
                            || match core.escalation_fallback(project) {
                                Some(tasks::EscalationHolder::Role { role: selected, .. }) => {
                                    match core.people.role(person, project) {
                                        Some(role) => role.number() == selected,
                                        None => false,
                                    }
                                }
                                Some(tasks::EscalationHolder::Task(_) | tasks::EscalationHolder::Person(_)) | None => {
                                    false
                                }
                            });
                    if !private {
                        people::Outcome::Refused(people::Refusal::Standing)
                    } else if bounded {
                        let choice = match row.decision {
                            people::EscalationDecision::Release => people::EscalationChoice::Released,
                            people::EscalationDecision::Reject { .. } => people::EscalationChoice::Rejected,
                            people::EscalationDecision::Pass => people::EscalationChoice::Passed,
                        };
                        people::Outcome::EscalationDecided { task, revision, by: row.by, choice }
                    } else {
                        people::Outcome::Refused(people::Refusal::Unknown)
                    }
                } else {
                    people::Outcome::Refused(people::Refusal::Unknown)
                }
            } else {
                people::Outcome::Refused(people::Refusal::Unknown)
            };
            work.push(Event::People(people::Event::Decided { request, outcome }));
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::EscalationRead { to, person, task, context } => {
            let answer = match context {
                Some(context) => {
                    let role = core.people.role(person, context.project);
                    if context.task == task && core.escalation_visible(&context, person, role) {
                        Now::EscalationReply { to, person, context }
                    } else {
                        Now::EscalationRefused { to, why: people::Refusal::Standing }
                    }
                }
                None => Now::EscalationRefused { to, why: people::Refusal::Unknown },
            };
            let mut out = Queue::with_capacity(2);
            out.push(Request::Now(Box::new(answer)));
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::NamedAnswer { to, key, part } => {
            assert!(core.current_proof(key.task, key.attempt), "named call belongs to current claim");
            let mut out = Queue::with_capacity(3);
            call_decision(core, &mut out, to, key, part);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::NamedAction { to, key, action } => named_action(core, env, work, to, key, action),
        Event::DelegateValidated { to, key, batch, stubs } => delegate_validated(core, env, to, key, batch, stubs),
        Event::DelegateInputFailed { to, key } => {
            work.push(Event::NamedAnswer {
                to,
                key,
                part: CallPart::DelegationRefused(tasks::Problem {
                    task: Some(key.task),
                    why: tasks::Refusal::Inputs,
                    blocked_by: None,
                }),
            });
            let mut out = Queue::with_capacity(1);
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::DelegateHoldings { request, connector, holdings } => {
            delegate_holdings(core, env, work, request, connector, holdings)
        }
        Event::ProcedureStep { task, step, connector, code, action } => {
            procedure_step(core, env, work, task, step, connector, code, action)
        }
        Event::ProcedureHoldings { task, step, connector, holdings } => {
            procedure_holdings(core, env, work, task, step, connector, holdings)
        }
        Event::Brief(event) => {
            let mut out = Queue::with_capacity(brief::gather_max_out(&env.limits.brief));
            brief::gather_step(
                &mut core.brief,
                &Env { now: env.now, wall: env.wall, limits: env.limits.brief },
                event,
                &mut out,
            );
            tag_brief(core, env, out, brief::gather_max_out(&env.limits.brief))
        }
        Event::Account(event) => {
            let mut out = Queue::with_capacity(accounts::MAX_OUT);
            accounts::step(
                &mut core.accounts,
                &Env { now: env.now, wall: env.wall, limits: env.limits.accounts },
                event,
                &mut out,
            );
            tag_account(out)
        }
        Event::View(event) => {
            let mut out = Queue::with_capacity(views::max_out(&env.limits.views));
            views::step(
                &mut core.views,
                &Env { now: env.now, wall: env.wall, limits: env.limits.views },
                event,
                &mut out,
            );
            tag_view(out, views::max_out(&env.limits.views))
        }
        Event::Notes(event) => {
            let mut child = Queue::with_capacity(notes::max_out(&env.limits.notes));
            notes::step(
                &mut core.notes,
                &Env { now: env.now, wall: env.wall, limits: env.limits.notes },
                event,
                &mut child,
            );
            tag_notes(core, env, work, child)
        }
    }
}

/// Fire one due child timer, returning its whole bounded request batch.
pub fn fire(core: &mut Core, env: &Env<Limits>, timer: Timer) -> Requests {
    match timer {
        Timer::Tasks => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::fire(&mut core.tasks, &Env { now: env.now, wall: env.wall, limits: env.limits.tasks }, &mut out);
            tag_tasks(core, env, out, tasks::max_out(&env.limits.tasks), false, false, false)
        }
        Timer::Fleet => {
            let mut out = Queue::with_capacity(fleet::max_out(&env.limits.fleet));
            fleet::fire(&mut core.fleet, &Env { now: env.now, wall: env.wall, limits: env.limits.fleet }, &mut out);
            tag_fleet(core, env, out, fleet::max_out(&env.limits.fleet))
        }
        Timer::Brief => {
            let mut out = Queue::with_capacity(brief::gather_max_out(&env.limits.brief));
            brief::gather_fire(
                &mut core.brief,
                &Env { now: env.now, wall: env.wall, limits: env.limits.brief },
                &mut out,
            );
            tag_brief(core, env, out, brief::gather_max_out(&env.limits.brief))
        }
        Timer::Account => {
            let mut out = Queue::with_capacity(accounts::MAX_OUT);
            accounts::fire(
                &mut core.accounts,
                &Env { now: env.now, wall: env.wall, limits: env.limits.accounts },
                &mut out,
            );
            tag_account(out)
        }
        Timer::View => {
            let mut out = Queue::with_capacity(views::max_out(&env.limits.views));
            views::fire(&mut core.views, &Env { now: env.now, wall: env.wall, limits: env.limits.views }, &mut out);
            tag_view(out, views::max_out(&env.limits.views))
        }
    }
}
