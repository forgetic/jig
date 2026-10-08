//! Core-owned funding admission for person-origin goals
//! (domain/engine.md, section 4.4; domain/authority.md, section 9).

use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::Queue;
use skein_lib::{ReplyTo, Token};

use crate::{Core, translate};

/// Authorized goal creation route and its authentic funding source.
#[derive(Debug)]
pub struct GoalStart {
    /// Person pool charged by a direct start or later accepted proposal.
    pub source: tasks::Funder,
    /// Authority bounded to this goal's requested budget.
    pub authority: authority::Authority,
    /// True when current policy and funding admit the task directly.
    pub direct: bool,
}

impl Core {
    /// Whether a configured recurring template is novel and affordable.
    #[must_use]
    pub fn recurring_admit(&self, project: u32, given: &tasks::Authority, template: &tasks::RecurringTemplate) -> bool {
        for member in &template.batch {
            match member.executor {
                tasks::Executor::Person(_) => return false,
                tasks::Executor::Agent { .. } | tasks::Executor::Procedure { .. } => {}
            }
        }
        for task in self.tasks.recurring_tasks(project) {
            if self.tasks.recurring_template(task).expect("identified recurring task").key == template.key {
                return false;
            }
        }
        let Some(policy) = self.authority.policy(project) else { return false };
        authority::at_most(&translate::authority_value(given), &policy.ceiling, &self.authority.rules().implies)
            && self.settings.period_budget <= policy.period_spend
    }

    /// Ask the task child to open a missing period or person pool before a
    /// goal starts; both requests join the same decision as the goal.
    #[must_use]
    pub fn goal_open_pool(&self, project: u32, person: u64) -> (Option<tasks::Event>, Option<tasks::Event>) {
        let period = tasks::Funder::Period { project, period: self.settings.period };
        let open_period = if self.tasks.funding(period).is_none() {
            Some(tasks::Event::OpenPeriod {
                reply_to: ReplyTo::new(Token::new(0)),
                project,
                period: self.settings.period,
                budget: self.settings.period_budget,
            })
        } else {
            None
        };
        let (pool, _) = self.goal_pool(project, person);
        let carve_pool = if self.tasks.funding(pool).is_none() {
            Some(tasks::Event::CarvePool {
                reply_to: ReplyTo::new(Token::new(0)),
                project,
                person,
                period: self.settings.period,
                budget: self.settings.person_budget,
            })
        } else {
            None
        };
        (open_period, carve_pool)
    }

    /// Check the current holder's standing on one person-origin goal.
    pub fn goal_standing(
        &self,
        proposal: &tasks::PersonProposal,
        project: u32,
        role: Option<people::Role>,
    ) -> Result<(), people::Refusal> {
        if proposal.project != project || role.is_none() {
            return Err(people::Refusal::Standing);
        }
        let named = role.expect("checked role");
        let Some(policy) = self.authority.role(project, named.number()) else {
            return Err(people::Refusal::Standing);
        };
        if !policy.decides.allows(authority::ProposalKind::Batch) {
            return Err(people::Refusal::Standing);
        }
        Ok(())
    }

    /// Recheck authentic pool funding before a goal proposal is accepted.
    pub fn goal_accept_allowed(
        &self,
        proposal: &tasks::PersonProposal,
        project: u32,
        person: u64,
        role: people::Role,
        tasks_left: u32,
    ) -> Result<tasks::Funder, people::Refusal> {
        let (source, numbers) = self.goal_pool(project, person);
        let charter = match proposal.goal.executor {
            tasks::Executor::Agent { charter } => charter,
            tasks::Executor::Procedure { .. } | tasks::Executor::Person(_) => unreachable!("goal is agent"),
        };
        let mut findings = Queue::with_capacity(authority::max_out(self.authority.limits()).expect("authority room"));
        let checked = authority::check_request(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: role.number(),
                pool: translate::authority_numbers(numbers),
                tasks_left,
                request: authority::PersonRequest::Accept(authority::Action::Batch(Box::new([authority::Delegate {
                    executor: authority::Executor::Charter(charter),
                    authority: translate::authority_value(&proposal.goal.authority),
                    symbolic: Box::new([]),
                }]))),
            },
            &mut findings,
        );
        if checked.answer == authority::Answer::Allow { Ok(source) } else { Err(people::Refusal::Authority) }
    }

    /// Read the person's current pool, including its configured unopened size.
    #[must_use]
    pub fn goal_pool(&self, project: u32, person: u64) -> (tasks::Funder, tasks::Numbers) {
        let source = tasks::Funder::Pool { project, person, period: self.settings.period };
        let numbers = match self.tasks.funding(source) {
            Some(row) => row.numbers,
            None => tasks::Numbers { budget: self.settings.person_budget, spent: 0, spent_below: 0, reserved: 0 },
        };
        (source, numbers)
    }

    /// Decide whether a person goal starts directly or is offered as a
    /// proposal, before either route allocates its durable number.
    pub fn goal_start(
        &self,
        project: u32,
        person: u64,
        role: people::Role,
        charter: u32,
        budget: u64,
        tasks_left: u32,
    ) -> Result<GoalStart, people::Refusal> {
        let Some(policy) = self.authority.policy(project) else { return Err(people::Refusal::Unknown) };
        let Some(role_policy) = self.authority.role(project, role.number()) else { return Err(people::Refusal::Role) };
        if !role_policy.requests.allows(authority::RequestKind::Create) {
            return Err(people::Refusal::Authority);
        }
        let mut given = self.settings.chat_authority.clone();
        given.budget.spend = budget;
        if charter != self.settings.charter
            || budget > policy.period_spend
            || budget > self.authority.rules().period_spend
            || !authority::at_most(&given, &policy.ceiling, &self.authority.rules().implies)
        {
            return Err(people::Refusal::Authority);
        }
        let (source, numbers) = self.goal_pool(project, person);
        let mut findings = Queue::with_capacity(authority::max_out(self.authority.limits()).expect("authority room"));
        let checked = authority::check_request(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: role.number(),
                pool: translate::authority_numbers(numbers),
                tasks_left,
                request: authority::PersonRequest::Create(Box::new([authority::Delegate {
                    executor: authority::Executor::Charter(charter),
                    authority: given.clone(),
                    symbolic: Box::new([]),
                }])),
            },
            &mut findings,
        );
        let beyond = budget > role_policy.authority.budget.spend
            || budget
                > numbers
                    .budget
                    .saturating_sub(numbers.spent.saturating_add(numbers.spent_below).saturating_add(numbers.reserved));
        if checked.answer != authority::Answer::Allow && !beyond {
            return Err(people::Refusal::Authority);
        }
        Ok(GoalStart { source, authority: given, direct: checked.answer == authority::Answer::Allow })
    }
}
