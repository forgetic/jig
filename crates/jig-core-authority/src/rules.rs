//! Configuration held in force (domain/authority.md, sections 6, 8 and 10).

use alloc::boxed::Box;

use skein_lib::Duration;

use crate::{Authority, Implies, Pattern};

/// Root-owned deployment configuration admitted by `Domain::new`; hard ceilings apply to every
/// later check. (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Rules {
    /// Deployment authority ceiling, admitted under `Limits`.
    pub ceiling: Authority,
    /// Deployment spend ceiling for a period; this value does not keep a period ledger.
    pub period_spend: u64,
    /// Exclusive lower bound of an offered run budget, strictly below the maximum.
    pub minimum_run_spend: u64,
    /// Inclusive run cap, no greater than the deployment authority's spend.
    pub maximum_run_spend: u64,
    /// Validated connector-scoped kind preorder, bounded by `Limits::implications`.
    pub implies: Implies,
    /// Deployment effect requirements, bounded by `Limits::requirements`.
    pub requirements: Box<[Requirement]>,
}

/// Root-submitted project policy; policy events validate its bounded values before replacing the
/// current entry. (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Policy {
    /// Optional final escalation recipient role, distinct from acceptance permissions. When present
    /// it must name a configured role allowed to accept escalations; the current root requires it.
    /// Disabled authority-only policies route no escalation fallback.
    pub escalation_role: Option<u32>,
    /// Project authority ceiling, no greater than the deployment ceiling.
    pub ceiling: Authority,
    /// Grants for goal projection writes, independent of task grants and funding.
    pub projections: Box<[crate::Grant]>,
    /// Project period ceiling, no greater than deployment period spend.
    pub period_spend: u64,
    /// Distinct numbered roles, bounded by `Limits::roles` and the project ceiling.
    pub roles: Box<[Role]>,
    /// Project requirements added to deployment requirements, bounded by `Limits::requirements`.
    pub requirements: Box<[Requirement]>,
}

/// Numbered project permission and funding ceiling; role membership is supplied by people through
/// the root. (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Role {
    pub number: u32,
    /// Authority this role may give, admitted below the project ceiling.
    pub authority: Authority,
    /// Role's period funding ceiling, no greater than the project's.
    pub period_spend: u64,
    pub requests: Requests,
    /// Proposal kinds this role may accept.
    pub decides: Proposals,
}

/// A judge selected by policy. Its parameters name configuration held by that connector.
/// (domain/authority.md, section 10).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Judge {
    pub connector: u16,
    pub requirement: u16,
    pub parameters: u32,
}

/// Whether a verdict holds at application or was observed before the decision.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Guard {
    /// The effect's system checks this state as it applies the effect.
    Guarded,
    /// The verdict must have been observed within this interval.
    Observed { freshness: Duration },
}

/// One requirement on an exact effect kind and a resource pattern.
/// The requirement's meaning belongs to its judging connector (domain/authority.md, section 10).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Requirement {
    pub connector: u16,
    pub kind: u16,
    /// Resources to which it applies, bounded by segment and byte limits.
    pub pattern: Pattern,
    pub judge: Judge,
    pub guard: Guard,
    pub must_be_guarded: bool,
}

/// Kinds of person request checked against a role's request bit set; execution remains with the
/// root. (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RequestKind {
    /// Create a task batch.
    Create,
    /// Give a complete future allotment.
    Allot,
    /// Accept one proposed action.
    Accept,
    /// Give authority newly needed by an amendment.
    Amend,
    /// Cancel a task.
    Cancel,
    /// Release a held task.
    Release,
    /// Move a task with the authority to fund it.
    Move,
    /// Watch live state.
    Watch,
    /// Change project policy.
    Policy,
}

/// Role's request-permission bit set; policy admission rejects bits outside `ALL`.
/// (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Requests(
    /** Bits 1 through 256 for Create, Allot, Accept, Amend, Cancel, Release, Move, Watch and Policy respectively. */
    pub u16,
);

impl Requests {
    /// All defined bits of this permission set; not a grant to a particular role.
    pub const ALL: Requests = Requests(511);

    /// Pure membership query for `kind`; returns one boolean and emits no child output or mutation.
    #[must_use]
    pub fn allows(self, kind: RequestKind) -> bool {
        let bit = match kind {
            RequestKind::Create => 1,
            RequestKind::Allot => 2,
            RequestKind::Accept => 4,
            RequestKind::Amend => 8,
            RequestKind::Cancel => 16,
            RequestKind::Release => 32,
            RequestKind::Move => 64,
            RequestKind::Watch => 128,
            RequestKind::Policy => 256,
        };
        self.0 & bit != 0
    }
}

/// Kinds of proposal a role may accept; permission does not replace the fresh action check.
/// (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProposalKind {
    /// A batch of delegated tasks.
    Batch,
    /// One pinned connector effect.
    Effect,
    /// Widen a task's authority.
    Widen,
    /// An amendment requiring more authority.
    Amend,
    /// Release an escalated task, with additional authority if required.
    Escalation,
}

/// Role's proposal-decision bit set; policy admission rejects bits outside `ALL`.
/// (domain/authority.md, sections 6 and 8.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Proposals(/** Bits 1, 2, 4, 8 and 16 for Batch, Effect, Widen, Amend and Escalation respectively. */ pub u8);

impl Proposals {
    /// All defined bits of this permission set; not a grant to a particular role.
    pub const ALL: Proposals = Proposals(31);

    /// Pure membership query for `kind`; returns one boolean and emits no child output or mutation.
    #[must_use]
    pub fn allows(self, kind: ProposalKind) -> bool {
        let bit = match kind {
            ProposalKind::Batch => 1,
            ProposalKind::Effect => 2,
            ProposalKind::Widen => 4,
            ProposalKind::Amend => 8,
            ProposalKind::Escalation => 16,
        };
        self.0 & bit != 0
    }
}
