//! Pure action checks and proposal needs (domain/authority.md, sections 8–9).

use alloc::boxed::Box;

use skein_lib::{List, Queue, Wall};

use crate::limits::{authority_within, name_within, pattern_within, within};
use crate::order::{differences, fit_lacks};
use crate::{
    Action, Answer, Authority, BatchAsk, Budget, Call, CallAsk, Checked, Delegate, Delegation, Domain, Effect,
    EffectAccess, EffectAsk, Finding, Given, Grant, Guard, Holder, Judge, Last, Limits, NoteScope, PersonAsk,
    PersonRequest, Policy, ProposalKind, RequestKind, Requirement, Role, RunAsk, Scopes, Source, Tools, Verdict,
    Writer, at_most, carve, charge, grant_at_most, grant_covers, left, max_out, pattern_at_most, pattern_covers,
};

fn room(domain: &Domain, why: &Queue<Finding>) {
    let count = max_out(domain.limits()).expect("domain validates its output bound");
    assert!(why.room() >= count, "caller reserves every finding before a check");
}

fn find(answer: &mut Answer, why: &mut Queue<Finding>, strict: Answer, finding: Finding) {
    *answer = (*answer).max(strict);
    why.push(finding);
}

fn refuse(why: &mut Queue<Finding>, finding: Finding) -> Answer {
    why.push(finding);
    Answer::Refuse
}

fn checked(answer: Answer, numbers: Option<crate::Numbers>) -> Checked {
    Checked { answer, numbers: if answer == Answer::Allow { numbers } else { None } }
}

fn ceilings(domain: &Domain, policy: &Policy, authority: &Authority, answer: &mut Answer, why: &mut Queue<Finding>) {
    for source in [Source::Project, Source::Deployment] {
        let ceiling = match source {
            Source::Project => &policy.ceiling,
            Source::Deployment => &domain.rules().ceiling,
            Source::Task | Source::Role => unreachable!("only ceiling sources are iterated"),
        };
        let lacks = differences(
            authority,
            ceiling,
            ceiling.delegation.tasks,
            ceiling.delegation.depth,
            ceiling.budget.spend,
            &domain.rules().implies,
        );
        if !lacks.is_empty() {
            find(answer, why, Answer::Refuse, Finding::Authority { source, lacks });
        }
    }
}

fn delegates_within(tasks: &[Delegate], limits: &Limits) -> bool {
    if !within(tasks.len(), limits.batch) {
        return false;
    }
    for task in tasks {
        if !authority_within(&task.authority, limits) {
            return false;
        }
        match task.authority.grants.len().checked_add(task.symbolic.len()) {
            Some(count) if count <= usize::try_from(limits.grants).expect("u32 fits usize") => {}
            Some(_) | None => return false,
        }
        for grant in &task.symbolic {
            if !pattern_within(&grant.pattern, limits) {
                return false;
            }
            match &grant.pattern.last {
                Last::Open(_) => {}
                Last::Exact(_) => return false,
            }
        }
    }
    true
}

fn effect_within(effect: &Effect, limits: &Limits) -> bool {
    if !name_within(&effect.name, limits)
        || !within(effect.additional.len(), limits.writes)
        || !within(effect.guards.len(), limits.facts)
    {
        return false;
    }
    for resource in &effect.additional {
        if !name_within(&resource.name, limits) {
            return false;
        }
    }
    true
}

fn matches_resource(effect: &Effect, pattern: &crate::Pattern) -> bool {
    if pattern_covers(pattern, &effect.name) {
        return true;
    }
    for resource in &effect.additional {
        if pattern_covers(pattern, &resource.name) {
            return true;
        }
    }
    false
}

/// Batch reservations are all-or-nothing. Every direct child consumes one lifetime task slot in
/// addition to the capacity allotted below that child. Root's pure bounded creation check,
/// returning the strictest answer and proposed replacement funding numbers only on allowance.
/// Reserve `max_out(domain.limits())` free finding slots; no ledger mutation, allocation or child
/// request. Commit reservation with task admission once.
#[must_use]
pub fn check_batch(domain: &Domain, ask: &BatchAsk, why: &mut Queue<Finding>) -> Checked {
    room(domain, why);
    if !authority_within(&ask.creator, domain.limits()) || !delegates_within(&ask.tasks, domain.limits()) {
        return checked(refuse(why, Finding::Oversized), None);
    }
    let Some(policy) = domain.policy(ask.project) else {
        return checked(refuse(why, Finding::UnknownProject), None);
    };
    batch(domain, policy, &ask.creator, ask.numbers, ask.tasks_left, &ask.tasks, Source::Task, why)
}

