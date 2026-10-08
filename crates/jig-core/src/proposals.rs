//! Core-owned proposal action checks and nearest-holder routing
//! (domain/engine.md, 4.4; domain/tasks.md, 9).

use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::{List, Queue};

use crate::{Core, Limits, ProposalDecisionRecord, translate};

fn kind(action: &tasks::ProposalAction) -> (tasks::ProposalKind, authority::ProposalKind) {
    match action {
        tasks::ProposalAction::Batch(_) => (tasks::ProposalKind::Batch, authority::ProposalKind::Batch),
        tasks::ProposalAction::Effect { .. } => (tasks::ProposalKind::Effect, authority::ProposalKind::Effect),
        tasks::ProposalAction::Amend { .. } => (tasks::ProposalKind::Amend, authority::ProposalKind::Amend),
        tasks::ProposalAction::Widen { .. } => (tasks::ProposalKind::Widen, authority::ProposalKind::Widen),
        tasks::ProposalAction::Release { .. } => (tasks::ProposalKind::Release, authority::ProposalKind::Escalation),
    }
}

fn action_for_check(action: &tasks::ProposalAction) -> Option<authority::Action> {
    match action {
        tasks::ProposalAction::Batch(batch) => {
            let mut members = List::with_capacity(u32::try_from(batch.len()).expect("bounded proposed batch"));
            for member in batch {
                let executor = match member.executor {
                    tasks::Executor::Agent { charter } => authority::Executor::Charter(charter),
                    tasks::Executor::Procedure { code, .. } => authority::Executor::Procedure(code),
                    tasks::Executor::Person(_) => return None,
                };
                members
                    .push(authority::Delegate {
                        executor,
                        authority: translate::authority_value(&member.authority),
                        symbolic: Box::new([]),
                    })
                    .expect("bounded proposed batch");
            }
            Some(authority::Action::Batch(members.into_boxed()))
        }
        // Tasks keeps a generic authority carrier; the connector supplies the
        // fresh pinned effect again when the holder accepts it.
        tasks::ProposalAction::Effect { authority, .. } | tasks::ProposalAction::Widen { authority, .. } => {
            Some(authority::Action::Widen(translate::authority_value(authority)))
        }
        tasks::ProposalAction::Amend { amendment, .. } => Some(authority::Action::Amend(translate::authority_value(
            amendment.authority.as_ref().expect("proposal amendment widens"),
        ))),
        tasks::ProposalAction::Release { .. } => Some(authority::Action::Escalate { release: None }),
    }
}

fn covers_task(
    core: &Core,
    _limits: &Limits,
    needed: &authority::Authority,
    ancestor: u64,
    distance: u32,
    project: u32,
) -> bool {
    let Some(holder) = core.tasks.delegation(ancestor) else { return false };
    holder.project == project
        && holder.deciding
        && authority::covers(
            &core.authority,
            needed,
            &authority::Holder::Task {
                project,
                authority: translate::authority_value(&holder.authority),
                numbers: translate::authority_numbers(holder.numbers),
                tasks_left: holder.tasks_left,
            },
            distance,
        )
}

fn distance(core: &Core, limits: &Limits, proposer: u64, holder: u64) -> Option<u32> {
    let mut next = core.tasks.delegation(proposer)?.requester;
    for depth in 1..=limits.tasks.depth.saturating_add(1) {
        match next {
            tasks::Party::Task(task) if task == holder => return Some(depth),
            tasks::Party::Task(task) => next = core.tasks.delegation(task)?.requester,
            tasks::Party::Person(_) | tasks::Party::Deployment { .. } => return None,
        }
    }
    None
}

fn covers_person(
    core: &Core,
    limits: &Limits,
    needed: &authority::Authority,
    person: u64,
    project: u32,
    kind: authority::ProposalKind,
) -> bool {
    let Some(role) = core.people.role(person, project) else { return false };
    let funder = tasks::Funder::Pool { project, person, period: core.settings.period };
    let numbers = match core.tasks.funding(funder) {
        Some(pool) => pool.numbers,
        None => tasks::Numbers { budget: core.settings.person_budget, spent: 0, spent_below: 0, reserved: 0 },
    };
    authority::covers(
        &core.authority,
        needed,
        &authority::Holder::Person {
            project,
            role: role.number(),
            proposal: kind,
            pool: translate::authority_numbers(numbers),
            tasks_left: limits.tasks.tree_tasks,
        },
        0,
    )
}

