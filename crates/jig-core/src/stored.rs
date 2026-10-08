//! The core's durable run and call identities (domain/engine.md, 5.4 and 7).

use crate::{Core, Counters, Deployment, Family, Limits, fresh};

/// Owner of a retained named-call answer when the call is asked again.
#[derive(Debug)]
pub enum CallReplay {
    /// The core kept the whole answer in its own vocabulary.
    Core(CallPart),
    /// The numbered connector kept the typed answer under the same key.
    Connector { connector: u16 },
}
use alloc::boxed::Box;
use skein_lib::Wall;

/// The core's retained part of a named call answer. A connector owns its
/// typed answer; the core keeps only its number, while it keeps complete
/// answers for calls routed among its children (domain/engine.md, 7.3).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum CallPart {
    /// An effect kept in the same decision; its settled connector answer may follow.
    Effect { connector: u16, entry: u64, deadline: Wall, outcome: Option<crate::connector::OutboxOutcome> },
    /// The application connector owns the typed answer.
    Connector { connector: u16 },
    /// The core denied a connector effect after checking its authority.
    EffectDenied { answer: jig_core_authority::Answer, findings: Box<[jig_core_authority::Finding]> },
    /// A named engine tool was refused before serving it, with the missing authority.
    ToolDenied { answer: jig_core_authority::Answer, findings: Box<[jig_core_authority::Finding]> },
    /// One held descendant decision reached its semantic terminal.
    EscalationDecided { task: u64, revision: u64, outcome: jig_core_tasks::EscalationOutcome },
    /// A task holder's decision call was refused before mutation.
    EscalationRefused(jig_core_tasks::Problem),
    /// Proposal entered its proposer's pending state.
    Proposed { proposal: u64 },
    /// Proposal was accepted, rejected, passed or withdrawn.
    ProposalDecided { proposal: u64, outcome: jig_core_tasks::ProposalOutcome },
    /// Proposal action or standing failed before mutation.
    ProposalRefused(jig_core_tasks::Problem),
    /// A named task control change entered the decision.
    Controlled,
    /// A named task control change was refused.
    ControlRefused(jig_core_tasks::Problem),
    /// A widening needs a holder or violates a ceiling.
    ControlDenied { answer: jig_core_authority::Answer },
    /// Message entered its target's inbox.
    Sent { message: u64 },
    /// Reciprocal task references were installed.
    Introduced,
    /// A message or introduction failed admission.
    MessageRefused(jig_core_tasks::Problem),
    /// One standing interest was installed.
    Subscribed { subscription: u64 },
    /// One standing interest was removed.
    Unsubscribed,
    /// A standing interest failed admission.
    SubscriptionRefused(jig_core_tasks::Problem),
    /// All members of an authorized batch, in call order.
    Delegated(Box<[u64]>),
    /// A batch declined by authority, with findings.
    DelegationDenied { answer: jig_core_authority::Answer, findings: Box<[jig_core_authority::Finding]> },
    /// A batch refused by structural or funding admission.
    DelegationRefused(jig_core_tasks::Problem),
    /// A scoped note was committed at the named revision.
    NoteWritten { name: u64, revision: u32 },
    /// One bounded recall page was answered from the notes child.
    NoteRecalled { entries: Box<[jig_core_notes::Entry]>, more: bool },
    /// A note request changed nothing.
    NoteRefused(jig_core_notes::Refusal),
    /// The named tool is deferred to a later route.
    Unavailable,
}