#[expect(
    clippy::too_many_arguments,
    reason = "a batch check borrows its values without allocating an intermediate ask"
)]
fn batch(
    domain: &Domain,
    policy: &Policy,
    creator: &Authority,
    numbers: crate::Numbers,
    tasks_left: u32,
    tasks: &[Delegate],
    source: Source,
    why: &mut Queue<Finding>,
) -> Checked {
    let task_answer = if source == Source::Task { Answer::Propose } else { Answer::Refuse };
    let mut answer = Answer::Allow;
    if numbers.spent.checked_add(numbers.spent_below).is_none() {
        find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
    }
    let mut spend = Some(0_u64);
    let mut count = Some(0_u32);
    for task in tasks {
        let lacks = fit_lacks(&task.authority, creator, &numbers, tasks_left, &domain.rules().implies);
        if !lacks.is_empty() {
            find(&mut answer, why, task_answer, Finding::Authority { source, lacks });
        }
        for source in [source, Source::Project, Source::Deployment] {
            let holder = match source {
                Source::Task | Source::Role => creator,
                Source::Project => &policy.ceiling,
                Source::Deployment => &domain.rules().ceiling,
            };
            let mut covered = true;
            for symbolic in &task.symbolic {
                let mut one = false;
                for grant in &holder.grants {
                    if grant_at_most(symbolic, grant, &domain.rules().implies) {
                        one = true;
                    }
                }
                if !one {
                    covered = false;
                }
            }
            if !covered {
                let strict = if source == Source::Task { task_answer } else { Answer::Refuse };
                find(&mut answer, why, strict, Finding::Grant { source });
            }
        }
        ceilings(domain, policy, &task.authority, &mut answer, why);
        for origin in [source, Source::Project, Source::Deployment] {
            let kinds = match origin {
                Source::Task | Source::Role => &creator.delegation.kinds,
                Source::Project => &policy.ceiling.delegation.kinds,
                Source::Deployment => &domain.rules().ceiling.delegation.kinds,
            };
            if !kinds.contains(&task.executor) {
                let strict = if origin == source { task_answer } else { Answer::Refuse };
                find(&mut answer, why, strict, Finding::Executor { source: origin });
            }
        }
        spend = match spend {
            Some(sum) => sum.checked_add(task.authority.budget.spend),
            None => None,
        };
        count = match count {
            Some(sum) => match task.authority.delegation.tasks.checked_add(1) {
                Some(capacity) => sum.checked_add(capacity),
                None => None,
            },
            None => None,
        };
    }
    let Some(spend) = spend else {
        find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
        return checked(answer, None);
    };
    let Some(count) = count else {
        find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
        return checked(answer, None);
    };
    for origin in [source, Source::Project, Source::Deployment] {
        let ceiling = match origin {
            Source::Task | Source::Role => creator,
            Source::Project => &policy.ceiling,
            Source::Deployment => &domain.rules().ceiling,
        };
        let capacity =
            if origin == source { tasks_left.min(ceiling.delegation.tasks) } else { ceiling.delegation.tasks };
        let budget = if origin == source { left(numbers).min(ceiling.budget.spend) } else { ceiling.budget.spend };
        let strict = if origin == source { task_answer } else { Answer::Refuse };
        if count > capacity {
            find(&mut answer, why, strict, Finding::Tasks { source: origin });
        }
        if spend > budget {
            find(&mut answer, why, strict, Finding::Spend { source: origin });
        }
    }
    let reserved = if answer == Answer::Allow {
        match carve(numbers, &[spend]) {
            Some(numbers) => Some(numbers),
            None => {
                find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
                None
            }
        }
    } else {
        None
    };
    checked(answer, reserved)
}

fn has_grant(authority: &Authority, effect: &Effect, domain: &Domain) -> bool {
    if !has_grant_on(authority, effect, &effect.name, domain) {
        return false;
    }
    for resource in &effect.additional {
        if !has_grant_on(authority, effect, &resource.name, domain) {
            return false;
        }
    }
    true
}

fn has_grant_on(authority: &Authority, effect: &Effect, name: &crate::Name, domain: &Domain) -> bool {
    for grant in &authority.grants {
        if grant_covers(grant, effect.connector, effect.kind, name, &domain.rules().implies) {
            return true;
        }
    }
    false
}

fn grants(
    domain: &Domain,
    policy: &Policy,
    authority: &Authority,
    effect: &Effect,
    answer: &mut Answer,
    why: &mut Queue<Finding>,
) {
    for source in [Source::Task, Source::Project, Source::Deployment] {
        let holder = match source {
            Source::Task => authority,
            Source::Project => &policy.ceiling,
            Source::Deployment => &domain.rules().ceiling,
            Source::Role => unreachable!("only task and ceiling sources are iterated"),
        };
        if !has_grant(holder, effect, domain) {
            let strict = if source == Source::Task { Answer::Propose } else { Answer::Refuse };
            find(answer, why, strict, Finding::Grant { source });
        }
    }
}

