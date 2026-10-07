use alloc::boxed::Box;
use skein_lib::{Duration, List, Token, Wall};

/// Whether an effect creates, changes or sets its target.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Form {
    /// Make a named object that did not exist.
    Creation,
    /// Move an object from one state to another.
    Transition,
    /// Set an object to a named value.
    Set,
}

/// What the system can guarantee after an uncertain write.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Recovery {
    /// The system remembers an operation's key.
    Keyed,
    /// The system checks a starting state when applying the write.
    Conditional,
    /// Repeating the write makes the same final state.
    Idempotent,
    /// No safe automatic retry is possible.
    Unrecoverable,
}

/// One numbered effect kind offered by this connector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct KindSpec {
    /// The connector-local kind number.
    pub kind: u16,
    /// What the write does.
    pub form: Form,
    /// How an uncertain outcome is handled.
    pub recovery: Recovery,
    /// Maximum cost charged when the effect is decided.
    pub price: Option<u64>,
}

/// A requested write, in this connector's terms.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Effect {
    /// Numbered kind whose form and recovery are in the configuration.
    pub kind: u16,
    /// Every resource this effect touches.
    pub resources: Box<[Path]>,
    /// Stable purpose within the asking task.
    pub purpose: u64,
    /// Starting state checked by the system, if available.
    pub condition: Option<u64>,
    /// State to make on the system.
    pub target: u64,
    /// State passed to every requirement judge.
    pub state: u64,
}

/// The part of an effect that the core may inspect.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Description {
    /// Numbered effect kind.
    pub kind: u16,
    /// Names checked against authority grants.
    pub resources: Box<[Path]>,
    /// Stable purpose used in its key.
    pub purpose: u64,
    /// Whether the system checks the effect's starting state.
    pub condition: bool,
    /// Creation, transition or set.
    pub form: Form,
    /// The kind's recovery class.
    pub recovery: Recovery,
    /// Maximum effect price, if any.
    pub price: Option<u64>,
    /// State the judges are asked about.
    pub state: u64,
}

/// A deployment-scoped key derived from an effect's task and purpose.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Key {
    /// Deployment identifier, never shared with another deployment.
    pub deployment: u64,
    /// Task that asked for this effect.
    pub task: u64,
    /// Stable purpose within that task.
    pub purpose: u64,
}

/// An attempt committed before it is sent to the system.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Attempt {
    /// Starts at one and grows with each retry.
    pub number: u32,
    /// Wall time at which this attempt was prepared.
    pub sent: Wall,
    /// Absolute latest time before an uncertain write may be retried.
    pub deadline: Wall,
}

/// The durable phase of an outbox entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EffectPhase {
    /// Waiting for the first release by the root's journal.
    Kept,
    /// Its attempt is durably recorded and may be in flight.
    Sent,
    /// An attempt may have reached the system.
    Uncertain,
    /// A proven failure waits for its retry deadline.
    Retry,
    /// An unrecoverable outcome waits for a person.
    Held,
}

/// A durable entry that has not yet settled.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct OutboxEntry {
    /// Numbered in decision order by the root.
    pub number: u64,
    /// Task that asked for it.
    pub task: u64,
    /// Stable deployment-scoped key.
    pub key: Key,
    /// Connector-owned request.
    pub effect: Effect,
    /// Last prepared attempt, if any.
    pub attempt: Option<Attempt>,
    /// Durable execution phase.
    pub phase: EffectPhase,
}

/// What an effect resolved to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Outcome {
    /// The system reached the decided state, possibly found after uncertainty.
    Made { state: u64, found: bool },
    /// The system refused the write or its condition failed.
    Failed,
    /// A write may still land; recovery continues or a person must decide.
    Uncertain,
    /// An unsent effect was cancelled.
    Withdrawn,
}

/// How the fake system answered an apply call.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ApplyResult {
    /// The write took effect or repeated its keyed result.
    Made { state: u64 },
    /// The guarded starting state no longer holds.
    Conflict,
    /// The request provably never reached the system.
    Transient,
    /// It may yet take effect.
    Uncertain,
}

/// What a lookup observed beneath the connector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Looked {
    /// Value of the target, if one exists.
    pub state: Option<u64>,
    /// Which key made that value, if the system can tell.
    pub owner: Option<Key>,
    /// Whether a keyed operation was found by its key.
    pub found_key: bool,
}

/// Calls sent down to the fake system after a decision's commit.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SystemRequest {
    /// Refresh one fact used by requirements and procedures.
    ReadFact { resource: Path, observed: Wall },
    /// Apply one committed outbox attempt.
    Apply { entry: u64, attempt: u32, key: Key, effect: Effect, form: Form, recovery: Recovery },
    /// Look for the result of an uncertain attempt.
    Look { entry: u64, key: Key, effect: Effect, recovery: Recovery },
}