impl CallPart {
    /// Reject malformed restored generic answers before they enter the live
    /// call table. Connector payload bounds remain with their owner.
    #[must_use]
    pub fn valid(
        &self,
        deployment: &Deployment,
        limits: &Limits,
        authority_limits: &jig_core_authority::Limits,
    ) -> bool {
        match self {
            CallPart::Effect { entry, .. } => *entry != 0 && *entry <= deployment.connector_rows,
            CallPart::Connector { .. }
            | CallPart::Unavailable
            | CallPart::Introduced
            | CallPart::Unsubscribed
            | CallPart::Controlled
            | CallPart::NoteRefused(_)
            | CallPart::ControlDenied { .. } => true,
            CallPart::Proposed { proposal } | CallPart::ProposalDecided { proposal, .. } => {
                *proposal != 0 && *proposal <= deployment.messages
            }
            CallPart::EscalationDecided { task, revision, .. } => {
                *task != 0 && *task <= deployment.tasks && *revision != 0
            }
            CallPart::Sent { message } => *message != 0 && *message <= deployment.messages,
            CallPart::NoteWritten { name, revision } => *name != 0 && *name <= deployment.messages && *revision != 0,
            CallPart::NoteRecalled { entries, .. } => {
                if entries.len() > usize::try_from(limits.notes.recalled).expect("u32 fits usize") {
                    return false;
                }
                for entry in entries {
                    if !jig_core_notes::valid_entry(entry, &limits.notes) {
                        return false;
                    }
                }
                true
            }
            CallPart::Subscribed { subscription } => *subscription != 0 && *subscription <= deployment.messages,
            CallPart::Delegated(numbers) => {
                if numbers.is_empty() || numbers.len() > usize::try_from(limits.tasks.batch).expect("u32 fits usize") {
                    return false;
                }
                for (at, &number) in numbers.iter().enumerate() {
                    if number == 0 || number > deployment.tasks {
                        return false;
                    }
                    for &earlier in numbers.iter().take(at) {
                        if number == earlier {
                            return false;
                        }
                    }
                }
                true
            }
            CallPart::EffectDenied { answer, findings }
            | CallPart::ToolDenied { answer, findings }
            | CallPart::DelegationDenied { answer, findings } => {
                *answer != jig_core_authority::Answer::Allow
                    && findings.len()
                        <= usize::try_from(
                            jig_core_authority::max_out(authority_limits).expect("valid authority bound"),
                        )
                        .expect("u32 fits usize")
            }
            CallPart::MessageRefused(problem)
            | CallPart::ProposalRefused(problem)
            | CallPart::EscalationRefused(problem)
            | CallPart::SubscriptionRefused(problem)
            | CallPart::DelegationRefused(problem)
            | CallPart::ControlRefused(problem) => match problem.task {
                Some(task) => task != 0 && task <= deployment.tasks,
                None => true,
            },
        }
    }
}

/// A retained named-call decision, keyed by its live run and block name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CallRecord {
    pub key: CallKey,
    pub part: CallPart,
    pub settled: Option<crate::SettledCall>,
}

/// Core-owned durable rows before the application's store wrapper assigns
/// its own child-family variant (domain/engine.md, 5.4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CoreRecord {
    /// Deployment numbers.
    Deployment(Deployment),
    /// One live named call's core answer.
    Call(CallRecord),
    /// Current claim evidence.
    RunProof(RunProof),
    /// One immutable transcript turn.
    Turn(TurnRecord),
    /// One immutable terminal.
    Terminal(TerminalRecord),
    /// One accepted proposal race.
    ProposalDecision(ProposalDecisionRecord),
    /// One accepted held-chat race.
    EscalationDecision(EscalationDecisionRecord),
}

/// Core-owned fixed-size addresses, ordered within the core's own family.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CoreKey {
    /// Singleton deployment header.
    Deployment,
    /// Current claim evidence for one task.
    RunProof(u64),
    /// Live named call answer.
    Call(CallKey),
    /// One immutable turn.
    Turn { task: u64, attempt: u64, turn: u32 },
    /// One immutable terminal.
    Terminal { task: u64, attempt: u64 },
    /// One accepted proposal race.
    ProposalDecision(u64),
    /// One accepted held-chat race.
    EscalationDecision { task: u64, revision: u64 },
}

/// The core's store vocabulary wraps each durable child under one variant.
/// A root wraps this enum once more beside its connector records.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Record {
    /// Rows owned by the core itself.
    Core(CoreRecord),
    /// Task hub and funding rows.
    Tasks(jig_core_tasks::Stored),
    /// Party identities, roles and keyed replies.
    People(jig_core_people::Stored),
    /// Scoped note entries and index lines.
    Notes(jig_core_notes::Record),
}

