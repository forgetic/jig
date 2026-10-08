//! Owned questions and answers (domain/authority.md, sections 8 and 9).

use alloc::boxed::Box;

use skein_lib::Wall;

use crate::{Authority, Executor, Judge, Name, Numbers, ProposalKind, ResourceScope, Tools};

/// Ordered from least to most strict: deciding never clears a fact failure.
/// Terminal result of one pure check, ordered by severity; it neither executes the action nor
/// commits a durable decision. (domain/authority.md, section 8).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Answer {
    /// Checks permit the action; the caller still owns execution and atomic commitment.
    Allow,
    /// Required facts, budget, account or writer condition are not ready; no action is authorized
    /// yet.
    Wait,
    /// Task authority is insufficient but hard ceilings permit seeking an eligible accepter.
    Propose,
    /// A hard requirement, admission bound or permission failed.
    Refuse,
}

/// Authority layer that supplied a ceiling or permission for a finding. (domain/authority.md,
/// sections 6 and 8).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Source {
    /// The acting or creating task's authority.
    Task,
    /// The root-verified person's project role.
    Role,
    /// The live project's ceiling.
    Project,
    /// Immutable deployment rules.
    Deployment,
}

/// Fixed-size reason emitted by a pure check into caller-reserved bounded room; multiple
/// independent reasons may accompany one answer. (domain/authority.md, sections 8–10).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Finding {
    /// Input shape or payload exceeds admission limits.
    Oversized,
    /// No live policy exists for the named project.
    UnknownProject,
    /// The live policy has no named role.
    UnknownRole,
    /// Componentwise authority deficits against this source.
    Authority {
        /** Authority layer against which this condition failed. */
        source: Source,
        /** Independent component deficits against that source. */
        lacks: crate::Lacks,
    },
    /// A child executor is not permitted by this source.
    Executor {
        /** Authority layer against which this condition failed. */
        source: Source,
    },
    /// Aggregate lifetime task capacity does not fit this source.
    Tasks {
        /** Authority layer against which this condition failed. */
        source: Source,
    },
    /// Aggregate reserved spend does not fit this source.
    Spend {
        /** Authority layer against which this condition failed. */
        source: Source,
    },
    /// Required accounting or aggregate arithmetic is unrepresentable.
    Arithmetic,
    /// Offered run budget is not above the minimum or available funding is insufficient.
    RunBudget,
    /// Offered run budget exceeds deployment or task authority cap.
    RunCap,
    /// Task deadline is past the supplied wall time.
    Deadline,
    /// A required model account is unusable.
    Account,
    /// A written resource is held by another task or has a pending hold.
    Writer,
    /// Configured family is absent from an applicable authority value.
    Tool,
    /// This source has no grant covering the requested operation.
    Grant {
        /** Authority layer against which this condition failed. */
        source: Source,
    },
    /// The effect's maximum price exceeds funding available at this source.
    Price { source: Source },
    /// The adopted resource is context only or current access forbids a write.
    ResourceAccess,
    /// A connector reports a resource unavailable in the project.
    ResourceUnavailable,
    /// The caller has no verified standing to message the task.
    Reference,
    /// The note scope is absent from this source.
    Scope {
        /** Authority layer against which this condition failed. */
        source: Source,
    },
    /// A pinned required fact is missing, unknown or pending.
    Required { judge: Judge },
    /// A matching pinned required fact failed.
    Failed { judge: Judge },
    /// The effect kind cannot guard a requirement marked as requiring a guard.
    Unguarded { judge: Judge },
    /// The person's role does not allow this request kind.
    Unpermitted,
    /// The person's role may not accept this proposal kind.
    Undecidable,
    /// A funding request's pool budget exceeds the role's period ceiling.
    PeriodSpend,
}

/// Pure check result and optional replacement funding snapshot; the root commits allowed numbers
/// together with the action once. (domain/authority.md, sections 7–8).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Checked {
    /// Strictest answer after all applicable checks.
    pub answer: Answer,
    /// Replacement funding numbers only on `Allow`; other answers carry `None` and authorize no
    /// debit.
    pub numbers: Option<Numbers>,
}

/// Root-translated directly created task's executor and complete authority, admitted under batch
/// and authority limits. (domain/authority.md, section 8.1).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Delegate {
    /// Executor permission that the creator must have.
    pub executor: Executor,
    /// Complete child authority, bounded by the configured authority limits.
    pub authority: Authority,
    /// Open-prefix grants narrowed with this delegate's number after the batch is admitted.
    pub symbolic: Box<[crate::Grant]>,
}