/// A bounded sequence of byte segments naming one resource.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Path {
    segments: Box<[Box<[u8]>]>,
}

impl Path {
    /// Takes owned segments after checking their configured count and size.
    #[must_use]
    pub fn new(segments: List<Box<[u8]>>, max_bytes: u32) -> Option<Path> {
        if segments.is_empty() {
            return None;
        }
        for segment in &segments {
            if segment.is_empty() || u32::try_from(segment.len()).ok()? > max_bytes {
                return None;
            }
        }
        Some(Path { segments: segments.into_boxed() })
    }

    /// Whether this name lies at or beneath the prefix.
    #[must_use]
    pub fn under(&self, prefix: &Path) -> bool {
        self.segments.starts_with(&prefix.segments)
    }

    /// The owned path segments, for a root translating resource names.
    #[must_use]
    pub fn segments(&self) -> &[Box<[u8]>] {
        &self.segments
    }
}

/// The project's relationship to a resource.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ResourceRole {
    /// The deployment made and owns it.
    Owned,
    /// Others may write it too.
    Participating,
    /// The application may read it but not write it.
    Context,
}

/// A connector's hold shape for a resource kind.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Hold {
    /// At most one task holds the resource.
    Exclusive { mode: HoldMode },
    /// A task takes one of a pool's counted slots.
    Pooled { mode: HoldMode },
    /// The resource is shared and has no hold.
    None,
}

/// What happens when a required hold is already taken.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HoldMode {
    /// Refuse the task's admission.
    Refuse,
    /// Admit the task into the hold's bounded wait queue.
    Wait,
}

/// A resource and the terms on which the connector may use it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ResourceSpec {
    /// The resource's path under the deployment prefix.
    pub path: Path,
    /// How tasks hold it, if they do.
    pub hold: Hold,
    /// What this deployment may do there.
    pub writable: bool,
}

/// A pool's initial slots and the allocations the fake system can later lose.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PoolSpec {
    /// The pool's path.
    pub path: Path,
    /// Initial count of slots.
    pub slots: u32,
}

/// A subject on which tasks may receive classified news.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TopicSpec {
    /// The connector-local topic number.
    pub topic: u16,
}

/// A connector-owned judge on the state of a named resource.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RequirementSpec {
    /// Connector-local requirement number.
    pub number: u16,
    /// The effect's system checks this state while applying the write.
    pub guarded: bool,
    /// Maximum age of an observed fact. The world configures this value.
    pub freshness: Duration,
}

/// A fact read from the system, rather than inferred from a write request.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fact {
    /// Last observed state, or none when the resource was absent.
    pub state: Option<u64>,
    /// Wall time of that observation.
    pub observed: Wall,
    /// An outstanding system update makes this fact undecided.
    pub pending: bool,
}

/// A requirement's verdict for the exact state in the question.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Verdict {
    /// The named state held on a sufficiently fresh fact.
    Met { state: u64, observed: Wall, guarded: bool },
    /// No sufficiently fresh final fact exists yet.
    Wait,
    /// A sufficiently fresh fact names a different state.
    Refuse { actual: Option<u64> },
}

/// One action in a seeded procedure's bounded program.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ProcedureAction {
    /// Ask for a checked effect with this kind and purpose.
    Effect { kind: u16, purpose: u64, target: u64 },
    /// Make a bounded batch of child tasks.
    Delegate { kinds: Box<[u16]> },
    /// Ask a person to approve the named proposal.
    Propose { number: u64 },
    /// Wait for a named fact to reach the state, no longer than the configured stall.
    Wait { state: u64 },
    /// Remain stalled until the caller delivers another message.
    Stall,
    /// Hold the task with a connector-owned reason.
    Hold { reason: u16 },
    /// Finish in jig's terms or with a connector-owned result.
    Finish { result: ProcedureResult },
}

/// A result a procedure hands to its task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProcedureResult {
    /// A generic success, carried directly to jig.
    Succeeded,
    /// Connector-owned result, kept by this connector for the task's brief.
    Local { code: u16 },
}

/// A seeded procedure program with a bound on decisions and waits.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProcedureSpec {
    /// Connector-local procedure number.
    pub number: u16,
    /// Its decisions in order.
    pub actions: Box<[ProcedureAction]>,
    /// Maximum number of steps before the task is held.
    pub max_steps: u16,
    /// Maximum time to wait for a fact.
    pub stall: Duration,
}

/// Durable procedure parameters and progress.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProcedureState {
    /// Task executing this procedure.
    pub task: u64,
    /// Configured procedure program.
    pub number: u16,
    /// Resource from which it reads its facts.
    pub resource: Path,
    /// Next action in that program.
    pub index: u16,
    /// Decisions committed so far.
    pub steps: u16,
    /// An effect or delegate was decided and awaits an answer.
    pub awaiting: bool,
    /// Absolute deadline while waiting for a fact.
    pub deadline: Option<Wall>,
}