fn writable(effect: &Effect, answer: &mut Answer, why: &mut Queue<Finding>) {
    let mut unavailable = effect.access == EffectAccess::Unavailable;
    for resource in &effect.additional {
        unavailable |= resource.access == EffectAccess::Unavailable;
    }
    if unavailable {
        find(answer, why, Answer::Refuse, Finding::ResourceUnavailable);
    }
    let mut denied = match effect.access {
        EffectAccess::Owned | EffectAccess::Participant => false,
        EffectAccess::Context | EffectAccess::Unavailable => true,
    };
    for resource in &effect.additional {
        denied |= match resource.access {
            EffectAccess::Owned | EffectAccess::Participant => false,
            EffectAccess::Context | EffectAccess::Unavailable => true,
        };
    }
    if denied {
        find(answer, why, Answer::Refuse, Finding::ResourceAccess);
    }
}

#[expect(clippy::too_many_arguments, reason = "the price check borrows one action and its current funding snapshot")]
fn price(
    domain: &Domain,
    policy: &Policy,
    authority: &Authority,
    numbers: crate::Numbers,
    funder: Source,
    effect: &Effect,
    answer: &mut Answer,
    why: &mut Queue<Finding>,
) {
    let Some(maximum) = effect.price else { return };
    if numbers.spent.checked_add(numbers.spent_below).is_none() {
        find(answer, why, Answer::Refuse, Finding::Arithmetic);
    }
    for (source, available) in [
        (funder, left(numbers).min(authority.budget.spend)),
        (Source::Project, policy.ceiling.budget.spend),
        (Source::Deployment, domain.rules().ceiling.budget.spend),
    ] {
        if maximum > available {
            let strict = if source == Source::Task { Answer::Propose } else { Answer::Refuse };
            find(answer, why, strict, Finding::Price { source });
        }
    }
}

fn requirements(
    requirements: &[Requirement],
    effect: &Effect,
    given: &[Given],
    now: Wall,
    answer: &mut Answer,
    why: &mut Queue<Finding>,
) {
    for requirement in requirements {
        if requirement.connector != effect.connector
            || requirement.kind != effect.kind
            || !matches_resource(effect, &requirement.pattern)
        {
            continue;
        }
        let guarded = effect.guards.contains(&requirement.judge);
        if (requirement.must_be_guarded || requirement.guard == Guard::Guarded) && !guarded {
            find(answer, why, Answer::Refuse, Finding::Unguarded { judge: requirement.judge });
        }
        if requirement.must_be_guarded && requirement.guard != Guard::Guarded {
            find(answer, why, Answer::Refuse, Finding::Unguarded { judge: requirement.judge });
        }
        let mut met = false;
        let mut waiting = false;
        let mut refused = false;
        for result in given {
            if result.judge != requirement.judge || result.state != effect.state {
                continue;
            }
            match result.verdict {
                Verdict::Met => {
                    let fresh = match requirement.guard {
                        Guard::Guarded => true,
                        Guard::Observed { freshness } => match now.as_nanos().checked_sub(result.at.as_nanos()) {
                            Some(age) => age <= freshness.as_nanos(),
                            None => false,
                        },
                    };
                    if fresh { met = true } else { waiting = true }
                }
                Verdict::Wait => waiting = true,
                Verdict::Refuse => refused = true,
            }
        }
        if refused {
            find(answer, why, Answer::Refuse, Finding::Failed { judge: requirement.judge });
        } else if waiting || !met {
            find(answer, why, Answer::Wait, Finding::Required { judge: requirement.judge });
        }
    }
}

/// Judges an effect needs under the deployment and live project policy.
/// The caller asks each judge through its connector for the effect's exact state.
#[must_use]
pub fn needed_judges(domain: &Domain, project: u32, effect: &Effect) -> Option<List<Judge>> {
    let policy = domain.policy(project)?;
    if !effect_within(effect, domain.limits()) {
        return None;
    }
    let capacity = domain.limits().requirements.checked_mul(2)?;
    let mut judges = List::with_capacity(capacity);
    for requirements in [&domain.rules().requirements, &policy.requirements] {
        for requirement in requirements {
            if requirement.connector == effect.connector
                && requirement.kind == effect.kind
                && matches_resource(effect, &requirement.pattern)
                && !judges.as_slice().contains(&requirement.judge)
            {
                judges.push(requirement.judge).ok()?;
            }
        }
    }
    Some(judges)
}

