//! Task-owned semantic authority and accounting carriers
//! (domain/tasks.md, sections 2–3; domain/authority.md, sections 3–7).
//! Root translates permission values for pure authority checks. Tasks owns
//! concrete financial counters and admits bounded payload shape; it neither
//! interprets connector names nor serves tools or implements a move route.

use alloc::boxed::Box;

use skein_lib::Wall;

/// Task-owned permission carrier supplied by the root after authority checks; tasks admits its
/// bounded shape but does not judge permission inclusion. (domain/tasks.md, sections 2–3).
/// (domain/authority.md, sections 3–7).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Authority {
    /// Configured permitted tool-family bits; no tool is served by tasks.
    pub tools: Tools,
    /// Owned resource grants, bounded by `Limits::authority_grants` and the aggregate authority-
    /// byte limit.
    pub grants: Box<[Grant]>,
    /// `Executor`, lifetime task-count and depth ceiling supplied after the root's policy check.
    pub delegation: Delegation,
    /// Static spend/time permission, distinct from current accounting.
    pub budget: Budget,
    /// Permitted note-scope bits carried without judging them here.
    pub notes: Scopes,
    /// Connector resource patterns where this task may write notes.
    pub note_resources: Box<[ResourceScope]>,
}

/// Root-configured tool-family bit set carried with a task, independent of authority's vocabulary.
/// (domain/tasks.md, sections 2–3). (domain/authority.md, sections 3–7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Tools(/** Up to 64 configured family bits; tasks assigns no family meanings. */ pub u64);

/// Task-relative note-scope bits carried as semantic data; the authority child checks scope
/// permission through the root. (domain/tasks.md, sections 2–3). (domain/authority.md,
/// sections 3–7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Scopes(/** Goal 1, project 2 and deployment 4 scope bits. */ pub u8);

impl Scopes {
    /// Goal-relative note-scope bit carried with authority.
    pub const GOAL: Scopes = Scopes(1);

    /// Project-relative note-scope bit carried with authority.
    pub const PROJECT: Scopes = Scopes(2);

    /// Deployment-relative note-scope bit carried with authority.
    pub const DEPLOYMENT: Scopes = Scopes(4);
}

/// One connector resource pattern carried with a task's note permissions.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ResourceScope {
    pub connector: u16,
    pub pattern: Pattern,
}

/// Owned connector permission carried by tasks; connector interpretation and authority decisions
/// remain outside this child. (domain/tasks.md, sections 2–3). (domain/authority.md,
/// sections 3–7).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Grant {
    pub connector: u16,
    /// Granted connector kind; implication checks belong to authority.
    pub kind: u16,
    /// Covered resources, bounded under the task authority shape limits.
    pub pattern: Pattern,
}

/// Literal base segments and terminal coverage rule carried without connector parsing or
/// normalization. (domain/tasks.md, sections 2–3). (domain/authority.md, sections 3–7).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Pattern {
    /// `Exact` literal base segments, bounded per grant by `Limits::authority_segments`; bytes
    /// count toward the aggregate authority-byte limit.
    pub segments: Box<[Box<[u8]>]>,
    /// Terminal rule whose bytes also count toward the aggregate authority-byte limit.
    pub last: Last,
}

/// Terminal resource coverage carrier; tasks bounds bytes while authority interprets coverage.
/// (domain/tasks.md, sections 2–3). (domain/authority.md, sections 3–7).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Last {
    /// Requires one exact additional segment alone, as interpreted by authority.
    Exact(/** Literal additional segment; its bytes count toward `Limits::authority_bytes`. */ Box<[u8]>),
    /// Requires an additional segment with this prefix and covers its descendants, as interpreted
    /// by authority.
    Open(/** Literal prefix of an additional segment; its bytes count toward `Limits::authority_bytes`. */ Box<[u8]>),
}

/// Permitted executor identities carried in authority; these are not additional implemented task
/// execution routes. (domain/tasks.md, sections 2–3). (domain/authority.md, sections 3–7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AuthorityExecutor {
    /// Permission to delegate to an agent charter.
    Charter(u32),
    /// `Procedure` permission carrier; the contracted task API exposes agent execution only.
    Procedure(/** `Procedure` permission number; not a current task `Executor` variant. */ u32),
    /// Person-role permission carrier; the contracted task API exposes agent execution only.
    Role(/** Project-role permission number; not a current task `Executor` variant. */ u32),
}

/// `Authority`'s permitted executors, lifetime task capacity and depth carried with a task; current
/// topology limits are checked separately. (domain/tasks.md, sections 2–3).
/// (domain/authority.md, sections 3–7).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Delegation {
    /// Permitted executor entries, bounded by `Limits::executor_kinds`.
    pub kinds: Box<[AuthorityExecutor]>,
    /// Lifetime descendant task allowance; it is not the current live delegate count.
    pub tasks: u32,
    /// Maximum descendant depth carried as permission, distinct from the child's structural depth
    /// bound.
    pub depth: u32,
}

/// Task's permission ceiling for deployment-unit spend and latest wall time; available `funding` is
/// tracked separately in `Numbers`. (domain/tasks.md, sections 2–3). (domain/authority.md,
/// sections 3–7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Budget {
    /// Maximum allotment in the root-configured deployment spending unit.
    pub spend: u64,
    /// Latest permitted ending wall time; `None` denotes no finite deadline.
    pub deadline: Option<Wall>,
}

/// Concrete accounting of one current allotment, owned by tasks: direct spend, settled funded spend
/// and still-open reservations. (domain/tasks.md, sections 2–3). (domain/authority.md,
/// sections 3–7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Numbers {
    /// Full amount of this allotment reserved against its funder.
    pub budget: u64,
    /// Direct expense posted here, within this allotment's budget.
    pub spent: u64,
    /// Expense of funded allotments already settled here, posted once when each task ends.
    pub spent_below: u64,
    /// Full budgets of directly funded allotments still open; availability is not recreated by
    /// opening another period.
    pub reserved: u64,
}

/// Financial source, distinct from requester topology; original period identities survive
/// later period openings. (domain/tasks.md, sections 2–3). (domain/authority.md, sections
/// 3–7).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Funder {
    /// Financial reservation against a live task in the creator's verified ancestry.
    Task(/** `Live` `funding` task number; the financial parent may differ from the requester. */ u64),
    /// Financial reservation against a retained person's project-period pool.
    Pool {
        project: u32,
        /** Person whose original project-period pool funds the task. */
        person: u64,
        /** Original project period of the pool, unchanged by later openings. */
        period: u64,
    },
    /// Per-period allotment of a live core recurring procedure, from its project's period.
    Recurring { project: u32, task: u64, period: u64 },
    /// Financial reservation directly against a retained project period.
    Period {
        project: u32,
        /** Monotonically opened period identity within that project. */
        period: u64,
    },
}