/// The core's child-first store-key order.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Key {
    /// Core-owned address.
    Core(CoreKey),
    /// Task hub address.
    Tasks(jig_core_tasks::Key),
    /// Party address.
    People(jig_core_people::Key),
    /// Scoped note address.
    Notes(jig_core_notes::Key),
}

/// Result of restoring one core-owned row during the live-range startup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Restored {
    /// The deployment header also supplies the journal's durable number.
    Deployment { commits: u64 },
    /// One live core row was retained.
    Live,
    /// An archive row is outside the startup's live ranges.
    Archive,
    /// The row fails the current live proof or bounded identity checks.
    Rejected,
}

impl Core {
    /// Decide whether this named answer belongs to the current durable
    /// claim, retaining it only while that claim still owns the call.
    #[must_use]
    pub fn decide_named_call(&mut self, key: CallKey, part: CallPart) -> bool {
        let _pending = self.pending_calls.remove(&key);
        let current = match self.proofs.get(&key.task) {
            Some(proof) => proof.attempt == key.attempt,
            None => false,
        };
        if !current {
            return false;
        }
        let _number = fresh(&mut self.counters, Family::Call).expect("admitted call counter");
        self.record_call(key, part);
        true
    }

    /// Find the retained part of a named call without consulting a
    /// connector's separately owned payload.
    #[must_use]
    pub fn replay_call(&self, key: CallKey) -> Option<CallReplay> {
        match self.call_parts.get(&key) {
            Some(CallPart::Connector { connector }) => Some(CallReplay::Connector { connector: *connector }),
            Some(part) => Some(CallReplay::Core(part.clone())),
            None => None,
        }
    }

    /// Validate a live task-family row against the deployment and retain the
    /// correlations that a later proof and view restoration will need.
    #[must_use]
    pub fn restore_task_row(&mut self, row: &jig_core_tasks::Stored) -> bool {
        use jig_core_tasks as tasks;

        let deployment = self.counters.deployment();
        match row {
            tasks::Stored::PersonProposal(proposal) => {
                proposal.number <= deployment.messages
                    && proposal.proposer <= deployment.people
                    && proposal.goal.number <= deployment.tasks
                    && self.authority.policy(proposal.project).is_some()
            }
            tasks::Stored::Stub(stub) => stub.task <= deployment.tasks,
            tasks::Stored::Live(task) => {
                if !supported_task(task, self.settings.charter)
                    || !supported_proposal(task, &deployment)
                    || task.number > deployment.tasks
                    || !supported_requester(task.requester, deployment.people, deployment.tasks)
                {
                    return false;
                }
                let agent_attempt = match task.executor {
                    tasks::Executor::Agent { .. } => task.attempt != 0,
                    tasks::Executor::Procedure { .. } | tasks::Executor::Person(_) => false,
                };
                if agent_attempt && task.attempt > deployment.runs {
                    return false;
                }
                if agent_attempt
                    && self
                        .restoring_proofs
                        .insert(
                            task.number,
                            crate::RestoringProof {
                                attempt: task.attempt,
                                turn: task.turn,
                                run_spent: task.run_spent,
                                last_answer: task.last_answer,
                            },
                        )
                        .is_err()
                {
                    return false;
                }
                self.view_phases.insert(task.number, (tasks::view_phase(&task.phase), task.tracked)).is_ok()
            }
            tasks::Stored::Ledger(_) | tasks::Stored::Writer(_) | tasks::Stored::Pool(_) => true,
            tasks::Stored::Ended(_) | tasks::Stored::History(_) => false,
        }
    }