/// Root's pure effect check over one coherent snapshot of connector verdicts.
/// Returns the strictest answer and findings, refusing oversized inputs; reserve
/// `max_out(domain.limits())` free slots. No connector call or retained state; carry the checked
/// pin into execution.
#[must_use]
pub fn check_effect(domain: &Domain, ask: &EffectAsk, given: &[Given], why: &mut Queue<Finding>) -> Answer {
    room(domain, why);
    if !authority_within(&ask.authority, domain.limits())
        || !effect_within(&ask.effect, domain.limits())
        || !within(given.len(), domain.limits().facts)
        || !within(ask.effect.guards.len(), domain.limits().facts)
    {
        return refuse(why, Finding::Oversized);
    }
    let Some(policy) = domain.policy(ask.project) else {
        return refuse(why, Finding::UnknownProject);
    };
    let mut answer = Answer::Allow;
    grants(domain, policy, &ask.authority, &ask.effect, &mut answer, why);
    writable(&ask.effect, &mut answer, why);
    price(domain, policy, &ask.authority, ask.numbers, Source::Task, &ask.effect, &mut answer, why);
    requirements(&domain.rules().requirements, &ask.effect, given, ask.now, &mut answer, why);
    requirements(&policy.requirements, &ask.effect, given, ask.now, &mut answer, why);
    answer
}

/// Check a connector-described projection under live project grants and requirements.
/// No task authority or funding participates; reserve `max_out` finding slots.
#[must_use]
pub fn check_projection(
    domain: &Domain,
    project: u32,
    effect: &Effect,
    now: Wall,
    given: &[Given],
    why: &mut Queue<Finding>,
) -> Answer {
    room(domain, why);
    if !effect_within(effect, domain.limits()) || !within(given.len(), domain.limits().facts) {
        return refuse(why, Finding::Oversized);
    }
    let Some(policy) = domain.policy(project) else { return refuse(why, Finding::UnknownProject) };
    let mut answer = Answer::Allow;
    projection_grant(domain, policy, effect, &effect.name, &mut answer, why);
    for resource in &effect.additional {
        projection_grant(domain, policy, effect, &resource.name, &mut answer, why);
    }
    for (source, holder) in [(Source::Project, &policy.ceiling), (Source::Deployment, &domain.rules().ceiling)] {
        if !has_grant(holder, effect, domain) {
            find(&mut answer, why, Answer::Refuse, Finding::Grant { source });
        }
    }
    writable(effect, &mut answer, why);
    requirements(&domain.rules().requirements, effect, given, now, &mut answer, why);
    requirements(&policy.requirements, effect, given, now, &mut answer, why);
    answer
}

fn projection_grant(
    domain: &Domain,
    policy: &Policy,
    effect: &Effect,
    name: &crate::Name,
    answer: &mut Answer,
    why: &mut Queue<Finding>,
) {
    let mut covered = false;
    for grant in &policy.projections {
        covered |= grant_covers(grant, effect.connector, effect.kind, name, &domain.rules().implies);
    }
    if !covered {
        find(answer, why, Answer::Refuse, Finding::Grant { source: Source::Project });
    }
}

/// Root's pure run-admission check over offered budget, clock/account reports and root-verified
/// writer holds. Returns the strictest answer and findings; reserve `max_out(domain.limits())` free
/// slots. No claim, run start, allocation or ledger mutation.
#[must_use]
pub fn check_run(domain: &Domain, ask: &RunAsk, why: &mut Queue<Finding>) -> Answer {
    room(domain, why);
    if !authority_within(&ask.authority, domain.limits())
        || !within(ask.accounts.len(), domain.limits().accounts)
        || !within(ask.writes.len(), domain.limits().writes)
    {
        return refuse(why, Finding::Oversized);
    }
    for write in &ask.writes {
        if !effect_within(&write.effect, domain.limits()) {
            return refuse(why, Finding::Oversized);
        }
    }
    let Some(policy) = domain.policy(ask.project) else {
        return refuse(why, Finding::UnknownProject);
    };
    let mut answer = Answer::Allow;
    if ask.numbers.spent.checked_add(ask.numbers.spent_below).is_none() {
        find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
    }
    if ask.budget <= domain.rules().minimum_run_spend || ask.budget > left(ask.numbers) {
        find(&mut answer, why, Answer::Wait, Finding::RunBudget);
    }
    if ask.budget > domain.rules().maximum_run_spend || ask.budget > ask.authority.budget.spend {
        find(&mut answer, why, Answer::Refuse, Finding::RunCap);
    }
    if ask.budget > policy.ceiling.budget.spend {
        find(&mut answer, why, Answer::Refuse, Finding::RunCap);
    }
    if let Some(deadline) = ask.authority.budget.deadline
        && ask.wall > deadline
    {
        find(&mut answer, why, Answer::Wait, Finding::Deadline);
    }
    for ceiling in [&policy.ceiling, &domain.rules().ceiling] {
        if let Some(deadline) = ceiling.budget.deadline
            && ask.wall > deadline
        {
            find(&mut answer, why, Answer::Refuse, Finding::Deadline);
        }
    }
    for usable in &ask.accounts {
        if !usable {
            find(&mut answer, why, Answer::Wait, Finding::Account);
        }
    }
    for write in &ask.writes {
        grants(domain, policy, &ask.authority, &write.effect, &mut answer, why);
        writable(&write.effect, &mut answer, why);
        match write.held {
            Writer::Task | Writer::Ancestor => {}
            Writer::Pending | Writer::Other => find(&mut answer, why, Answer::Wait, Finding::Writer),
        }
    }
    answer
}

