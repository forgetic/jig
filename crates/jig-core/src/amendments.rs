//! Core-owned standing and policy decisions for a person's task amendment.

use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::Queue;

use crate::{Core, translate};

impl Core {
    /// Admit a task-origin amendment, including ancestry and widening.
    pub fn task_amend_admit(
        &self,
        caller: u64,
        target: u64,
        amendment: &tasks::Amendment,
        depth: u32,
    ) -> Result<bool, TaskAmendDenied> {
        let Some(holder) = self.tasks.delegation(caller) else {
            return Err(TaskAmendDenied::Refused { task: caller, why: tasks::Refusal::Unknown });
        };
        let Some(current) = self.tasks.delegation(target) else {
            return Err(TaskAmendDenied::Refused { task: target, why: tasks::Refusal::Unknown });
        };
        let mut next = target;
        let mut descendant = false;
        for _ in 0..=depth {
            if next == caller {
                descendant = true;
                break;
            }
            let Some(context) = self.tasks.delegation(next) else { break };
            match context.requester {
                tasks::Party::Task(parent) => next = parent,
                tasks::Party::Person(_) | tasks::Party::Deployment { .. } => break,
            }
        }
        if target == caller || current.project != holder.project || !descendant {
            return Err(TaskAmendDenied::Refused { task: target, why: tasks::Refusal::Reference });
        }
        let Some(after) = &amendment.authority else { return Ok(false) };
        let before = translate::authority_value(&current.authority);
        let after = translate::authority_value(after);
        let implies = &self.authority.rules().implies;
        let stop_run = !authority::at_most(&before, &after, implies);
        if !authority::at_most(&after, &before, implies) {
            let hard = match self.authority.policy(current.project) {
                Some(policy) => {
                    authority::at_most(&after, &policy.ceiling, implies)
                        && authority::at_most(&after, &self.authority.rules().ceiling, implies)
                }
                None => false,
            };
            let answer = if hard {
                if authority::at_most(&after, &translate::authority_value(&holder.authority), implies) {
                    authority::Answer::Allow
                } else {
                    authority::Answer::Propose
                }
            } else {
                authority::Answer::Refuse
            };
            if answer != authority::Answer::Allow {
                return Err(TaskAmendDenied::Denied(answer));
            }
        }
        Ok(stop_run)
    }

    /// Return whether to stop a current run and whether widening must be
    /// offered to a policy holder as a proposal.
    #[expect(clippy::too_many_arguments, reason = "one amendment carries its authenticated party, task and limits")]
    pub fn amend_admit(
        &self,
        person: u64,
        role: Option<people::Role>,
        project: u32,
        task: u64,
        amendment: &tasks::Amendment,
        depth: u32,
        limits: &tasks::Limits,
    ) -> Result<(bool, bool), people::Refusal> {
        let Some(context) = self.tasks.delegation(task) else { return Err(people::Refusal::Ended) };
        let Some(role) = role else { return Err(people::Refusal::Role) };
        let any_task = match role {
            people::Role::Owner | people::Role::Maintainer => true,
            people::Role::Member | people::Role::Observer => false,
            people::Role::Policy { .. } => match self.authority.role(project, role.number()) {
                Some(policy) => policy.requests.allows(authority::RequestKind::Amend),
                None => false,
            },
        };
        let standing = self.person_tree(person, task, depth) || self.person_escalation_recipient(person, role, task);
        if context.project != project || !any_task && !standing {
            return Err(people::Refusal::Standing);
        }
        let Some(role_policy) = self.authority.role(project, role.number()) else {
            return Err(people::Refusal::Authority);
        };
        if !role_policy.requests.allows(authority::RequestKind::Amend) && !standing {
            return Err(people::Refusal::Authority);
        }
        let Some(after) = &amendment.authority else { return Ok((false, false)) };
        if !tasks::valid_authority(limits, after) {
            return Err(people::Refusal::Limit);
        }
        let before = translate::authority_value(&context.authority);
        let after = translate::authority_value(after);
        let implies = &self.authority.rules().implies;
        let stop_run = !authority::at_most(&before, &after, implies);
        if authority::at_most(&after, &before, implies) {
            return Ok((stop_run, false));
        }
        let pool = tasks::Funder::Pool { project, person, period: self.settings.period };
        let numbers = match self.tasks.funding(pool) {
            Some(row) => row.numbers,
            None => tasks::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 },
        };
        let mut findings = Queue::with_capacity(authority::max_out(self.authority.limits()).expect("authority bound"));
        let checked = authority::check_request_with_standing(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: role.number(),
                pool: translate::authority_numbers(numbers),
                tasks_left: context.tasks_left,
                request: authority::PersonRequest::Amend(after),
            },
            standing,
            &mut findings,
        );
        let role_shortage = if findings.len() == 1 {
            match findings.pop() {
                Some(authority::Finding::Authority { source: authority::Source::Role, .. }) => true,
                Some(_) | None => false,
            }
        } else {
            false
        };
        let answer = if checked.answer == authority::Answer::Refuse && role_shortage {
            authority::Answer::Propose
        } else {
            checked.answer
        };
        match answer {
            authority::Answer::Allow => Ok((stop_run, false)),
            authority::Answer::Propose => Ok((stop_run, true)),
            authority::Answer::Wait | authority::Answer::Refuse => Err(people::Refusal::Authority),
        }
    }
}

/// One admission outcome translated by the application into its call answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskAmendDenied {
    /// Invalid task, reference or caller.
    Refused { task: u64, why: tasks::Refusal },
    /// Authority did not allow widening directly.
    Denied(authority::Answer),
}
