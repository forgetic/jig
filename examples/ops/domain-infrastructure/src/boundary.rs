use alloc::boxed::Box;
use skein_lib::Token;

/// A service in one environment.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Service {
    /// Environment name.
    pub environment: Box<[u8]>,
    /// Service within it.
    pub name: Box<[u8]>,
}

impl Service {
    /// Names one service with validated bytes.
    #[must_use]
    pub fn new(environment: Box<[u8]>, name: Box<[u8]>) -> Self {
        Self { environment, name }
    }
}

/// An environment created in one pool.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Environment {
    /// Pool whose slot it occupies.
    pub pool: Box<[u8]>,
    /// Environment name within the pool.
    pub name: Box<[u8]>,
}

impl Environment {
    /// Names one environment with validated bytes.
    #[must_use]
    pub fn new(pool: Box<[u8]>, name: Box<[u8]>) -> Self {
        Self { pool, name }
    }
}

/// A named pool of environment slots.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Pool(pub Box<[u8]>);

/// Resource kinds this connector offers to tasks and policy.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Resource {
    /// An exclusive service being remediated.
    Service(Service),
    /// One environment made under a pool.
    Environment(Environment),
    /// A pool's counted slots.
    Pool(Pool),
    /// A pool's quota fact.
    Quota(Pool),
}

impl Resource {
    /// Segments of this connector's name; the root adds its connector number.
    #[must_use]
    pub fn segments(&self) -> Box<[Box<[u8]>]> {
        match self {
            Resource::Service(service) => Box::from([
                Box::<[u8]>::from(&b"env"[..]),
                service.environment.clone(),
                Box::<[u8]>::from(&b"service"[..]),
                service.name.clone(),
            ]),
            Resource::Environment(environment) => Box::from([
                Box::<[u8]>::from(&b"pool"[..]),
                environment.pool.clone(),
                Box::<[u8]>::from(&b"environment"[..]),
                environment.name.clone(),
            ]),
            Resource::Pool(pool) => Box::from([Box::<[u8]>::from(&b"pool"[..]), pool.0.clone()]),
            Resource::Quota(pool) => Box::from([Box::<[u8]>::from(&b"quota"[..]), pool.0.clone()]),
        }
    }
}

/// What the core takes for a resource.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Hold {
    /// One task at a time; a taken service waits.
    ExclusiveWait,
    /// One of the pool's counted slots; a full pool waits.
    PooledWait,
    /// Reads need no hold.
    None,
}

/// A resource and its hold.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Named {
    /// Resource named by the task.
    pub resource: Resource,
    /// Hold the core takes.
    pub hold: Hold,
}

/// A stable deployment-scoped effect key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Key {
    /// Deployment that owns the operation.
    pub deployment: u64,
    /// Asking task.
    pub task: u64,
    /// Stable purpose within the task.
    pub purpose: u64,
}

/// Whether the infrastructure backend accepts restart operation IDs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Backend {
    /// IDs make uncertain restarts recoverable by key.
    OperationIds,
    /// Without IDs, an uncertain restart is held for a person.
    NoOperationIds,
}

/// A infrastructure effect.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Effect {
    /// Restart once by operation ID where supported.
    Restart { service: Service, operation: u64 },
    /// Change replicas only from the count decided from.
    Scale { service: Service, from: u32, to: u32 },
    /// Change version only from the version decided from.
    Rollback { service: Service, from: Box<[u8]>, to: Box<[u8]> },
    /// Create once by key, priced at its maximum.
    CreateEnvironment { environment: Environment, until: u64, price: u64 },
    /// Delete only the environment created under this key.
    TearDown { environment: Environment, created_by: Key },
}

/// What kind of state transition an effect makes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Form {
    /// Make a named object.
    Creation,
    /// Change from one state to another.
    Transition,
    /// Set one final value.
    Set,
}

/// How uncertainty is settled.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Recovery {
    /// Find the operation's stable key.
    Keyed,
    /// Retry only under the original condition.
    Conditional,
    /// No safe automatic retry.
    Unrecoverable,
}