/// Root's pure tool-call check over one configured family bit, read grant, message standing or note
/// scope. Returns the strictest answer and bounded findings; reserve `max_out(domain.limits())`
/// free slots. Makes no external call or state change.
#[must_use]
pub fn check_call(domain: &Domain, ask: &CallAsk, why: &mut Queue<Finding>) -> Answer {
    room(domain, why);
    if !authority_within(&ask.authority, domain.limits()) || ask.family.0.count_ones() != 1 {
        return refuse(why, Finding::Oversized);
    }
    match &ask.call {
        Call::Read(effect) => {
            if !effect_within(effect, domain.limits()) {
                return refuse(why, Finding::Oversized);
            }
        }
        Call::Note(scope) => match scope {
            NoteScope::Goal | NoteScope::Project | NoteScope::Deployment => {}
            NoteScope::Resources(resource) => {
                if !pattern_within(&resource.pattern, domain.limits()) {
                    return refuse(why, Finding::Oversized);
                }
            }
        },
        Call::Tool | Call::Message { .. } => {}
    }
    let Some(policy) = domain.policy(ask.project) else {
        return refuse(why, Finding::UnknownProject);
    };
    let mut answer = Answer::Allow;
    for source in [Source::Task, Source::Project, Source::Deployment] {
        let holder = match source {
            Source::Task => &ask.authority,
            Source::Project => &policy.ceiling,
            Source::Deployment => &domain.rules().ceiling,
            Source::Role => unreachable!("only task and ceiling sources are iterated"),
        };
        if ask.family.0 & !holder.tools.0 != 0 {
            let strict = if source == Source::Task { Answer::Propose } else { Answer::Refuse };
            find(&mut answer, why, strict, Finding::Tool);
        }
    }
    match &ask.call {
        Call::Tool => {}
        Call::Read(effect) => grants(domain, policy, &ask.authority, effect, &mut answer, why),
        Call::Message { referenced } => {
            if !referenced {
                find(&mut answer, why, Answer::Refuse, Finding::Reference);
            }
        }
        Call::Note(scope) => {
            for source in [Source::Task, Source::Project, Source::Deployment] {
                let holder = match source {
                    Source::Task => &ask.authority,
                    Source::Project => &policy.ceiling,
                    Source::Deployment => &domain.rules().ceiling,
                    Source::Role => unreachable!("only task and ceiling sources are iterated"),
                };
                if !scope_covered(scope, holder) {
                    let strict = if source == Source::Task { Answer::Propose } else { Answer::Refuse };
                    find(&mut answer, why, strict, Finding::Scope { source });
                }
            }
        }
    }
    answer
}

fn scope_covered(scope: &NoteScope, holder: &Authority) -> bool {
    match scope {
        NoteScope::Goal => holder.notes.0 & Scopes::GOAL.0 != 0,
        NoteScope::Project => holder.notes.0 & Scopes::PROJECT.0 != 0,
        NoteScope::Deployment => holder.notes.0 & Scopes::DEPLOYMENT.0 != 0,
        NoteScope::Resources(resource) => {
            for ceiling in &holder.note_resources {
                if resource.connector == ceiling.connector && pattern_at_most(&resource.pattern, &ceiling.pattern) {
                    return true;
                }
            }
            false
        }
    }
}

fn action_within(action: &Action, limits: &Limits) -> bool {
    match action {
        Action::Batch(tasks) => delegates_within(tasks, limits),
        Action::Effect(effect) => effect_within(effect, limits),
        Action::Widen(authority) | Action::Amend(authority) => authority_within(authority, limits),
        Action::Escalate { release } => match release {
            Some(authority) => authority_within(authority, limits),
            None => true,
        },
    }
}

fn proposal_kind(action: &Action) -> ProposalKind {
    match action {
        Action::Batch(_) => ProposalKind::Batch,
        Action::Effect(_) => ProposalKind::Effect,
        Action::Widen(_) => ProposalKind::Widen,
        Action::Amend(_) => ProposalKind::Amend,
        Action::Escalate { .. } => ProposalKind::Escalation,
    }
}