/// Root-gathered creation question over one coherent creator and funding snapshot; no tasks or
/// reservations are retained here. (domain/authority.md, section 8.1).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BatchAsk {
    pub project: u32,
    /// Creator's current authority, bounded by `Limits`.
    pub creator: Authority,
    /// Creator's current allotment snapshot, not a copy of its static ceiling.
    pub numbers: Numbers,
    /// Lifetime task capacity still available, kept separately by tasks. Current lifetime task
    /// capacity available from the tasks ledger.
    pub tasks_left: u32,
    /// Whole all-or-nothing batch, bounded by `Limits::batch`; each child costs one slot plus its
    /// descendant capacity.
    pub tasks: Box<[Delegate]>,
}

/// A read or effect of one connector, pinned to the state it names.
/// Root-translated connector read or effect with a literal resource and exact state pin, bounded on
/// admission. (domain/authority.md, section 8.2).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Effect {
    pub connector: u16,
    /// Requested exact kind; grant implications do not remove requirements on this kind.
    pub kind: u16,
    /// Full literal resource name, bounded by segment and byte limits.
    pub name: Name,
    pub state: [u8; 32],
    /// Maximum deployment-unit charge, posted in the effect's deciding commit.
    pub price: Option<u64>,
    /// Connector-verified current access to the resource and object being written.
    pub access: EffectAccess,
    /// Other resources touched by this same effect, each with its own current access.
    pub additional: Box<[EffectResource]>,
    /// Judges this effect kind's condition checks when it is applied.
    pub guards: Box<[Judge]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EffectResource {
    pub name: Name,
    pub access: EffectAccess,
}

/// A connector's current write relationship for this exact effect. Participating writes are
/// permitted as a participant, subject to the same grant and requirement checks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EffectAccess {
    Owned,
    Participant,
    Context,
    Unavailable,
}

/// Root-gathered permission and authentic-fact question for one pinned effect; this value makes no
/// connector call. (domain/authority.md, section 8.2).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EffectAsk {
    pub project: u32,
    /// Acting task or accepter authority, admitted under `Limits`.
    pub authority: Authority,
    /// Acting task or accepter's current four funding numbers.
    pub numbers: Numbers,
    /// Single pinned connector operation being checked.
    pub effect: Effect,
    /// Wall time of this decision, used for observed verdict freshness.
    pub now: Wall,
}

/// A connector's judgement for the exact state of the checked effect.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Verdict {
    /// The requirement held when judged.
    Met,
    /// The judge cannot yet say whether it holds.
    Wait,
    /// The requirement failed.
    Refuse,
}

/// One named judgement and the wall time at which its facts were observed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Given {
    pub judge: Judge,
    pub verdict: Verdict,
    pub at: Wall,
    /// The exact effect state the connector was asked to judge.
    pub state: [u8; 32],
}

/// Root-verified writer-hold relationship for a run's written resource; authority performs no
/// ownership discovery. (domain/authority.md, section 8.3).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Writer {
    /// Hold belongs to the running task.
    Task,
    /// Hold belongs to a root-verified ancestor of the running task.
    Ancestor,
    /// Hold is unresolved; run admission waits.
    Pending,
    /// Another task holds the writer; run admission waits.
    Other,
}

/// One workspace resource and its root-verified writer relationship, admitted under run write/name
/// limits. (domain/authority.md, section 8.3).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Write {
    /// Written resource and required connector kind, including saved workspace writes.
    pub effect: Effect,
    /// Root-verified current writer hold.
    pub held: Writer,
}

/// Root-gathered run admission snapshot; no claim, worker, clock or account state is held by this
/// question. (domain/authority.md, section 8.3).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RunAsk {
    pub project: u32,
    /// Current task authority, bounded by `Limits`.
    pub authority: Authority,
    /// Current funding snapshot used to compute available run spend.
    pub numbers: Numbers,
    /// Offered run budget in the deployment unit; must exceed the minimum and fit all caps.
    pub budget: u64,
    /// Root-supplied current wall time for the deadline check.
    pub wall: Wall,
    /// One status for each model account its charter will use. One usability status per model
    /// account used by the charter, bounded by `Limits::accounts`.
    pub accounts: Box<[bool]>,
    /// Every resource its workspace will write, including saved work. Every written resource,
    /// including saved work, bounded by `Limits::writes`.
    pub writes: Box<[Write]>,
}