    /// Retain one of the core's own live rows. The root wraps store bytes and
    /// routes connector rows separately; it cannot alter this live decision.
    pub fn restore_core(&mut self, record: CoreRecord, limits: &Limits) -> Restored {
        match record {
            CoreRecord::Deployment(deployment) => {
                self.counters = Counters::new(deployment);
                Restored::Deployment { commits: deployment.commits }
            }
            CoreRecord::Call(record) => {
                let mut valid_settled = match &record.settled {
                    Some(call) => {
                        call.serial != 0 && call.serial <= self.counters.deployment().calls && call.valid(limits)
                    }
                    None => true,
                };
                if let Some(call) = &record.settled {
                    for (_, previous) in &self.call_settled {
                        if call.serial == previous.serial {
                            valid_settled = false;
                        }
                    }
                }
                if valid_settled
                    && record.part.valid(&self.counters.deployment(), limits, self.authority.limits())
                    && self.restore_call(record.key, record.part)
                {
                    if let Some(call) = record.settled {
                        assert!(
                            self.call_settled.insert(record.key, call).is_ok(),
                            "restored settled names share the call bound"
                        );
                    }
                    Restored::Live
                } else {
                    Restored::Rejected
                }
            }
            CoreRecord::RunProof(proof) => {
                if self.restore_proof(proof, limits.tasks.result_bytes) {
                    Restored::Live
                } else {
                    Restored::Rejected
                }
            }
            CoreRecord::Turn(_)
            | CoreRecord::Terminal(_)
            | CoreRecord::ProposalDecision(_)
            | CoreRecord::EscalationDecision(_) => Restored::Archive,
        }
    }
}

fn supported_task(task: &jig_core_tasks::TaskRecord, charter: u32) -> bool {
    use jig_core_tasks as tasks;
    let executor = match task.executor {
        tasks::Executor::Agent { charter: configured } => charter == configured,
        tasks::Executor::Procedure { code, .. } => code != 0,
        tasks::Executor::Person(tasks::PersonAddress::Person(person)) => person != 0,
        tasks::Executor::Person(tasks::PersonAddress::Role(role)) => role <= 3,
    };
    task.number != 0 && task.result_position == 0 && executor
}

fn supported_proposal(task: &jig_core_tasks::TaskRecord, deployment: &Deployment) -> bool {
    use jig_core_tasks as tasks;
    let Some(proposal) = &task.proposal else { return true };
    if proposal.number == 0 || proposal.number > deployment.messages || proposal.proposer != task.number {
        return false;
    }
    let holder = match proposal.state {
        tasks::ProposalState::Pending { holder, .. } => holder,
        tasks::ProposalState::Accepted { .. }
        | tasks::ProposalState::Rejected { .. }
        | tasks::ProposalState::Withdrawn => return false,
    };
    let holder_valid = match holder {
        tasks::ProposalHolder::Task(number) => number != 0 && number <= deployment.tasks,
        tasks::ProposalHolder::Person(number) => number != 0 && number <= deployment.people,
        tasks::ProposalHolder::Policy { project, .. } => project == task.project,
    };
    if !holder_valid {
        return false;
    }
    match &proposal.action {
        tasks::ProposalAction::Effect { attempt, completion, .. } => {
            *attempt != 0 && *attempt <= deployment.runs && *completion != 0
        }
        tasks::ProposalAction::Batch(batch) => {
            for member in batch {
                if member.number == 0 || member.number > deployment.tasks {
                    return false;
                }
            }
            true
        }
        tasks::ProposalAction::Amend { task, .. }
        | tasks::ProposalAction::Widen { task, .. }
        | tasks::ProposalAction::Release { task } => *task != 0 && *task <= deployment.tasks,
    }
}

fn supported_requester(requester: jig_core_tasks::Party, people: u64, tasks: u64) -> bool {
    match requester {
        jig_core_tasks::Party::Person(person) => person != 0 && person <= people,
        jig_core_tasks::Party::Task(task) => task != 0 && task <= tasks,
        jig_core_tasks::Party::Deployment { .. } => true,
    }
}

/// Stable call identity supplied by a run and scoped by the root's task and
/// claim (domain/engine.md, sections 5.4 and 7.3).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CallKey {
    pub task: u64,
    pub attempt: u64,
    /// One-based assistant completion.
    pub completion: u32,
    /// Zero-based assistant block position.
    pub position: u32,
}