/// What reached a procedure task; the next step reads state and current facts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProcedureSignal {
    Activate,
    Message,
    Settled,
    DelegateDone,
    ProposalDone,
    Cancel,
}

/// One decision of a procedure, sent through the root for authority checks.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum StepDecision {
    Effect(Effect),
    Delegate { kinds: Box<[u16]> },
    Propose { number: u64 },
    Wait { deadline: Wall },
    Stall,
    Hold { reason: u16 },
    Finish { result: ProcedureResult },
}

/// A read or brief section has a caller-supplied byte budget.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Read {
    pub resource: Path,
    pub size: u32,
}

/// One connector-owned workspace preparation instruction.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Item {
    pub resource: Path,
    pub writable: bool,
    pub state: Option<u64>,
}

/// The three ordered restart phases supplied by the root.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RestartStep {
    Records,
    FreshRead { resource: Path },
    SettleOutbox,
}

/// Configuration generated by a world from its seed.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Config {
    /// Identifier included in every effect key.
    pub deployment: u64,
    /// Deployment-owned path prefix.
    pub prefix: Path,
    /// Resources the fake system exposes.
    pub resources: Box<[ResourceSpec]>,
    /// Pools and their initial slot counts.
    pub pools: Box<[PoolSpec]>,
    /// Topics on which the system reports changes.
    pub topics: Box<[TopicSpec]>,
    /// Effect kinds and their recovery classes.
    pub kinds: Box<[KindSpec]>,
    /// Requirements judged from this connector's facts.
    pub requirements: Box<[RequirementSpec]>,
    /// Bounded procedure programs.
    pub procedures: Box<[ProcedureSpec]>,
}

/// News a subscribed task should hear.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Class {
    /// Wake a task that is waiting for news.
    Wake,
    /// Put news in the task's inbox without waking it.
    Keep,
}

/// News classified independently for one subscriber.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Classed {
    /// The subscribing task.
    pub task: u64,
    /// The subscriber's class for this news.
    pub class: Class,
}

/// Where a system hint came from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Origin {
    /// A change this connector made, echoed by the system.
    Own,
    /// A change by another hand.
    Other,
}

/// An event from the fake system beneath the connector.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SystemEvent {
    /// A fresh system read or hint for one resource.
    Fact { resource: Path, fact: Fact, origin: Origin },
    /// A pool changed size; named allocations no longer present drift.
    Pool { path: Path, slots: u32, lost: Box<[u64]> },
    /// A hint on a topic, with importance on a configured scale.
    News { topic: u16, importance: u16, origin: Origin },
    /// The result of a previously released write attempt.
    Applied { entry: u64, attempt: u32, result: ApplyResult },
    /// A lookup for an uncertain effect completed.
    Looked { entry: u64, looked: Looked },
}

/// What the root tells the connector for this part of its contract.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Event {
    /// Ask the connector to judge the named state on its facts.
    Judge { token: Token, requirement: u16, resources: Box<[Path]>, state: u64 },
    /// Create, wake or answer a connector-owned procedure task.
    Procedure { task: u64, number: u16, resource: Path, signal: ProcedureSignal },
    /// Serve an ephemeral read.
    Read { token: Token, read: Read },
    /// Gather a brief section for one task.
    Gather { token: Token, task: u64, budget: u32 },
    /// Cut a gathered section to a smaller size.
    Cut { token: Token, size: u32 },
    /// Gather workspace items for an assignment.
    Items { token: Token, task: u64 },
    /// Transfer a staged section or items exactly once.
    HandOver { token: Token },
    /// The root asks for one ordered recovery phase.
    Restart(RestartStep),
    /// A live task names resources.
    Names { task: u64, project: u32, resources: Box<[Path]> },
    /// A task no longer names resources.
    Unnamed { task: u64 },
    /// Add a resource to a project under the deployment's terms.
    Adopt { project: u32, resource: Path, role: ResourceRole },
    /// A task starts receiving a topic, with its own classification thresholds.
    Subscribe { task: u64, topic: u16, wake_at: u16, keep_at: u16 },
    /// A task stops receiving a topic.
    Unsubscribe { task: u64, topic: u16 },
    /// Describe and stage an effect until the core has checked it.
    Describe { token: Token, effect: Effect },
    /// Keep a checked effect under a deployment-scoped key.
    Keep { token: Token, entry: u64, task: u64, key: Key },
    /// Drop a staged effect after a refusal.
    Drop { token: Token },
    /// The journal released an entry after its first save committed.
    Make { entry: u64 },
    /// Cancel an entry that has never been sent.
    Withdraw { entry: u64 },
    /// Restore one previously committed record before work resumes.
    Restore { record: Record },
    /// The connector's system reported a change.
    System(SystemEvent),
}

