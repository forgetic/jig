use alloc::boxed::Box;
use skein_lib::Token;

/// A service named by environment and service, in observability's terms.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Service {
    /// Environment containing the service.
    pub environment: Box<[u8]>,
    /// Service name within it.
    pub name: Box<[u8]>,
}

impl Service {
    /// Names a service with already validated bytes.
    #[must_use]
    pub fn new(environment: Box<[u8]>, name: Box<[u8]>) -> Self {
        Self { environment, name }
    }
}

/// A resource named by this connector for authority and subscriptions.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Resource {
    /// A service and its health.
    Service(Service),
    /// The service's log stream.
    Logs(Service),
    /// The service's metric stream.
    Metrics(Service),
    /// An alert rule.
    AlertRule(Box<[u8]>),
}

impl Resource {
    /// Segments of the connector-local resource name; the root adds its number.
    #[must_use]
    pub fn segments(&self) -> Box<[Box<[u8]>]> {
        match self {
            Resource::Service(service) => {
                Box::from([Box::<[u8]>::from(&b"services"[..]), service.environment.clone(), service.name.clone()])
            }
            Resource::Logs(service) => {
                Box::from([Box::<[u8]>::from(&b"logs"[..]), service.environment.clone(), service.name.clone()])
            }
            Resource::Metrics(service) => {
                Box::from([Box::<[u8]>::from(&b"metrics"[..]), service.environment.clone(), service.name.clone()])
            }
            Resource::AlertRule(rule) => Box::from([Box::<[u8]>::from(&b"rules"[..]), rule.clone()]),
        }
    }
}

/// A service's alert or health stream.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Topic {
    /// Alerts about this service.
    Alerts(Service),
    /// Changes to this service's health.
    Health(Service),
}

/// A time window whose endpoints are seconds on the system clock.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Window {
    /// Inclusive start.
    pub from: u64,
    /// Inclusive end.
    pub through: u64,
}

/// A bounded read available to an agent.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Read {
    /// Filtered log lines.
    Logs { service: Service, window: Window, filter: Box<[u8]>, max_bytes: u32 },
    /// Load and error samples.
    Series { service: Service, window: Window, max_bytes: u32 },
    /// Fired alerts.
    Fired { service: Service, window: Window, max_bytes: u32 },
}

/// A requirement judged on this connector's own facts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Requirement {
    /// A restart leaves one healthy replica elsewhere.
    HealthyReplicaElsewhere,
    /// Load has stayed below a percentage for the whole interval.
    LoadBelow { percent: u8, for_seconds: u64 },
}

/// A silence owned by the asking task and found again by its key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Effect {
    /// Alert rule to silence.
    pub rule: Box<[u8]>,
    /// End of the silence in system seconds.
    pub until: u64,
}

/// The origin that fixes an effect's purpose across retries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Purpose {
    /// One activation-qualified agent call.
    Call { attempt: u64, completion: u32, position: u32 },
    /// One stable procedure purpose.
    Procedure { purpose: u64 },
    /// One stable goal projection purpose.
    Projection { purpose: u64 },
}

/// Full deployment identity and the owner's stable effect purpose.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Key {
    /// Deployment that owns the operation.
    pub deployment: [u8; 16],
    /// Asking task.
    pub task: u64,
    /// The call, procedure or projection that asked.
    pub origin: Purpose,
    /// Stable purpose within that owner.
    pub purpose: u64,
}

impl Key {
    /// A procedure's stable purpose in one deployment.
    #[must_use]
    pub const fn procedure(deployment: [u8; 16], task: u64, purpose: u64) -> Self {
        Self { deployment, task, origin: Purpose::Procedure { purpose }, purpose }
    }
}

/// One sample used to judge sustained load.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LoadPoint {
    /// System time in seconds.
    pub at: u64,
    /// Load as a percentage.
    pub percent: u8,
}

/// Facts from one bounded service read.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Fact {
    /// System time of the read.
    pub observed: u64,
    /// Number of healthy replicas.
    pub healthy_replicas: u32,
    /// Current error count.
    pub errors: u32,
    /// Error rate as a percentage of requests in the latest sample.
    pub error_rate_percent: u8,
    /// Current load.
    pub load_percent: u8,
    /// Samples spanning a configured observation window.
    pub load: Box<[LoadPoint]>,
}

