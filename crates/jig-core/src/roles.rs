//! Core-owned role and funding admission for authenticated parties
//! (domain/engine.md, sections 3 and 4.4).

use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::{Queue, ReplyTo, Token};

use crate::{Core, Limits, PersonTaskRoute, translate};

impl Core {
    /// Admit a person's funded move of a live task.
    pub fn move_admit(
        &self,
        person: u64,
        role: Option<people::Role>,
        project: u32,
        task: u64,
        tasks_left: u32,
        depth: u32,
    ) -> Result<(), people::Refusal> {
        let Some(context) = self.tasks.delegation(task) else { return Err(people::Refusal::Ended) };
        if context.project != project || role.is_none() {
            return Err(people::Refusal::Standing);
        }
        let holding = role.expect("checked member");
        let Some(role_policy) = self.authority.role(project, holding.number()) else {
            unreachable!("member belongs to configured policy")
        };
        let standing = self.person_tree(person, task, depth) || self.person_escalation_recipient(person, holding, task);
        if !role_policy.requests.allows(authority::RequestKind::Amend) && !standing {
            return Err(people::Refusal::Authority);
        }
        let Some(policy) = self.authority.policy(project) else { unreachable!("configured policy") };
        if self.settings.period_budget > policy.period_spend || self.settings.person_budget > role_policy.period_spend {
            return Err(people::Refusal::Authority);
        }
        let Some(spent) = context.numbers.spent.checked_add(context.numbers.spent_below) else {
            return Err(people::Refusal::Limit);
        };
        let Some(left) = context.numbers.budget.checked_sub(spent) else {
            return Err(people::Refusal::Limit);
        };
        let (_, pool_numbers) = self.goal_pool(project, person);
        let mut giving = translate::authority_value(&context.authority);
        giving.budget.spend = left;
        let mut findings = Queue::with_capacity(authority::max_out(self.authority.limits()).expect("authority output"));
        let checked = authority::check_request_with_standing(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: holding.number(),
                pool: translate::authority_numbers(pool_numbers),
                tasks_left,
                request: authority::PersonRequest::Move(giving),
            },
            true,
            &mut findings,
        );
        if checked.answer == authority::Answer::Allow { Ok(()) } else { Err(people::Refusal::Authority) }
    }

    /// Admit a directly requested chat under current person and project policy.
    pub fn chat_admit(
        &self,
        project: u32,
        person: u64,
        role: people::Role,
        tasks_left: u32,
    ) -> Result<(), people::Refusal> {
        let (_, pool_numbers) = self.goal_pool(project, person);
        let mut findings =
            Queue::with_capacity(authority::max_out(self.authority.limits()).expect("authority check bound"));
        let checked = authority::check_request(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: role.number(),
                pool: translate::authority_numbers(pool_numbers),
                tasks_left,
                request: authority::PersonRequest::Create(Box::new([authority::Delegate {
                    executor: authority::Executor::Charter(self.settings.charter),
                    authority: self.settings.chat_authority.clone(),
                    symbolic: Box::new([]),
                }])),
            },
            &mut findings,
        );
        if checked.answer != authority::Answer::Allow {
            return Err(people::Refusal::Authority);
        }
        let Some(policy) = self.authority.policy(project) else { unreachable!("checked project policy") };
        let Some(role_policy) = self.authority.role(project, role.number()) else {
            unreachable!("checked role policy")
        };
        if match policy.escalation_role {
            Some(role) => role > 3,
            None => true,
        } || self.settings.period_budget > policy.period_spend
            || self.settings.person_budget > role_policy.period_spend
        {
            return Err(people::Refusal::Authority);
        }
        Ok(())
    }
    /// Check standing, current policy and financial authority for a party's
    /// stop, cancel or release request before routing it to the task child.
    pub fn control_admit(
        &self,
        ask: &people::Ask,
        person: u64,
        project: u32,
        role: Option<people::Role>,
        depth: u32,
        result_bytes: u32,
    ) -> Result<u64, people::Refusal> {
        let (task, kind, reason_len) = match ask {
            people::Ask::Stop { task, .. } => (*task, authority::RequestKind::Cancel, 0),
            people::Ask::Cancel { task, reason, .. } => (*task, authority::RequestKind::Cancel, reason.len()),
            people::Ask::Release { task, .. } => (*task, authority::RequestKind::Release, 0),
            people::Ask::TakePerson { .. }
            | people::Ask::HandBackPerson { .. }
            | people::Ask::AnswerPerson { .. }
            | people::Ask::Move { .. }
            | people::Ask::DecideProposal { .. }
            | people::Ask::Say { .. }
            | people::Ask::AnswerQuestion { .. }
            | people::Ask::Prioritise { .. }
            | people::Ask::Amend { .. }
            | people::Ask::Watch { .. }
            | people::Ask::EditNote { .. }
            | people::Ask::MakeService { .. }
            | people::Ask::SetRoles { .. }
            | people::Ask::Adopt { .. }
            | people::Ask::ChangePolicy { .. }
            | people::Ask::SetPool { .. }
            | people::Ask::DecideEscalation { .. }
            | people::Ask::StartChat { .. }
            | people::Ask::SetGoal { .. } => unreachable!("control route owns its ask"),
        };
        let Some(context) = self.tasks.delegation(task) else { return Err(people::Refusal::Ended) };
        let Some(holding) = role else { return Err(people::Refusal::Role) };
        let any_task = match holding {
            people::Role::Owner | people::Role::Maintainer => true,
            people::Role::Member | people::Role::Observer => false,
            people::Role::Policy { .. } => match self.authority.role(project, holding.number()) {
                Some(policy) => policy.requests.allows(kind),
                None => false,
            },
        };
        let standing = self.person_tree(person, task, depth) || self.person_escalation_recipient(person, holding, task);
        if context.project != project || !any_task && !standing {
            return Err(people::Refusal::Standing);
        }
        let pool = tasks::Funder::Pool { project, person, period: self.settings.period };
        let numbers = match self.tasks.funding(pool) {
            Some(record) => record.numbers,
            None => tasks::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 },
        };
        let action = if kind == authority::RequestKind::Cancel {
            authority::PersonRequest::Cancel
        } else {
            authority::PersonRequest::Release
        };
        let mut findings = Queue::with_capacity(authority::max_out(self.authority.limits()).expect("bounded findings"));
        let checked = authority::check_request_with_standing(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: holding.number(),
                pool: translate::authority_numbers(numbers),
                tasks_left: context.tasks_left,
                request: action,
            },
            standing,
            &mut findings,
        );
        if checked.answer != authority::Answer::Allow {
            return Err(people::Refusal::Authority);
        }
        if reason_len > usize::try_from(result_bytes).expect("u32 fits usize") {
            Err(people::Refusal::Limit)
        } else {
            Ok(task)
        }
    }

    /// Whether a person is the requesting ancestor of a live task.
    #[must_use]
    pub fn person_tree(&self, person: u64, task: u64, depth: u32) -> bool {
        let mut next = task;
        for _ in 0..=depth {
            let Some(context) = self.tasks.delegation(next) else { return false };
            match context.requester {
                tasks::Party::Person(requester) => return requester == person,
                tasks::Party::Task(parent) => next = parent,
                tasks::Party::Deployment { .. } => return false,
            }
        }
        false
    }

    /// Whether a person or their current role holds a task's escalation.
    #[must_use]
    pub fn person_escalation_recipient(&self, person: u64, role: people::Role, task: u64) -> bool {
        let Some(context) = self.tasks.escalation(task) else { return false };
        match context.escalation {
            tasks::Escalation::Waiting { holder: tasks::EscalationHolder::Person(holder), .. } => holder == person,
            tasks::Escalation::Waiting { holder: tasks::EscalationHolder::Role { project, role: holder }, .. } => {
                project == context.project && holder == role.number()
            }
            tasks::Escalation::Waiting { holder: tasks::EscalationHolder::Task(_), .. }
            | tasks::Escalation::Unheld { .. }
            | tasks::Escalation::Routing { .. }
            | tasks::Escalation::Rejected { .. } => false,
        }
    }

    /// Check an authenticated party's authority and bounded goal list for
    /// one priority change.
    fn priorities_allowed(
        &self,
        project: u32,
        role: Option<people::Role>,
        goals: usize,
        task_capacity: u32,
    ) -> Result<(), people::Refusal> {
        let allowed = match role {
            Some(people::Role::Owner | people::Role::Maintainer | people::Role::Policy { .. }) => {
                match self.authority.role(project, role.expect("checked role").number()) {
                    Some(policy) => policy.requests.allows(authority::RequestKind::Amend),
                    None => false,
                }
            }
            Some(people::Role::Member | people::Role::Observer) | None => false,
        };
        if !allowed {
            return Err(people::Refusal::Authority);
        }
        if goals > usize::try_from(task_capacity).expect("u32 fits usize") {
            return Err(people::Refusal::Limit);
        }
        Ok(())
    }

    /// Route one admitted priority change to the task child, retaining the
    /// party's keyed continuation until the task terminal arrives.
    pub fn prioritise(
        &mut self,
        request: Token,
        person: u64,
        project: u32,
        role: Option<people::Role>,
        goals: Box<[(u64, u32)]>,
        task_capacity: u32,
    ) -> Result<tasks::Event, people::Refusal> {
        self.priorities_allowed(project, role, goals.len(), task_capacity)?;
        assert!(
            self.person_tasks.insert(request, PersonTaskRoute::Prioritised(project)) == Ok(None),
            "one priority route"
        );
        Ok(tasks::Event::Prioritise {
            reply_to: ReplyTo::new(request),
            project,
            by: tasks::Party::Person(person),
            goals,
        })
    }

    /// Check a party role's current authority to open a live watch.
    #[must_use]
    pub fn watch_authorized(&self, project: u32, role: Option<people::Role>) -> bool {
        let Some(role) = role else { return false };
        match self.authority.role(project, role.number()) {
            Some(policy) => policy.requests.allows(authority::RequestKind::Watch),
            None => false,
        }
    }

    /// Check a party role's current note scope, including connector paths.
    #[must_use]
    pub fn note_authorized(&self, project: u32, role: Option<people::Role>, scope: &people::NoteScope) -> bool {
        let Some(role) = role else { return false };
        let Some(policy) = self.authority.role(project, role.number()) else { return false };
        match scope {
            people::NoteScope::Deployment => policy.authority.notes.0 & 4 != 0,
            people::NoteScope::Project => policy.authority.notes.0 & 2 != 0,
            people::NoteScope::Goal { .. } => policy.authority.notes.0 & 1 != 0,
            people::NoteScope::Resources { connector, pattern } => {
                let needed = translate::pattern_to_authority(pattern.clone());
                for offered in &policy.authority.note_resources {
                    if offered.connector == *connector && authority::pattern_at_most(&needed, &offered.pattern) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Preflight every waiting escalation against a candidate role roster
    /// before applying any membership change.
    pub fn roles_preflight(
        &self,
        limits: &Limits,
        contexts: &[tasks::EscalationContext],
        holdings: &[people::Holding],
    ) -> Result<u32, people::Refusal> {
        let mut changed = 0_u32;
        for context in contexts {
            let mut role = None;
            for holding in holdings {
                if holding.person == context.requester {
                    role = Some(holding.role);
                }
            }
            let Some(holder) = self.escalation_recipient(limits, context, role) else {
                return Err(people::Refusal::Authority);
            };
            match context.escalation {
                tasks::Escalation::Waiting { revision, holder: old, .. } => {
                    if old != holder {
                        if revision.checked_add(1).is_none() {
                            return Err(people::Refusal::Limit);
                        }
                        changed = changed.checked_add(1).expect("bounded task count");
                    }
                }
                tasks::Escalation::Unheld { .. }
                | tasks::Escalation::Routing { .. }
                | tasks::Escalation::Rejected { .. } => unreachable!("inspection contains waiting contexts only"),
            }
        }
        Ok(changed)
    }

    /// Admit an owner asking to change current role policy.
    pub fn role_allowed(&self, person: u64, project: u32, tasks_left: u32) -> Result<(), people::Refusal> {
        if self.people.role(person, project) != Some(people::Role::Owner) {
            return Err(people::Refusal::Role);
        }
        if self.authority.policy(project).is_none() {
            return Err(people::Refusal::Unknown);
        }
        let pool = tasks::Funder::Pool { project, person, period: self.settings.period };
        let numbers = match self.tasks.funding(pool) {
            Some(record) => record.numbers,
            None => tasks::Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 },
        };
        let mut findings =
            Queue::with_capacity(authority::max_out(self.authority.limits()).expect("validated authority bound"));
        let checked = authority::check_request(
            &self.authority,
            &authority::PersonAsk {
                project,
                role: people::Role::Owner.number(),
                pool: authority::Numbers {
                    budget: numbers.budget,
                    spent: numbers.spent,
                    spent_below: numbers.spent_below,
                    reserved: numbers.reserved,
                },
                tasks_left,
                request: authority::PersonRequest::Policy,
            },
            &mut findings,
        );
        if checked.answer == authority::Answer::Allow { Ok(()) } else { Err(people::Refusal::Authority) }
    }
}