/// What the root translates into jig's effect vocabulary.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Description {
    /// Connector-local effect kind number.
    pub kind: u16,
    /// Resources checked against authority and holds.
    pub resources: Box<[Resource]>,
    /// Whether the system checks the starting state when applying.
    pub condition: bool,
    /// Creation, transition or set.
    pub form: Form,
    /// Recovery offered by this backend.
    pub recovery: Recovery,
    /// Maximum cost, if this is priced.
    pub price: Option<u64>,
    /// State the requirement judges are asked about.
    pub state: u64,
}

/// Outcome of an infrastructure effect.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Outcome {
    /// Made, or found after an uncertain keyed attempt.
    Made,
    /// The system rejected the effect.
    Failed,
    /// A person must settle an uncertain outcome.
    Uncertain,
}

/// Result of a system apply call.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ApplyResult {
    /// The effect took place.
    Made,
    /// A checked starting state changed.
    Conflict,
    /// Its target is absent.
    Missing,
    /// No quota slot was available.
    Full,
    /// A known failure before the effect was sent.
    Error,
    /// The effect may have been made but its answer was lost.
    Uncertain,
}

/// What a recovery lookup can tell after uncertainty.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Looked {
    /// The keyed operation was found.
    Made,
    /// The original conditional state still holds, so retry may be safe.
    CanRetry,
    /// Another hand may have reached the target; automatic recovery cannot prove ownership.
    Ambiguous,
}

/// Durable outbox phase.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Phase {
    /// Saved, not yet sent.
    Kept,
    /// An attempt was committed for release.
    Sent,
    /// The answer was lost; lookup or deadline remains.
    Uncertain,
    /// A person must settle the effect.
    Held,
}

/// One durable entry in the connector's outbox.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Entry {
    /// Deployment-scoped key.
    pub key: Key,
    /// Connector-owned effect.
    pub effect: Effect,
    /// Attempt number.
    pub attempt: u32,
    /// Absolute system time when an uncertain attempt may be retried.
    pub deadline: u64,
    /// Current phase.
    pub phase: Phase,
}

/// Fresh service facts from infrastructure.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ServiceFact {
    /// Current version.
    pub version: Box<[u8]>,
    /// Current replicas.
    pub replicas: u32,
    /// Whether the service is healthy, as observed by infrastructure.
    pub healthy: bool,
    /// Revision changed by either hand.
    pub revision: u64,
    /// System time of the read.
    pub observed: u64,
}

/// Fresh environment facts from infrastructure.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EnvironmentFact {
    /// Creation key, if it exists.
    pub created_by: Key,
    /// System time when provisioning completes.
    pub ready_at: u64,
    /// Lease end.
    pub until: u64,
    /// Maximum price charged.
    pub price: u64,
}

/// Procedure parameters kept under a task's number.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Procedure {
    /// Remediate a service and wait for health.
    Remediate { service: Service, operation: u64, deadline: u64 },
    /// Create an environment and wait until it is ready.
    Provision { environment: Environment, until: u64, price: u64, deadline: u64 },
    /// Tear down an environment on release.
    TearDown { environment: Environment, created_by: Key, deadline: u64 },
}

/// Durable phase of one procedure.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProcedurePhase {
    /// Evaluate current facts and decide once.
    Active,
    /// An effect was asked for and is not settled.
    EffectOutstanding,
    /// Wait for a fact to reach the requested condition.
    WaitingCondition,
    /// Provisioned environment remains owned until task release.
    Holding,
    /// The task ended.
    Done,
    /// A bound or drift needs a person.
    Held,
}

/// One procedure task's durable parameters and progress.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProcedureState {
    /// Task executing it.
    pub task: u64,
    /// Its parameters.
    pub procedure: Procedure,
    /// Its progress.
    pub phase: ProcedurePhase,
}

/// Why a procedure was stepped.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProcedureSignal {
    /// Activated or asked again on current facts.
    Step,
    /// The effect it asked for was made.
    EffectMade,
    /// Its effect failed or was held.
    EffectFailed,
    /// The task is closing and releases what it held.
    Release,
}

/// One level-triggered procedure decision for the core.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum StepDecision {
    /// Ask for one effect.
    Effect(Effect),
    /// Wait on facts or an outstanding effect until this time.
    Wait { until: u64 },
    /// End successfully.
    Finish,
    /// Hold for a person with a connector-local reason.
    Hold { reason: u16 },
}