fn give(
    domain: &Domain,
    policy: &Policy,
    role: &Role,
    ask: &PersonAsk,
    authority: &Authority,
    why: &mut Queue<Finding>,
) -> Checked {
    let mut answer = Answer::Allow;
    let lacks = differences(
        authority,
        &role.authority,
        ask.tasks_left.min(role.authority.delegation.tasks),
        role.authority.delegation.depth,
        left(ask.pool).min(role.authority.budget.spend),
        &domain.rules().implies,
    );
    if !lacks.is_empty() {
        find(&mut answer, why, Answer::Refuse, Finding::Authority { source: Source::Role, lacks });
    }
    ceilings(domain, policy, authority, &mut answer, why);
    let numbers = match carve(ask.pool, &[authority.budget.spend]) {
        Some(numbers) => Some(numbers),
        None => {
            find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
            None
        }
    };
    checked(answer, numbers)
}

/// A person's request checks their role and funding. An accepted effect must additionally pass
/// `check_effect` on pinned facts in the same commit. Root's pure person-role/funding check
/// returning a strict answer and replacement numbers only when allowed. Reserve
/// `max_out(domain.limits())` free finding slots; caller verifies person membership, checks
/// accepted effect facts and commits the decision atomically.
#[must_use]
pub fn check_request(domain: &Domain, ask: &PersonAsk, why: &mut Queue<Finding>) -> Checked {
    check_request_with_standing(domain, ask, false, why)
}

/// Check a current requester or escalation recipient whose right to control
/// this task came from the task tree, while still enforcing authority and
/// funding. Root authenticates that standing before calling this function.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "standing changes only the request bit check within the existing person admission"
)]
pub fn check_request_with_standing(
    domain: &Domain,
    ask: &PersonAsk,
    standing: bool,
    why: &mut Queue<Finding>,
) -> Checked {
    room(domain, why);
    let admitted = match &ask.request {
        PersonRequest::Create(tasks) => delegates_within(tasks, domain.limits()),
        PersonRequest::Allot(authority) | PersonRequest::Amend(authority) | PersonRequest::Move(authority) => {
            authority_within(authority, domain.limits())
        }
        PersonRequest::Accept(action) => action_within(action, domain.limits()),
        PersonRequest::Cancel | PersonRequest::Release | PersonRequest::Watch | PersonRequest::Policy => true,
    };
    if !admitted {
        return checked(refuse(why, Finding::Oversized), None);
    }
    let Some(policy) = domain.policy(ask.project) else {
        return checked(refuse(why, Finding::UnknownProject), None);
    };
    let Some(role) = domain.role(ask.project, ask.role) else {
        return checked(refuse(why, Finding::UnknownRole), None);
    };
    let kind = match &ask.request {
        PersonRequest::Create(_) => RequestKind::Create,
        PersonRequest::Allot(_) => RequestKind::Allot,
        PersonRequest::Accept(_) => RequestKind::Accept,
        PersonRequest::Amend(_) => RequestKind::Amend,
        PersonRequest::Move(_) => RequestKind::Move,
        PersonRequest::Cancel => RequestKind::Cancel,
        PersonRequest::Release => RequestKind::Release,
        PersonRequest::Watch => RequestKind::Watch,
        PersonRequest::Policy => RequestKind::Policy,
    };
    let control = match &ask.request {
        PersonRequest::Amend(_) | PersonRequest::Move(_) | PersonRequest::Cancel | PersonRequest::Release => true,
        PersonRequest::Create(_)
        | PersonRequest::Allot(_)
        | PersonRequest::Accept(_)
        | PersonRequest::Watch
        | PersonRequest::Policy => false,
    };
    if standing && !control {
        return checked(refuse(why, Finding::Unpermitted), None);
    }
    if !standing && !role.requests.allows(kind) {
        return checked(refuse(why, Finding::Unpermitted), None);
    }
    if needs_funding(&ask.request) && ask.pool.budget > role.period_spend {
        return checked(refuse(why, Finding::PeriodSpend), None);
    }
    match &ask.request {
        PersonRequest::Create(tasks) => {
            if !delegates_within(tasks, domain.limits()) {
                return checked(refuse(why, Finding::Oversized), None);
            }
            batch(domain, policy, &role.authority, ask.pool, ask.tasks_left, tasks, Source::Role, why)
        }
        PersonRequest::Allot(authority) | PersonRequest::Amend(authority) | PersonRequest::Move(authority) => {
            if !authority_within(authority, domain.limits()) {
                return checked(refuse(why, Finding::Oversized), None);
            }
            give(domain, policy, role, ask, authority, why)
        }
        PersonRequest::Accept(action) => {
            if !action_within(action, domain.limits()) {
                return checked(refuse(why, Finding::Oversized), None);
            }
            if !role.decides.allows(proposal_kind(action)) {
                return checked(refuse(why, Finding::Undecidable), None);
            }
            match action {
                Action::Batch(tasks) => {
                    batch(domain, policy, &role.authority, ask.pool, ask.tasks_left, tasks, Source::Role, why)
                }
                Action::Effect(effect) => {
                    let mut answer = Answer::Allow;
                    if !has_grant(&role.authority, effect, domain) {
                        find(&mut answer, why, Answer::Refuse, Finding::Grant { source: Source::Role });
                    }
                    if !has_grant(&policy.ceiling, effect, domain) {
                        find(&mut answer, why, Answer::Refuse, Finding::Grant { source: Source::Project });
                    }
                    if !has_grant(&domain.rules().ceiling, effect, domain) {
                        find(&mut answer, why, Answer::Refuse, Finding::Grant { source: Source::Deployment });
                    }
                    writable(effect, &mut answer, why);
                    price(domain, policy, &role.authority, ask.pool, Source::Role, effect, &mut answer, why);
                    let numbers = if answer == Answer::Allow {
                        match effect.price {
                            Some(amount) => charge(ask.pool, amount),
                            None => Some(ask.pool),
                        }
                    } else {
                        None
                    };
                    if numbers.is_none() && answer == Answer::Allow {
                        find(&mut answer, why, Answer::Refuse, Finding::Arithmetic);
                    }
                    checked(answer, numbers)
                }
                Action::Widen(authority) | Action::Amend(authority) => give(domain, policy, role, ask, authority, why),
                Action::Escalate { release } => match release {
                    Some(authority) => give(domain, policy, role, ask, authority, why),
                    None => checked(Answer::Allow, Some(ask.pool)),
                },
            }
        }
        PersonRequest::Cancel | PersonRequest::Release | PersonRequest::Watch | PersonRequest::Policy => {
            checked(Answer::Allow, Some(ask.pool))
        }
    }
}