/// Select the nearest live covering ancestor, then the root requester, then
/// the policy's eligible people. `after` skips holders through one pass/stall.
fn holder(
    core: &Core,
    limits: &Limits,
    proposer: u64,
    action: &tasks::ProposalAction,
    after: Option<tasks::ProposalHolder>,
) -> Option<tasks::ProposalHolder> {
    let context = core.tasks.delegation(proposer)?;
    let checked = action_for_check(action)?;
    let needed = authority::needs(&checked)?;
    let (kind, policy_kind) = kind(action);
    let mut next = context.requester;
    let mut distance = 1_u32;
    let mut passed = after.is_none();
    for _ in 0..limits.tasks.depth.saturating_add(1) {
        match next {
            tasks::Party::Task(task) => {
                let holder = tasks::ProposalHolder::Task(task);
                if passed && covers_task(core, limits, &needed, task, distance, context.project) {
                    return Some(holder);
                }
                if after == Some(holder) {
                    passed = true;
                }
                next = match core.tasks.delegation(task) {
                    Some(context) => context.requester,
                    None => return Some(tasks::ProposalHolder::Policy { project: context.project, kind }),
                };
                distance = distance.checked_add(1)?;
            }
            tasks::Party::Person(person) => {
                let holder = tasks::ProposalHolder::Person(person);
                if passed && covers_person(core, limits, &needed, person, context.project, policy_kind) {
                    return Some(holder);
                }
                if after == Some(holder) {
                    // The next recipient is the final policy group.
                }
                return Some(tasks::ProposalHolder::Policy { project: context.project, kind });
            }
            tasks::Party::Deployment { .. } => {
                return Some(tasks::ProposalHolder::Policy { project: context.project, kind });
            }
        }
    }
    None
}

impl Core {
    /// Check a task's proposed action against both project and deployment ceilings.
    pub fn proposal_admit_action(
        &self,
        project: u32,
        action: &tasks::ProposalAction,
    ) -> Result<authority::Authority, tasks::Refusal> {
        let Some(checked) = action_for_check(action) else { return Err(tasks::Refusal::Executor) };
        let Some(needed) = authority::needs(&checked) else { return Err(tasks::Refusal::AuthorityShape) };
        let Some(policy) = self.authority.policy(project) else { return Err(tasks::Refusal::Project) };
        let implies = &self.authority.rules().implies;
        if !authority::at_most(&needed, &policy.ceiling, implies)
            || !authority::at_most(&needed, &self.authority.rules().ceiling, implies)
        {
            return Err(tasks::Refusal::AuthorityShape);
        }
        Ok(needed)
    }

    /// Check that a task holder can fund the action it accepted.
    pub fn proposal_accept_allowed(
        &self,
        limits: &Limits,
        proposer: u64,
        holder: u64,
        project: u32,
        action: &tasks::ProposalAction,
    ) -> Result<(), tasks::Refusal> {
        let Some(checked) = action_for_check(action) else { return Err(tasks::Refusal::Executor) };
        let Some(needed) = authority::needs(&checked) else { return Err(tasks::Refusal::AuthorityShape) };
        let Some(depth) = distance(self, limits, proposer, holder) else { return Err(tasks::Refusal::Reference) };
        if covers_task(self, limits, &needed, holder, depth, project) { Ok(()) } else { Err(tasks::Refusal::Funding) }
    }
    /// Revisit each pending holder after a project policy or role change.
    #[must_use]
    pub fn proposal_recheck_project(&self, limits: &Limits, project: u32) -> Queue<tasks::Event> {
        let mut out = Queue::with_capacity(limits.tasks.tasks);
        for view in self.tasks.view_tasks() {
            if view.project != project {
                continue;
            }
            let Some(record) = self.tasks.task(view.number) else { continue };
            let Some(proposal) = &record.proposal else { continue };
            let current = match proposal.state {
                tasks::ProposalState::Pending { holder, .. } => holder,
                tasks::ProposalState::Accepted { .. }
                | tasks::ProposalState::Rejected { .. }
                | tasks::ProposalState::Withdrawn => continue,
            };
            let Some(next) = self.proposal_holder(limits, view.number, &proposal.action, None) else { continue };
            if current != next {
                out.push(tasks::Event::StalledProposal {
                    proposer: view.number,
                    proposal: proposal.number,
                    from: current,
                    revision: record.revision,
                    holder: next,
                });
            }
        }
        out
    }