/// Stable key for a connector record.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RecordKey {
    /// Resources named by a task.
    Rely(u64),
    /// A procedure task's state.
    Procedure(u64),
    /// An unsettled effect.
    Outbox(Key),
    /// An environment this deployment made.
    Made(Environment),
}

/// State committed by the root with a decision.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Record {
    /// Resources a task relies on.
    Rely { task: u64, resources: Box<[Resource]> },
    /// One procedure's state.
    Procedure(ProcedureState),
    /// One unsettled effect.
    Outbox(Entry),
    /// An environment made under a key.
    Made { environment: Environment, key: Key },
}

impl Record {
    /// Its stable store key.
    #[must_use]
    pub fn key(&self) -> RecordKey {
        match self {
            Record::Rely { task, .. } => RecordKey::Rely(*task),
            Record::Procedure(state) => RecordKey::Procedure(state.task),
            Record::Outbox(entry) => RecordKey::Outbox(entry.key),
            Record::Made { environment, .. } => RecordKey::Made(environment.clone()),
        }
    }
}

/// Root-to-connector events.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Event {
    /// A live task names resources and needs their holds.
    Names { task: u64, resources: Box<[Resource]> },
    /// A task no longer relies on its named resources.
    Unnamed { task: u64 },
    /// Start a procedure with parameters.
    StartProcedure { task: u64, procedure: Procedure },
    /// Step a procedure on current facts.
    Procedure { task: u64, signal: ProcedureSignal },
    /// Stage an effect for the core's authority check.
    Describe { token: Token, effect: Effect },
    /// Keep an approved effect under its key.
    Keep { token: Token, key: Key },
    /// Drop a staged effect after refusal.
    Drop { token: Token },
    /// The journal released this kept entry.
    Make { key: Key },
    /// Restore one committed record.
    Restore { record: Record },
    /// Refresh live resources, then settle outbox entries.
    Restart,
    /// A system fact or answer.
    System(SystemEvent),
}

/// Calls to the fake production after the journal releases them.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SystemRequest {
    /// Read a service afresh.
    Service { service: Service },
    /// Read one environment afresh.
    Environment { environment: Environment },
    /// Read a pool's current capacity.
    Pool { pool: Pool },
    /// Apply an outbox attempt.
    Apply { key: Key, attempt: u32, effect: Effect },
    /// Look for an uncertain outcome.
    Look { key: Key, effect: Effect },
}

/// Answers and hints from infrastructure's system.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SystemEvent {
    /// Fresh service fact and whether another hand made this change.
    Service { service: Service, fact: ServiceFact, other_hand: bool },
    /// Fresh environment fact, or absence, and whether another hand removed it.
    Environment { environment: Environment, fact: Option<EnvironmentFact>, other_hand: bool },
    /// Current pool quota and slots used.
    Pool { pool: Pool, quota: u32, used: u32 },
    /// An apply answer.
    Applied { key: Key, attempt: u32, result: ApplyResult },
    /// Recovery lookup result.
    Looked { key: Key, result: Looked },
}

/// Requests to the root and its journal.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Request {
    /// Resources and holds a task must take atomically.
    Named { task: u64, resources: Box<[Named]> },
    /// Updated pool capacity; existing holders are retained.
    Slots { pool: Pool, quota: u32, used: u32 },
    /// Another hand changed an owned resource a task relies on.
    Drift { task: u64, resource: Resource },
    /// A fact changed and procedures should step again.
    Changed { resource: Resource },
    /// Description for the core's authority check.
    Described { token: Token, description: Description },
    /// An invalid effect was refused before authority.
    Refused { token: Token },
    /// A committed outbox entry ready to make after release.
    Make { key: Key },
    /// An outbox result.
    Outcome { key: Key, outcome: Outcome },
    /// One procedure decision.
    Step { task: u64, decision: StepDecision },
    /// Save a connector-owned record.
    Save { record: Record },
    /// Erase a connector-owned record.
    Erase { key: RecordKey },
    /// System call held behind the decision's commit.
    System(SystemRequest),
}
