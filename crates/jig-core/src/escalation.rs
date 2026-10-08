//! Core-owned held-recipient selection and restore validation
//! (domain/engine.md, sections 3 and 4.4).

use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::Queue;

use crate::{Core, Limits, translate};

fn fallback(core: &Core, project: u32) -> Option<tasks::EscalationHolder> {
    // The policy names the escalation role; this slice resolves that role here.
    let role = core.authority.policy(project)?.escalation_role?;
    Some(tasks::EscalationHolder::Role { project, role })
}

fn covers(
    core: &Core,
    limits: &Limits,
    context: &tasks::EscalationContext,
    person: u64,
    role: Option<people::Role>,
) -> bool {
    let Some(role) = role else { return false };
    let pool = tasks::Funder::Pool { project: context.project, person, period: core.settings.period };
    let numbers = match core.tasks.funding(pool) {
        Some(record) => record.numbers,
        None => tasks::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 },
    };
    let Some(needed) = authority::needs(&authority::Action::Escalate { release: None }) else { return false };
    authority::covers(
        &core.authority,
        &needed,
        &authority::Holder::Person {
            project: context.project,
            role: role.number(),
            proposal: authority::ProposalKind::Escalation,
            pool: translate::authority_numbers(numbers),
            tasks_left: limits.tasks.tree_tasks,
        },
        0,
    )
}

fn supported(core: &Core, task: &tasks::TaskRecord) -> bool {
    let Some(fallback) = fallback(core, task.project) else { return false };
    match &task.escalation {
        tasks::Escalation::Waiting { holder: tasks::EscalationHolder::Role { .. }, entry, .. } => {
            // An old selector can be rechecked, but its project must remain the chat's.
            *entry <= core.counters.deployment().messages
                && match fallback {
                    tasks::EscalationHolder::Role { project, .. } => project == task.project,
                    tasks::EscalationHolder::Task(_) | tasks::EscalationHolder::Person(_) => false,
                }
        }
        tasks::Escalation::Rejected { by, .. } => {
            *by <= core.counters.deployment().people.max(core.counters.deployment().tasks)
        }
        tasks::Escalation::Waiting { entry, .. } => *entry <= core.counters.deployment().messages,
        tasks::Escalation::Unheld { .. } | tasks::Escalation::Routing { .. } => true,
    }
}

/// Pure recipient selection for actual startup/live routing and candidate-roster
/// preflight; preserves a final-role holder.
fn recipient(
    core: &Core,
    limits: &Limits,
    context: &tasks::EscalationContext,
    requester_role: Option<people::Role>,
) -> Option<tasks::EscalationHolder> {
    recipient_after(core, limits, context, requester_role, None)
}

fn recipient_after(
    core: &Core,
    limits: &Limits,
    context: &tasks::EscalationContext,
    requester_role: Option<people::Role>,
    after: Option<tasks::EscalationHolder>,
) -> Option<tasks::EscalationHolder> {
    match context.escalation {
        tasks::Escalation::Waiting { holder: tasks::EscalationHolder::Role { .. }, .. } => {
            fallback(core, context.project)
        }
        tasks::Escalation::Unheld { .. }
        | tasks::Escalation::Routing { .. }
        | tasks::Escalation::Waiting { .. }
        | tasks::Escalation::Rejected { .. } => {
            let needed = authority::needs(&authority::Action::Escalate { release: None })?;
            let mut above = context.immediate;
            let mut distance = 1_u32;
            let mut passed = after.is_none();
            for _ in 0..limits.tasks.depth.saturating_add(1) {
                let parent = match above {
                    tasks::Party::Task(parent) => parent,
                    tasks::Party::Person(_) | tasks::Party::Deployment { .. } => break,
                };
                let Some(holder) = core.tasks.delegation(parent) else {
                    return fallback(core, context.project);
                };
                if passed
                    && holder.deciding
                    && holder.project == context.project
                    && authority::covers(
                        &core.authority,
                        &needed,
                        &authority::Holder::Task {
                            project: context.project,
                            authority: translate::authority_value(&holder.authority),
                            numbers: translate::authority_numbers(holder.numbers),
                            tasks_left: holder.tasks_left,
                        },
                        distance,
                    )
                {
                    return Some(tasks::EscalationHolder::Task(parent));
                }
                if after == Some(tasks::EscalationHolder::Task(parent)) {
                    passed = true;
                }
                above = holder.requester;
                distance = distance.checked_add(1)?;
            }
            if passed && context.requester != 0 && covers(core, limits, context, context.requester, requester_role) {
                Some(tasks::EscalationHolder::Person(context.requester))
            } else {
                fallback(core, context.project)
            }
        }
    }
}

impl Core {
    /// Recheck a person's current funding and authority before releasing a
    /// held task's escalation.
    #[must_use]
    pub fn escalation_release_allowed(
        &self,
        context: &tasks::EscalationContext,
        person: u64,
        role: Option<people::Role>,
        tasks_left: u32,
    ) -> bool {
        let Some(role) = role else { return false };
        let pool = tasks::Funder::Pool { project: context.project, person, period: self.settings.period };
        let numbers = match self.tasks.funding(pool) {
            Some(record) => record.numbers,
            None => tasks::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 },
        };
        let mut findings =
            Queue::with_capacity(authority::max_out(self.authority.limits()).expect("checked authority output"));
        authority::check_request(
            &self.authority,
            &authority::PersonAsk {
                project: context.project,
                role: role.number(),
                pool: translate::authority_numbers(numbers),
                tasks_left,
                request: authority::PersonRequest::Accept(authority::Action::Escalate { release: None }),
            },
            &mut findings,
        )
        .answer
            == authority::Answer::Allow
    }

    /// Select the project's configured fallback role for a held decision.
    #[must_use]
    pub fn escalation_fallback(&self, project: u32) -> Option<tasks::EscalationHolder> {
        fallback(self, project)
    }

    /// Check whether a party can hold the escalation at this route.
    #[must_use]
    pub fn escalation_covers(
        &self,
        limits: &Limits,
        context: &tasks::EscalationContext,
        person: u64,
        role: Option<people::Role>,
    ) -> bool {
        covers(self, limits, context, person, role)
    }

    /// Validate one restored task's held-recipient shape.
    #[must_use]
    pub fn escalation_supported(&self, task: &tasks::TaskRecord) -> bool {
        supported(self, task)
    }

    /// Select the current holder of a waiting escalation.
    #[must_use]
    pub fn escalation_recipient(
        &self,
        limits: &Limits,
        context: &tasks::EscalationContext,
        requester_role: Option<people::Role>,
    ) -> Option<tasks::EscalationHolder> {
        recipient(self, limits, context, requester_role)
    }

    /// Select the next holder after one current holder stalls.
    #[must_use]
    pub fn escalation_recipient_after(
        &self,
        limits: &Limits,
        context: &tasks::EscalationContext,
        requester_role: Option<people::Role>,
        after: Option<tasks::EscalationHolder>,
    ) -> Option<tasks::EscalationHolder> {
        recipient_after(self, limits, context, requester_role, after)
    }
}