    /// Whether a person's current role may decide this proposal kind.
    #[must_use]
    pub fn proposal_policy_standing(&self, person: u64, project: u32, kind: authority::ProposalKind) -> bool {
        let Some(role) = self.people.role(person, project) else { return false };
        let Some(policy) = self.authority.role(project, role.number()) else { return false };
        policy.decides.allows(kind)
    }

    /// The task and authority kinds of a proposal action.
    #[must_use]
    pub fn proposal_kind(action: &tasks::ProposalAction) -> (tasks::ProposalKind, authority::ProposalKind) {
        kind(action)
    }

    /// Translate a proposal action for authority admission.
    #[must_use]
    pub fn proposal_action_for_check(action: &tasks::ProposalAction) -> Option<authority::Action> {
        action_for_check(action)
    }

    /// Whether one ancestor can decide this proposal.
    #[must_use]
    pub fn proposal_covers_task(
        &self,
        limits: &Limits,
        needed: &authority::Authority,
        ancestor: u64,
        distance: u32,
        project: u32,
    ) -> bool {
        covers_task(self, limits, needed, ancestor, distance, project)
    }

    /// Distance from a proposer to a named ancestor, if live.
    #[must_use]
    pub fn proposal_distance(&self, limits: &Limits, proposer: u64, holder: u64) -> Option<u32> {
        distance(self, limits, proposer, holder)
    }

    /// Whether one party can decide this proposal.
    #[must_use]
    pub fn proposal_covers_person(
        &self,
        limits: &Limits,
        needed: &authority::Authority,
        person: u64,
        project: u32,
        kind: authority::ProposalKind,
    ) -> bool {
        covers_person(self, limits, needed, person, project, kind)
    }

    /// Choose the next live proposal holder after any passed holder.
    #[must_use]
    pub fn proposal_holder(
        &self,
        limits: &Limits,
        proposer: u64,
        action: &tasks::ProposalAction,
        after: Option<tasks::ProposalHolder>,
    ) -> Option<tasks::ProposalHolder> {
        holder(self, limits, proposer, action, after)
    }
}

/// Extract one immutable person-facing final decision from the child's durable row.
pub(crate) fn decision_record(row: &tasks::Stored) -> Option<ProposalDecisionRecord> {
    match row {
        tasks::Stored::PersonProposal(row) => {
            let (by, choice) = match &row.state {
                tasks::PersonProposalState::Accepted { by: tasks::Party::Person(by) } => {
                    (*by, people::ProposalChoice::Accepted)
                }
                tasks::PersonProposalState::Rejected { by: tasks::Party::Person(by), .. } => {
                    (*by, people::ProposalChoice::Rejected)
                }
                tasks::PersonProposalState::Pending { .. }
                | tasks::PersonProposalState::Accepted {
                    by: tasks::Party::Task(_) | tasks::Party::Deployment { .. },
                }
                | tasks::PersonProposalState::Rejected {
                    by: tasks::Party::Task(_) | tasks::Party::Deployment { .. },
                    ..
                } => return None,
            };
            Some(ProposalDecisionRecord {
                project: row.project,
                proposer: tasks::Party::Person(row.proposer),
                proposal: row.number,
                kind: tasks::ProposalKind::Batch,
                by,
                choice,
                created: None,
            })
        }
        tasks::Stored::History(history) => {
            let proposal = history.proposal.as_ref()?;
            let (by, choice) = match &proposal.state {
                tasks::ProposalState::Accepted { by: tasks::Party::Person(by) } => {
                    (*by, people::ProposalChoice::Accepted)
                }
                tasks::ProposalState::Rejected { by: tasks::Party::Person(by), .. } => {
                    (*by, people::ProposalChoice::Rejected)
                }
                tasks::ProposalState::Pending { .. }
                | tasks::ProposalState::Withdrawn
                | tasks::ProposalState::Accepted { by: tasks::Party::Task(_) | tasks::Party::Deployment { .. } }
                | tasks::ProposalState::Rejected {
                    by: tasks::Party::Task(_) | tasks::Party::Deployment { .. }, ..
                } => return None,
            };
            Some(ProposalDecisionRecord {
                project: proposal.project,
                proposer: tasks::Party::Task(proposal.proposer),
                proposal: proposal.number,
                kind: kind(&proposal.action).0,
                by,
                choice,
                created: None,
            })
        }
        tasks::Stored::Live(_)
        | tasks::Stored::Ended(_)
        | tasks::Stored::Ledger(_)
        | tasks::Stored::Writer(_)
        | tasks::Stored::Pool(_)
        | tasks::Stored::Stub(_)
        | tasks::Stored::Milestone(_) => None,
    }
}