/// One alert received from the system.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Alert {
    /// Number unique within the deployment.
    pub number: u64,
    /// Service that raised it.
    pub service: Service,
    /// Severity set by this connector's classification rules.
    pub severity: u8,
}

/// How a subscriber receives news.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Class {
    /// Wake its task.
    Wake,
    /// Keep the news in its inbox without waking.
    Keep,
}

/// One subscriber's independent classification.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Classed {
    /// Durable core subscription that owns this interest.
    pub subscription: u64,
    /// Subscribed task.
    pub task: u64,
    /// What that task receives.
    pub class: Class,
}

/// A standing watch's durable template and last emitted batch.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Watch {
    /// Procedure task that owns the watch.
    pub task: u64,
    /// Services it watches.
    pub services: Box<[Service]>,
    /// Number of the triage template in the application.
    pub template: u16,
    /// Last wake batch for which a triage was asked.
    pub last_batch: Option<u64>,
    /// Alert identities retained until their batched wake is delegated.
    pub pending: Box<[u64]>,
}

/// Whether a requirement holds on fresh facts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Verdict {
    /// Facts observed at this system time met it.
    Met { observed: u64 },
    /// Facts are stale, absent or incomplete; a refresh was requested.
    Wait,
    /// Fresh facts failed the requirement.
    Refuse,
}

/// What became of a keyed silence.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Outcome {
    /// The silence was made or found by its key.
    Made,
    /// The system refused the request.
    Failed,
    /// The answer was lost and lookup must settle it.
    Uncertain,
}

/// Durable phase of a silence in the connector's outbox.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Phase {
    /// Saved but not yet sent.
    Kept,
    /// An attempt was committed for release.
    Sent,
    /// An answer was lost; lookup follows.
    Uncertain,
    /// The attempt bound was reached; a person must settle it.
    Held,
}

/// One durable outbox entry.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Entry {
    /// Core-owned outbox number, separate from the stable effect key.
    pub number: u64,
    /// Stable effect key.
    pub key: Key,
    /// The silence's value kept by this connector.
    pub effect: Effect,
    /// Last attempt number.
    pub attempt: u32,
    /// Absolute system time after which an uncertain attempt may be retried.
    pub deadline: u64,
    /// Current durable phase.
    pub phase: Phase,
}

/// Key of a record committed by the root.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RecordKey {
    /// One subscriber to a topic.
    Subscription(Topic, u64),
    /// One standing watch.
    Watch(u64),
    /// One unsettled silence.
    Outbox(Key),
}

/// A record owned by this connector and committed by the root.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Record {
    /// A subscriber's thresholds.
    Subscription { topic: Topic, task: u64, subscription: u64, wake_at: u8, keep_at: u8 },
    /// A standing watch's progress.
    Watch(Watch),
    /// An unsettled keyed silence.
    Outbox(Entry),
}

impl Record {
    /// Its stable store key.
    #[must_use]
    pub fn key(&self) -> RecordKey {
        match self {
            Record::Subscription { topic, task, .. } => RecordKey::Subscription(topic.clone(), *task),
            Record::Watch(watch) => RecordKey::Watch(watch.task),
            Record::Outbox(entry) => RecordKey::Outbox(entry.key),
        }
    }
}