fn batch_has_spend(tasks: &[Delegate]) -> bool {
    for task in tasks {
        if task.authority.budget.spend != 0 {
            return true;
        }
    }
    false
}

fn needs_funding(request: &PersonRequest) -> bool {
    match request {
        PersonRequest::Create(tasks) => batch_has_spend(tasks),
        PersonRequest::Allot(authority) | PersonRequest::Amend(authority) | PersonRequest::Move(authority) => {
            authority.budget.spend != 0
        }
        PersonRequest::Accept(action) => match action {
            Action::Batch(tasks) => batch_has_spend(tasks),
            Action::Widen(authority) | Action::Amend(authority) => authority.budget.spend != 0,
            Action::Effect(effect) => effect.price.unwrap_or(0) != 0,
            Action::Escalate { release } => match release {
                Some(authority) => authority.budget.spend != 0,
                None => false,
            },
        },
        PersonRequest::Cancel | PersonRequest::Release | PersonRequest::Watch | PersonRequest::Policy => false,
    }
}

fn empty() -> Authority {
    Authority {
        tools: Tools(0),
        grants: Box::new([]),
        delegation: Delegation { kinds: Box::new([]), tasks: 0, depth: 0 },
        budget: Budget { spend: 0, deadline: Some(Wall::EPOCH) },
        notes: Scopes(0),
        note_resources: Box::new([]),
    }
}

fn exact_effect_grant(effect: &Effect, name: &crate::Name) -> Option<Grant> {
    Some(Grant {
        connector: effect.connector,
        kind: effect.kind,
        pattern: crate::Pattern {
            segments: name.segments.get(..name.segments.len().checked_sub(1)?)?.into(),
            last: Last::Exact(name.segments.last()?.clone()),
        },
    })
}

/// Construct an owned least value, bounded by the caller-admitted action. Unlike the borrowed
/// checks, constructing this result copies owned bytes. Pure constructor over a caller-admitted
/// action; copies bounded grants/segments/executors into the least authority needed. Returns `None`
/// on aggregate arithmetic overflow; caller counts this owned output separately and no lifecycle
/// event is emitted.
#[must_use]
pub fn needs(action: &Action) -> Option<Authority> {
    match action {
        Action::Batch(tasks) => batch_needs(tasks),
        Action::Effect(effect) => {
            let mut authority = empty();
            authority.budget.spend = effect.price.unwrap_or(0);
            let count = u32::try_from(effect.additional.len()).ok()?.checked_add(1)?;
            let mut grants = List::with_capacity(count);
            grants.push(exact_effect_grant(effect, &effect.name)?).ok()?;
            for resource in &effect.additional {
                grants.push(exact_effect_grant(effect, &resource.name)?).ok()?;
            }
            authority.grants = grants.into_boxed();
            Some(authority)
        }
        Action::Widen(authority) | Action::Amend(authority) => Some(authority.clone()),
        Action::Escalate { release } => match release {
            Some(authority) => Some(authority.clone()),
            None => Some(empty()),
        },
    }
}