/// A named resource's role and hold for the core's use.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Named {
    /// The resource.
    pub path: Path,
    /// The project's role there.
    pub role: ResourceRole,
    /// The hold the core takes, if any.
    pub hold: Hold,
}

/// The result of adopting a resource.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Adoption {
    /// The resource is available for this project.
    Added,
    /// The connector has no such resource.
    Unknown,
    /// The resource is outside this deployment's prefix.
    Outside,
    /// The system does not allow the requested role.
    Refused,
}

/// Stable identity of one connector record.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RecordKey {
    /// One procedure task's progress.
    Procedure(u64),
    /// Connector-owned result retained for a dependent's brief.
    Result(u64),
    /// Resources named by a live task.
    Task(u64),
    /// One project's adopted resource.
    Adoption { project: u32, path: Path },
    /// One task's topic subscription.
    Subscription { topic: u16, task: u64 },
    /// A pool's last known slots.
    Pool(Path),
    /// One unsettled outbox entry.
    Outbox(Key),
    /// An object made by this deployment.
    Made(Key),
}

/// Data owned by this connector and committed by its root.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Record {
    /// Parameters and state of one procedure task.
    Procedure(ProcedureState),
    /// A completed connector-owned result.
    Result { task: u64, code: u16 },
    /// A live task's names and project.
    Task { task: u64, project: u32, resources: Box<[Path]> },
    /// A project's resource role.
    Adoption { project: u32, path: Path, role: ResourceRole },
    /// A task's topic thresholds.
    Subscription { topic: u16, task: u64, wake_at: u16, keep_at: u16 },
    /// A pool's last observed slots.
    Pool { path: Path, slots: u32 },
    /// An unsettled effect and its last durable attempt.
    Outbox(OutboxEntry),
    /// An object this deployment made, named by its key.
    Made { key: Key, resources: Box<[Path]>, state: u64 },
}

impl Record {
    /// The key under which the parent keeps this record.
    #[must_use]
    pub fn key(&self) -> RecordKey {
        match self {
            Record::Procedure(state) => RecordKey::Procedure(state.task),
            Record::Result { task, .. } => RecordKey::Result(*task),
            Record::Task { task, .. } => RecordKey::Task(*task),
            Record::Adoption { project, path, .. } => RecordKey::Adoption { project: *project, path: path.clone() },
            Record::Subscription { topic, task, .. } => RecordKey::Subscription { topic: *topic, task: *task },
            Record::Pool { path, .. } => RecordKey::Pool(path.clone()),
            Record::Outbox(entry) => RecordKey::Outbox(entry.key),
            Record::Made { key, .. } => RecordKey::Made(*key),
        }
    }
}

/// What the connector asks its root to route or commit.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Request {
    /// An exact verdict, including the fact's observed time when met.
    Verdict { token: Token, verdict: Verdict },
    /// One bounded procedure decision.
    Step { task: u64, decision: StepDecision },
    /// A read answer leaves the connector immediately.
    Answer { token: Token, bytes: Box<[u8]> },
    /// A staged section's current byte size.
    Ready { token: Token, size: u32 },
    /// A section transferred to its brief once.
    Section { token: Token, bytes: Box<[u8]> },
    /// Workspace items transferred to an assignment once.
    Workspace { token: Token, items: Box<[Item]> },
    /// One observed change to an owned resource.
    DriftResource { tasks: Box<[u64]>, resource: Path },
    /// A changed fact may wake procedures that read this resource.
    Changed { resource: Path },
    /// Ordered restart phases have reached their final step.
    RestartDone,
    /// Description for the core's authority check.
    Described { token: Token, description: Description },
    /// A staged effect was refused by this connector.
    EffectRefused { token: Token },
    /// The root journals this release after saving the outbox entry.
    Make { entry: u64 },
    /// An effect's settled or uncertain outcome.
    Outcome { entry: u64, outcome: Outcome },
    /// A request to the fake system, held behind any accompanying save.
    System(SystemRequest),
    /// Roles and holds of one live task's named resources.
    Named { task: u64, resources: Box<[Named]> },
    /// A name was unknown or not adopted by its project.
    Unknown { task: u64, resource: Path },
    /// A task's request exceeds a configured bound.
    Refused { task: u64 },
    /// The adoption's result.
    Adopted { project: u32, resource: Path, result: Adoption },
    /// The pool's new capacity.
    Slots { pool: Path, slots: u32 },
    /// A holder's allocation disappeared while the hold remains its own.
    Drift { pool: Path, tasks: Box<[u64]> },
    /// A topic's news for every subscriber that kept or woke on it.
    News { topic: u16, subscribers: Box<[Classed]> },
    /// Save a record with the decision that produced it.
    Save { record: Record },
    /// Erase a record with the decision that removed it.
    Erase { key: RecordKey },
}
