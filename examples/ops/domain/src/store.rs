//! Wrap each owner's rows and addresses without inspecting its values.
//! A new application replaces the two connector arms (domain/root.md, 6).
use jig_core as core;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;

/// A contiguous family of rows belonging to one child.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    /// The core's live rows, in its restoration order.
    Core,
    /// Observability's live rows.
    Observability,
    /// Infrastructure's live rows.
    Infrastructure,
}

/// One child's stable row address.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Key {
    /// A core or core-child address.
    Core(core::Key),
    /// An observability address.
    Observability(observability::RecordKey),
    /// An infrastructure address.
    Infrastructure(infrastructure::RecordKey),
}

/// One child's durable row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Record {
    /// A core or core-child row.
    Core(core::Record),
    /// An observability row.
    Observability(observability::Record),
    /// An infrastructure row.
    Infrastructure(infrastructure::Record),
}
impl Record {
    /// The row's stable address.
    #[must_use]
    pub fn key(&self) -> Key {
        match self {
            Self::Core(row) => Key::Core(core_key(row)),
            Self::Observability(row) => Key::Observability(row.key()),
            Self::Infrastructure(row) => Key::Infrastructure(row.key()),
        }
    }
}
/// One mutation in the event's atomic decision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Write {
    /// Save the owner's row at its key.
    Save(Record),
    /// Erase the owner's address.
    Erase(Key),
}
fn core_key(row: &core::Record) -> core::Key {
    match row {
        core::Record::Core(row) => core::Key::Core(match row {
            core::CoreRecord::Deployment(_) => core::CoreKey::Deployment,
            core::CoreRecord::Projection(row) => core::CoreKey::Projection(row.key()),
            core::CoreRecord::Call(row) => core::CoreKey::Call(row.key),
            core::CoreRecord::RunProof(row) => core::CoreKey::RunProof(row.task),
            core::CoreRecord::Turn(row) => core::CoreKey::Turn { task: row.task, attempt: row.attempt, turn: row.turn },
            core::CoreRecord::Terminal(row) => core::CoreKey::Terminal { task: row.task, attempt: row.attempt },
            core::CoreRecord::ProposalDecision(row) => core::CoreKey::ProposalDecision(row.proposal),
            core::CoreRecord::EscalationDecision(row) => {
                core::CoreKey::EscalationDecision { task: row.task, revision: row.revision }
            }
        }),
        core::Record::Tasks(row) => core::Key::Tasks(row.key()),
        core::Record::People(row) => core::Key::People(row.key()),
        core::Record::Notes(row) => core::Key::Notes(match row {
            jig_core_notes::Record::Entry(entry) => jig_core_notes::Key::Entry { name: entry.name },
            jig_core_notes::Record::Line(line) => {
                jig_core_notes::Key::Line { scope: line.scope.clone(), name: line.name }
            }
        }),
    }
}