#[expect(clippy::manual_map, reason = "the foundation's step subset uses exhaustive matching without closures")]
fn batch_needs(tasks: &[Delegate]) -> Option<Authority> {
    let mut grant_count = 0_u32;
    let mut kind_count = 0_u32;
    let mut scope_count = 0_u32;
    for task in tasks {
        grant_count = grant_count
            .checked_add(u32::try_from(task.authority.grants.len()).ok()?)?
            .checked_add(u32::try_from(task.symbolic.len()).ok()?)?;
        scope_count = scope_count.checked_add(u32::try_from(task.authority.note_resources.len()).ok()?)?;
        kind_count =
            kind_count.checked_add(u32::try_from(task.authority.delegation.kinds.len()).ok()?.checked_add(1)?)?;
    }
    let mut grants = List::with_capacity(grant_count);
    let mut scopes = List::with_capacity(scope_count);
    let mut kinds = List::with_capacity(kind_count);
    let mut authority = empty();
    for task in tasks {
        let child = &task.authority;
        authority.tools.0 |= child.tools.0;
        authority.notes.0 |= child.notes.0;
        authority.delegation.tasks = authority.delegation.tasks.checked_add(child.delegation.tasks.checked_add(1)?)?;
        authority.delegation.depth = authority.delegation.depth.max(child.delegation.depth.checked_add(1)?);
        authority.budget.spend = authority.budget.spend.checked_add(child.budget.spend)?;
        authority.budget.deadline = match authority.budget.deadline {
            Some(so_far) => match child.budget.deadline {
                Some(deadline) => Some(so_far.max(deadline)),
                None => None,
            },
            None => None,
        };
        for grant in &child.grants {
            grants.push(grant.clone()).expect("all child grants were counted");
        }
        for grant in &task.symbolic {
            grants.push(grant.clone()).expect("all symbolic grants were counted");
        }
        for scope in &child.note_resources {
            scopes.push(scope.clone()).expect("all note resource scopes were counted");
        }
        kinds.push(task.executor).expect("every child executor was counted");
        for kind in &child.delegation.kinds {
            kinds.push(*kind).expect("all delegated executors were counted");
        }
    }
    authority.grants = grants.into_boxed();
    authority.note_resources = scopes.into_boxed();
    authority.delegation.kinds = kinds.into_boxed();
    Some(authority)
}

fn needed_within(authority: &Authority, limits: &Limits) -> bool {
    let mut aggregate = *limits;
    let count = limits.batch.max(limits.writes.saturating_add(1));
    let Some(grants) = limits.grants.checked_mul(count) else {
        return false;
    };
    let Some(one) = limits.executors.checked_add(1) else {
        return false;
    };
    let Some(executors) = one.checked_mul(count) else {
        return false;
    };
    aggregate.grants = grants;
    aggregate.executors = executors;
    authority_within(authority, &aggregate)
}

/// The caller supplies an eligible ancestor/role and its verified distance to the proposer. The
/// action's needs already include its creation depth. Pure proposal-holder coverage query; root
/// verifies standing and distance `below` and supplies current funding/task capacity. Returns false
/// on missing policy/role, bounds, arithmetic or coverage failure; emits no findings or child event
/// and allocates nothing.
#[must_use]
pub fn covers(domain: &Domain, needed: &Authority, holder: &Holder, below: u32) -> bool {
    if !needed_within(needed, domain.limits()) {
        return false;
    }
    let (project, authority, numbers, tasks_left) = match holder {
        Holder::Task { project, authority, numbers, tasks_left } => (*project, authority, *numbers, *tasks_left),
        Holder::Person { project, role, proposal, pool, tasks_left } => {
            let Some(role) = domain.role(*project, *role) else {
                return false;
            };
            if !role.requests.allows(RequestKind::Accept)
                || !role.decides.allows(*proposal)
                || (needed.budget.spend != 0 && pool.budget > role.period_spend)
            {
                return false;
            }
            (*project, &role.authority, *pool, *tasks_left)
        }
    };
    if !authority_within(authority, domain.limits()) || numbers.spent.checked_add(numbers.spent_below).is_none() {
        return false;
    }
    let Some(policy) = domain.policy(project) else {
        return false;
    };
    if !at_most(needed, &policy.ceiling, &domain.rules().implies)
        || !at_most(needed, &domain.rules().ceiling, &domain.rules().implies)
    {
        return false;
    }
    let Some(depth) = authority.delegation.depth.checked_sub(below) else {
        return false;
    };
    differences(
        needed,
        authority,
        tasks_left.min(authority.delegation.tasks),
        depth,
        left(numbers).min(authority.budget.spend),
        &domain.rules().implies,
    )
    .is_empty()
}
