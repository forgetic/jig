//! Deployment numbers owned by the core (domain/engine.md, 3 and 5.4).

/// Core to store, then store to core at startup: one fixed-size deployment
/// header with allocated high-water marks. Every writing decision saves it
/// atomically with its other rows (domain/engine.md, 5.1 and 5.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Deployment {
    /// Shell-supplied identity committed on the deployment's first start and restored thereafter;
    /// exactly sixteen bytes.
    pub id: [u8; 16],
    /// Last task number allocated by the core; zero before its first allocation, never reset or
    /// reused.
    pub tasks: u64,
    /// Last candidate person number allocated for sign-in; refused or existing identities may leave
    /// gaps.
    pub people: u64,
    /// Last sign-in candidate allocated by the core; persisted gaps are allowed and no secret
    /// session bytes are held here.
    pub sign_ins: u64,
    /// Last core-issued result position in commit order.
    pub messages: u64,
    /// Last activation number allocated for a claim; the candidate may leave a gap if preparation
    /// fails.
    pub runs: u64,
    /// Last core-allocated call number.
    pub calls: u64,
    /// Last allocated stable store identity for a connector row.
    pub connector_rows: u64,
    /// Last issued ordered commit. A restored header is already durable; a live journal may await
    /// this number's store terminal.
    pub commits: u64,
}

/// Core-selected counter for `fresh`; each is a separate checked `u64` high-water
/// mark, not a channel token or child-owned handle (domain/engine.md, 5.4).
/// Selecting a family is pure; allocating it dirties the journal header.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Family {
    /// Task identities saved with the task row.
    Task,
    /// Person candidates supplied to people sign-in admission.
    Person,
    /// Secret-free sign-in candidates supplied to people.
    SignIn,
    /// Result positions issued as tasks end.
    Message,
    /// Fresh attempt identities saved at claims.
    Run,
    /// Count of distinct named calls newly decided by the engine.
    Call,
    /// Stable connector row identity, allocated once per live connector key.
    ConnectorRow,
}

/// Deployment numbers retained by the core.
#[derive(Debug)]
pub struct Counters {
    deployment: Deployment,
    dirty: bool,
}

impl Counters {
    /// An empty store needs the deployment identity committed even before its first task. The id is
    /// a root input, drawn once by the shell at startup. Validated limits size the fixed held
    /// queue; the next accepted decision emits the dirty header's commit.
    #[must_use]
    pub fn bootstrap(id: [u8; 16]) -> Counters {
        let deployment = Deployment {
            id,
            tasks: 0,
            people: 0,
            sign_ins: 0,
            messages: 0,
            runs: 0,
            calls: 0,
            connector_rows: 0,
            commits: 0,
        };
        Counters { deployment, dirty: true }
    }

    /// A loaded header names the last fully applied commit. It is durable; outstanding answers from
    /// a previous process need not be reconstructed. The root supplies the decoded store header and
    /// validated startup bounds; construction emits no effect.
    #[must_use]
    pub const fn new(deployment: Deployment) -> Counters {
        Counters { deployment, dirty: false }
    }

    /// Pure snapshot of allocated counters, which may run ahead of the durable store until its
    /// issued commits answer.
    #[must_use]
    pub const fn deployment(&self) -> Deployment {
        self.deployment
    }

    /// Whether the next decision must save the deployment numbers.
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty
    }

    /// Number the decision's one commit and return its header for the store.
    pub fn next_commit(&mut self) -> Deployment {
        self.deployment.commits = self.deployment.commits.checked_add(1).expect("admitted commit number");
        self.dirty = false;
        self.deployment
    }

    /// Pure idle query given the journal's own idle result.
    #[must_use]
    pub const fn quiescent(&self, journal_idle: bool) -> bool {
        !self.dirty && journal_idle
    }
}

/// Root numbers are never reused. Allocation is part of the admitted decision, even if its
/// candidate is unused; the next commit saves the gap. The root chooses the counter family after
/// admission. Returns its next positive `u64`, or `None` when stopped/exhausted, with no effect on
/// refusal; emits no request itself.
pub fn fresh(counters: &mut Counters, family: Family) -> Option<u64> {
    let counter = match family {
        Family::Task => &mut counters.deployment.tasks,
        Family::Person => &mut counters.deployment.people,
        Family::SignIn => &mut counters.deployment.sign_ins,
        Family::Message => &mut counters.deployment.messages,
        Family::Run => &mut counters.deployment.runs,
        Family::Call => &mut counters.deployment.calls,
        Family::ConnectorRow => &mut counters.deployment.connector_rows,
    };
    let next = counter.checked_add(1)?;
    *counter = next;
    counters.dirty = true;
    Some(next)
}