/// Root to store: an accepted numbered transcript and cumulative priced spend,
/// saved atomically with child task/funding and root latest-turn proof before
/// its worker ACK
/// (domain/engine.md, 5.1 and 7.2). Loads return the same owned shape.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TurnRecord {
    /// Positive durable task number owning this transcript.
    pub task: u64,
    /// Positive root-issued activation fence for the transcript.
    pub attempt: u64,
    /// Positive accepted turn number; sequence admission belongs to tasks.
    pub turn: u32,
    /// Cumulative accepted spend for this attempt, not an additional charge; represented by `u64`.
    pub spent: u64,
    /// No message fence is admitted in this current route; tasks refuses `Some` before mutation
    /// until an actual inbox route joins.
    pub read: Option<u64>,
    /// Injected wall time when the root accepted the turn; it survives restart without depending on
    /// the process clock.
    pub at: Wall,
    /// Owned transcript bytes, bounded by journal `transcript_bytes` before writing and by load
    /// page budgets when read.
    pub transcript: Box<[u8]>,
}

/// Root's accepted terminal identity (worker offer or actual root-translated
/// unpriced terminal), saved atomically with task and
/// financial writes before ACK; result bytes obey root admission (domain/engine.md, 7.4).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TerminalRecord {
    /// Root-issued positive durable task identity.
    pub task: u64,
    /// Root-issued positive attempt identity; unchanged on replay.
    pub attempt: u64,
    /// Accepted cumulative expense for a priced worker offer or unchanged expense for an unpriced
    /// root terminal; child counters change once.
    pub cumulative: u64,
    /// Exact bounded worker terminal, even when child lifecycle normalizes it; a refused answer
    /// archives root's unpriced Invalid normalization. A lost claim is `Failed(Lost)` and spends a
    /// try, whether or not a turn was kept.
    pub end: jig_core_tasks::End,
}

/// Root's latest accepted turn metadata; the full transcript stays only in
/// the immutable turn archive. Fleet fences earlier bodies (domain/engine.md, 7.2).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TurnProof {
    /// Positive consecutive accepted turn, supplied by the worker.
    pub turn: u32,
    /// Accepted cumulative priced spend; tasks owns all financial counters.
    pub cumulative: u64,
    /// Admitted message fence, taken with the turn and charge.
    pub read: Option<u64>,
}

/// Root's current claim evidence, store to root on paged startup. At most
/// tasks.tasks rows are kept; each has one latest turn and one terminal.
/// Claim reserves map room before child mutation and replaces this row. Ending
/// erases it atomically; immutable transcript/terminal archives never enter the
/// live map. Startup validates every row or stops, dropping none silently
/// (domain/engine.md, sections 6 and 7.2).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RunProof {
    /// Positive task identity allocated by root; unique in the live proof table.
    pub task: u64,
    /// Positive current claim identity allocated by root.
    pub attempt: u64,
    /// Earliest attempt whose turns belong to this usable conversation; zero keeps all.
    pub transcript_from: u64,
    /// Highest inbox message offered to this attempt in an assignment or committed relay.
    pub offered: Option<u64>,
    /// Latest accepted turn, or none before the first turn; no historical body is retained.
    pub turn: Option<TurnProof>,
    /// Typed accepted worker offer or actual root-translated unpriced terminal; present iff the
    /// child answered this current attempt. At most twice task result bytes before normalization;
    /// cleared on replacement/end.
    pub terminal: Option<TerminalRecord>,
}

/// Root-owned immutable first accepted decision for one held-chat revision.
/// Saved atomically with semantic task change and keyed people outcome; bounded
/// reason is never restored into a live archive map.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EscalationDecisionRecord {
    /// Actual child's project, used to authenticate historical reads rather
    /// than trust the caller's project.
    pub project: u32,
    /// Actual person requester from the accepted bounded semantic context;
    /// current requester/policy-role standing controls replay privacy
    pub requester: u64,
    /// Positive task identity.
    pub task: u64,
    /// Positive checked semantic revision.
    pub revision: u64,
    /// Positive authenticated winning person.
    pub by: u64,
    /// Exact accepted bounded choice; rejection reason fits journal `result_bytes`/`transcript_bytes`
    /// and both child bounds before mutation.
    pub decision: jig_core_people::EscalationDecision,
}

/// Immutable decision evidence for a person-facing proposal race.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ProposalDecisionRecord {
    pub project: u32,
    pub proposer: jig_core_tasks::Party,
    pub proposal: u64,
    pub kind: jig_core_tasks::ProposalKind,
    pub by: u64,
    pub choice: jig_core_people::ProposalChoice,
}
