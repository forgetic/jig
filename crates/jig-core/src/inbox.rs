//! Volatile party inbox projections from the task child's durable rows.
//! The core routes these sibling facts on save and restore (domain/engine.md, 4.4).

use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::List;

use crate::{Core, Limits};

fn add(
    entries: &mut List<people::Entry>,
    row: &tasks::TaskRecord,
    whom: people::Whom,
    kind: people::EntryKind,
    at: skein_lib::Wall,
) {
    entries
        .push(people::Entry { task: row.number, project: row.project, whom, kind, at })
        .expect("one bounded projection per task state");
}

fn proposal_kind(kind: tasks::ProposalKind) -> authority::ProposalKind {
    match kind {
        tasks::ProposalKind::Batch => authority::ProposalKind::Batch,
        tasks::ProposalKind::Amend => authority::ProposalKind::Amend,
        tasks::ProposalKind::Widen => authority::ProposalKind::Widen,
        tasks::ProposalKind::Release => authority::ProposalKind::Escalation,
    }
}

fn question_answered(row: &tasks::TaskRecord, question: u64) -> bool {
    for word in &row.inbox {
        match word.kind {
            tasks::MessageKind::Answer { question: answered } if answered == question => return true,
            tasks::MessageKind::Answer { .. }
            | tasks::MessageKind::Question
            | tasks::MessageKind::Words
            | tasks::MessageKind::Amendment { .. }
            | tasks::MessageKind::Proposal { .. }
            | tasks::MessageKind::ProposalDecision { .. }
            | tasks::MessageKind::Escalation { .. }
            | tasks::MessageKind::Notice { .. }
            | tasks::MessageKind::Timer { .. }
            | tasks::MessageKind::News { .. }
            | tasks::MessageKind::Result(_) => {}
        }
    }
    false
}

impl Core {
    /// Project the currently waiting person-facing references of one task.
    #[must_use]
    #[expect(clippy::too_many_lines, reason = "one exhaustive task projection covers all person-facing waiting kinds")]
    pub fn waiting_entries(&self, limits: &Limits, row: &tasks::TaskRecord) -> Box<[people::Entry]> {
        let capacity = limits
            .tasks
            .inbox_messages
            .checked_add(self.authority.limits().roles)
            .expect("validated task inbox projection bound")
            .checked_add(3)
            .expect("validated task inbox projection bound");
        let mut entries = List::with_capacity(capacity);
        match &row.phase {
            tasks::Phase::Ended(_) | tasks::Phase::Closing(_) => return entries.into_boxed(),
            tasks::Phase::Waiting | tasks::Phase::Active(_) | tasks::Phase::Held { .. } => {}
        }
        match row.requester {
            tasks::Party::Person(person) => {
                for word in &row.inbox {
                    match word.kind {
                        tasks::MessageKind::Question => {
                            if !question_answered(row, word.number) {
                                add(
                                    &mut entries,
                                    row,
                                    people::Whom::Person(person),
                                    people::EntryKind::Question { message: word.number },
                                    word.at,
                                );
                            }
                        }
                        tasks::MessageKind::Answer { .. } => add(
                            &mut entries,
                            row,
                            people::Whom::Person(person),
                            people::EntryKind::Reply { message: word.number },
                            word.at,
                        ),
                        tasks::MessageKind::ProposalDecision { .. }
                        | tasks::MessageKind::Proposal { .. }
                        | tasks::MessageKind::Words
                        | tasks::MessageKind::Amendment { .. }
                        | tasks::MessageKind::Notice { .. }
                        | tasks::MessageKind::Timer { .. }
                        | tasks::MessageKind::News { .. }
                        | tasks::MessageKind::Result(_)
                        | tasks::MessageKind::Escalation { .. } => {}
                    }
                }
            }
            tasks::Party::Task(_) | tasks::Party::Deployment { .. } => {}
        }
        if let Some(proposal) = &row.proposal {
            match proposal.state {
                tasks::ProposalState::Pending { holder, since } => match holder {
                    tasks::ProposalHolder::Person(person) => add(
                        &mut entries,
                        row,
                        people::Whom::Person(person),
                        people::EntryKind::Proposal { number: proposal.number },
                        since,
                    ),
                    tasks::ProposalHolder::Policy { project, kind } => {
                        if let Some(project_policy) = self.authority.policy(project) {
                            for policy in &project_policy.roles {
                                if !policy.decides.allows(proposal_kind(kind)) {
                                    continue;
                                }
                                add(
                                    &mut entries,
                                    row,
                                    people::Whom::Role { project, role: policy.number },
                                    people::EntryKind::Proposal { number: proposal.number },
                                    since,
                                );
                            }
                        }
                    }
                    tasks::ProposalHolder::Task(_) => {}
                },
                tasks::ProposalState::Accepted { .. }
                | tasks::ProposalState::Rejected { .. }
                | tasks::ProposalState::Withdrawn => {}
            }
        }
        match row.escalation {
            tasks::Escalation::Waiting { holder, revision, since, .. } => match holder {
                tasks::EscalationHolder::Person(person) => {
                    add(
                        &mut entries,
                        row,
                        people::Whom::Person(person),
                        people::EntryKind::Escalation { revision },
                        since,
                    );
                }
                tasks::EscalationHolder::Role { project, role } => {
                    add(
                        &mut entries,
                        row,
                        people::Whom::Role { project, role },
                        people::EntryKind::Escalation { revision },
                        since,
                    );
                }
                tasks::EscalationHolder::Task(_) => {}
            },
            tasks::Escalation::Unheld { .. }
            | tasks::Escalation::Routing { .. }
            | tasks::Escalation::Rejected { .. } => {}
        }
        match row.executor {
            tasks::Executor::Person(tasks::PersonAddress::Person(person)) => {
                if row.taken_by.is_none() {
                    add(&mut entries, row, people::Whom::Person(person), people::EntryKind::PersonTask, row.created_at);
                }
            }
            tasks::Executor::Person(tasks::PersonAddress::Role(role)) => {
                let whom = match row.taken_by {
                    Some(person) => people::Whom::Person(person),
                    None => people::Whom::Role { project: row.project, role },
                };
                add(&mut entries, row, whom, people::EntryKind::PersonTask, row.created_at);
            }
            tasks::Executor::Agent { .. } | tasks::Executor::Procedure { .. } => {}
        }
        entries.into_boxed()
    }
}