/// An event routed to the observability connector.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Event {
    /// Settle this task before acknowledging its closing barrier.
    Close { task: u64 },
    /// Release this task's connector-owned live state.
    Release { task: u64 },
    /// Ask for an observed verdict within the stated freshness in seconds.
    Judge { token: Token, requirement: Requirement, service: Service, freshness: u64 },
    /// Serve a bounded agent read.
    Read { token: Token, read: Read },
    /// Subscribe one task to a topic, with its own thresholds.
    Subscribe { task: u64, topic: Topic, wake_at: u8, keep_at: u8 },
    /// Bind the watch's alert interests to its creator-authorized core subscription.
    LinkWatch { task: u64, subscription: u64 },
    /// Stop delivering a topic to one task.
    Unsubscribe { task: u64, topic: Topic },
    /// Activate a standing watch subscribed to these services.
    StartWatch { task: u64, services: Box<[Service]>, template: u16 },
    /// Cancel a standing watch and its alert subscriptions.
    StopWatch { task: u64 },
    /// A batched wake of a standing watch.
    WakeWatch { task: u64, batch: u64, alerts: Box<[u64]> },
    /// Describe and stage a silence pending the core's authority check.
    Describe { token: Token, effect: Effect },
    /// Keep an approved silence in the outbox.
    Keep { token: Token, entry: u64, key: Key },
    /// Discard a refused staged silence.
    Drop { token: Token },
    /// The journal released the kept entry.
    Make { key: Key },
    /// Make the entry named by the core after its commit.
    MakeEntry { entry: u64 },
    /// Restore a committed record during restart.
    Restore { record: Record },
    /// Refresh the services named by restored subscriptions and watches.
    ReadAfresh,
    /// Settle restored entries after all live reads have answered.
    Restart,
    /// A system result or hint.
    System(SystemEvent),
}

/// Events supplied by the production system beneath this connector.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SystemEvent {
    /// Fresh facts for one service.
    Fact { service: Service, fact: Fact },
    /// An alert; own echoes are dropped.
    Alert { alert: Alert, own: bool },
    /// A read's bounded answer.
    ReadDone { token: Token, bytes: Box<[u8]> },
    /// An attempted silence's answer.
    Applied { key: Key, attempt: u32, outcome: Outcome },
    /// A lookup of a keyed silence.
    Found { key: Key, found: bool },
}

/// Calls released to the production system only after the root's commit.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SystemRequest {
    /// Refresh a service's health and metric history.
    Facts { service: Service },
    /// Read a bounded stream.
    Read { token: Token, read: Read },
    /// Apply a keyed silence.
    Silence { key: Key, attempt: u32, effect: Effect },
    /// Look for a keyed silence after uncertainty.
    FindSilence { key: Key, rule: Box<[u8]> },
}

/// What the connector asks its root to route or commit.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Request {
    /// This task has no unsettled entries.
    Closed { task: u64 },
    /// This task's live state was released.
    Released { task: u64 },
    /// The connector finished its selected restart stage.
    Restarted { stage: RestartStage },
    /// An observed requirement verdict.
    Verdict { token: Token, verdict: Verdict },
    /// A read answer for a tool, bounded by the requested size.
    Answer { token: Token, bytes: Box<[u8]> },
    /// An alert classified per subscriber.
    News { alert: Alert, subscribers: Box<[Classed]> },
    /// A health change classified per subscriber.
    Health { service: Service, healthy: bool, subscribers: Box<[Classed]> },
    /// A watch asks for a triage task from its template and wake batch.
    Triage { watch: u64, template: u16, batch: u64, alerts: Box<[u64]>, delegate: Box<[u8]> },
    /// A description for the core's authority check.
    Described { token: Token, effect: Effect },
    /// A refused staged effect or invalid read.
    Refused { token: Token },
    /// A committed outbox entry ready for journal release.
    Make { key: Key },
    /// Outcome of a silence for its asking task.
    Outcome { entry: u64, key: Key, outcome: Outcome },
    /// One durable record to commit.
    Save { record: Record },
    /// One durable record to erase.
    Erase { key: RecordKey },
    /// A system request held by the root's journal.
    System(SystemRequest),
    /// A changed fact may wake procedures that read it.
    Changed { service: Service },
}

/// The connector-owned completion of a core-selected restart hand-off.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RestartStage {
    /// Every live resource has been read afresh.
    ReadAfresh,
    /// Every outbox entry is settled or explicitly held.
    Outbox,
}

/// The watch's configured delegate request, encoded in the application's tool vocabulary.
/// The connector chooses the template; the root only decodes its hand-off.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TriageTemplate {
    /// Identity named by a watch.
    pub number: u16,
    /// One bounded delegate tool input.
    pub delegate: Box<[u8]>,
}