/// Root-translated person action after membership verification; authority returns a decision over
/// values, not task mutations. (domain/authority.md, section 8.4).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PersonRequest {
    /// Check role permission and all-or-nothing batch funding.
    Create(
        /** Whole directly created batch, bounded by `Limits::batch` and each child's authority limits. */
        Box<[Delegate]>,
    ),
    /// The complete future authority/allotment to fund, not an unchecked delta. Check role
    /// permission and the complete future allotment.
    Allot(/** Complete future authority/allotment to reserve, bounded by authority limits. */ Authority),
    /// Check role acceptance and rights for one proposed action.
    Accept(/** Single proposed action; effect acceptance still needs its pinned-fact check. */ Action),
    /// What the amendment must newly give; a narrowing gives empty authority. Check role permission
    /// and authority newly given by the amendment.
    Amend(
        /** Authority the amendment newly gives, bounded by authority limits; narrowing supplies empty authority. */
        Authority,
    ),
    /// Check role permission and the authority/allotment funding the move.
    Move(/** Complete authority/allotment to fund for the moved task, bounded by authority limits. */ Authority),
    /// Check specific cancellation permission; does not cancel here.
    Cancel,
    /// Check specific release permission; does not release here.
    Release,
    /// Check specific watch permission; does not create a view here.
    Watch,
    /// Check specific policy-change permission; does not replace policy here.
    Policy,
}

/// Root-gathered person-role question with the funding snapshot and separate available
/// lifetime capacity. (domain/authority.md, section 8.4).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PersonAsk {
    pub project: u32,
    pub role: u32,
    /// Person-period pool snapshot; funding requests check its budget against role period
    /// spend.
    pub pool: Numbers,
    /// Current lifetime task capacity available to this funding decision.
    pub tasks_left: u32,
    /// Single translated action checked for specific role permission.
    pub request: PersonRequest,
}

/// Root-translated tool-call requirements beyond its configured family bit; serving the call
/// remains with the root. (domain/authority.md, section 8.5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Call {
    /// Family permission alone is checked here.
    Tool,
    /// Family permission and resource read grant are checked.
    Read(/** Pinned resource read requiring a covered grant and bounded name. */ Effect),
    /// Family permission and root-verified task reference are checked.
    Message {
        /** Whether the root verified a reference giving standing to message this task. */
        referenced: bool,
    },
    /// Family permission and one supported scope are checked.
    Note(/** Exactly one note scope, including a connector resource pattern. */ NoteScope),
}

/// Where one note write applies, relative to the acting task and project.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum NoteScope {
    /// The acting task's goal.
    Goal,
    /// The task's project.
    Project,
    /// The deployment.
    Deployment,
    /// One connector's resource pattern.
    Resources(ResourceScope),
}

/// Root-gathered call permission question over one configured family and admitted
/// authority/resource values. (domain/authority.md, section 8.5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CallAsk {
    pub project: u32,
    /// Current caller authority, admitted under `Limits`.
    pub authority: Authority,
    /// Exactly one family bit, assigned by configuration. Exactly one tool-family bit selected by
    /// the root's registry; other shapes refuse.
    pub family: Tools,
    pub call: Call,
}

/// Owned proposed action used for needs and acceptance checks; caller bounds its payloads and
/// commits acceptance with execution. (domain/authority.md, section 9).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    /// Propose an all-or-nothing creation batch.
    Batch(/** Whole delegated batch, bounded by batch and authority limits. */ Box<[Delegate]>),
    /// Propose one exact pinned connector effect.
    Effect(/** One pinned connector effect with a bounded resource name. */ Effect),
    /// Propose wider authority for a task.
    Widen(/** Complete requested wider authority, bounded at admission. */ Authority),
    /// Propose authority newly needed by an amendment.
    Amend(/** Authority newly needed by the amendment, bounded at admission. */ Authority),
    /// Topology grants standing; release may additionally need authority. Propose release based on
    /// standing and any additionally needed authority.
    Escalate {
        /// Authority newly needed for release, or `None` when standing alone suffices.
        release: Option<Authority>,
    },
}

/// Root-verified eligible proposal accepter with current funding and task capacity; standing and
/// distance are not discovered here. (domain/authority.md, section 9).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Holder {
    /// The caller has verified that this task stands above the proposer. Ancestor task whose
    /// standing the caller has verified.
    Task {
        project: u32,
        /// Ancestor's admitted current authority.
        authority: Authority,
        /// Ancestor's current funding snapshot.
        numbers: Numbers,
        /// Ancestor's remaining lifetime task capacity.
        tasks_left: u32,
    },
    /// Person whose role membership and proposal standing the root has verified.
    Person {
        project: u32,
        role: u32,
        /// Proposal kind this role must be allowed to decide.
        proposal: ProposalKind,
        /// Person-period funding snapshot.
        pool: Numbers,
        /// Current lifetime task capacity available to acceptance.
        tasks_left: u32,
    },
}
