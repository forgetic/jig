//! Owned authority data (domain/authority.md, sections 3 and 4).

use alloc::boxed::Box;

use skein_lib::Wall;

/// What a task may do. Each collection is bounded before the root admits it.
/// Root-supplied permission value, ordered componentwise; checked inputs must fit the configured
/// collection and byte limits. (domain/authority.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Authority {
    pub tools: Tools,
    /// Resource grants, bounded by `Limits::grants` at admission.
    pub grants: Box<[Grant]>,
    /// Executor, lifetime task-count and depth ceiling.
    pub delegation: Delegation,
    /// Spend and wall-time ceiling, rather than current available funding.
    pub budget: Budget,
    pub notes: Scopes,
    /// Connector resource patterns on which this task may write notes.
    pub note_resources: Box<[ResourceScope]>,
}

/// Up to 64 tool families, one bit each. Configuration assigns the bits,
/// including each connector's read family; the root translates them.
/// Configuration-selected tool-family bits; authority interprets no connector-specific family
/// names. (domain/authority.md, sections 3–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Tools(/** Up to 64 configured family bits; inclusion is a bit-set subset test. */ pub u64);

/// Note scopes relative to the task: goal, project and deployment.
/// Connector resource patterns are carried in `Authority::note_resources`.
/// (domain/authority.md, sections 3–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Scopes(/** Scope bits: goal 1, project 2 and deployment 4. */ pub u8);

impl Scopes {
    /// Goal-relative note scope.
    pub const GOAL: Scopes = Scopes(1);

    /// Project-relative note scope.
    pub const PROJECT: Scopes = Scopes(2);

    /// Deployment-relative note scope.
    pub const DEPLOYMENT: Scopes = Scopes(4);
}

/// One connector resource pattern permitted as a note scope.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ResourceScope {
    pub connector: u16,
    pub pattern: Pattern,
}

/// An effect or read of one connector's kind on the resources a pattern covers.
/// Permission for one connector's kind and the resources covered by its pattern.
/// (domain/authority.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Grant {
    pub connector: u16,
    /// Granted kind, including configured implied kinds.
    pub kind: u16,
    /// Covered resources, bounded by segment and byte limits.
    pub pattern: Pattern,
}

/// A connector's resource path. Bytes and segment boundaries are literal:
/// empty segments are permitted, with no parsing or normalization here.
/// Owned literal resource segments; callers admit segment count and bytes before value queries
/// that do not perform admission themselves. (domain/authority.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Name {
    /// Literal ordered segments, bounded by `Limits::segments` and `Limits::segment_bytes` when
    /// admitted.
    pub segments: Box<[Box<[u8]>]>,
}

/// Exact base segments, followed by the terminal's matching rule.
/// Owned resource coverage value; base and terminal bytes have the configured name limits.
/// (domain/authority.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Pattern {
    /// Exact base segments, bounded by the configured segment and byte limits.
    pub segments: Box<[Box<[u8]>]>,
    /// Terminal coverage rule after the base.
    pub last: Last,
}

/// Terminal coverage after the exact base; even an empty terminal is one literal segment.
/// (domain/authority.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Last {
    /// One additional segment exactly equal to this, with no descendants.
    Exact(/** Literal additional segment, bounded by `Limits::segment_bytes` at admission. */ Box<[u8]>),
    /// One additional segment beginning with this prefix, and all its descendants.
    Open(
        /** Prefix of the additional segment, bounded by `Limits::segment_bytes`; empty covers any additional segment. */
         Box<[u8]>,
    ),
}

/// Executors local to authority, numbered by configuration or project policy.
/// Configuration or project-policy identifier for a permitted executor, with no executor state
/// retained here. (domain/authority.md, sections 3–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Executor {
    /// An agent running a configured charter.
    Charter(u32),
    /// A configured procedure executor.
    Procedure(u32),
    /// A person executor addressed to a project role.
    Role(u32),
}

/// Lifetime descendant capacity and executor permissions; direct creation also consumes a task slot
/// and a depth level. (domain/authority.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Delegation {
    /// Permitted executors, bounded by `Limits::executors` at admission; duplicates do not widen
    /// permission.
    pub kinds: Box<[Executor]>,
    /// Lifetime task creation ceiling; current remaining capacity is supplied separately.
    pub tasks: u32,
    /// Maximum descendant depth; zero permits no delegation.
    pub depth: u32,
}

/// Spend authority and latest permitted wall time; current spend and reservations are separate
/// accounting inputs. (domain/authority.md, sections 3–5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Budget {
    /// Maximum allotment in the deployment's unit; authority does not choose that unit.
    pub spend: u64,
    /// Latest ending wall time; `None` is later than every finite deadline.
    pub deadline: Option<Wall>,
}
