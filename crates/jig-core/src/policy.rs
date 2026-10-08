//! Total translations between party policy values and the authority child.

use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_people as people;
use skein_lib::List;

pub(crate) fn pattern_to_authority(value: people::Pattern) -> authority::Pattern {
    authority::Pattern {
        segments: value.segments,
        last: match value.last {
            people::Last::Exact(word) => authority::Last::Exact(word),
            people::Last::Open(word) => authority::Last::Open(word),
        },
    }
}

fn pattern_to_people(value: &authority::Pattern) -> people::Pattern {
    people::Pattern {
        segments: value.segments.clone(),
        last: match &value.last {
            authority::Last::Exact(word) => people::Last::Exact(word.clone()),
            authority::Last::Open(word) => people::Last::Open(word.clone()),
        },
    }
}

fn authority_to_authority(value: people::Authority) -> Option<authority::Authority> {
    let mut grants = List::with_capacity(u32::try_from(value.grants.len()).ok()?);
    for grant in value.grants {
        grants
            .push(authority::Grant {
                connector: grant.connector,
                kind: grant.kind,
                pattern: pattern_to_authority(grant.pattern),
            })
            .ok()?;
    }
    let mut note_resources = List::with_capacity(u32::try_from(value.note_resources.len()).ok()?);
    for scope in value.note_resources {
        note_resources
            .push(authority::ResourceScope { connector: scope.connector, pattern: pattern_to_authority(scope.pattern) })
            .ok()?;
    }
    let mut kinds = List::with_capacity(u32::try_from(value.delegation.kinds.len()).ok()?);
    for kind in value.delegation.kinds {
        kinds
            .push(match kind {
                people::Executor::Charter(number) => authority::Executor::Charter(number),
                people::Executor::Procedure(number) => authority::Executor::Procedure(number),
                people::Executor::Role(number) => authority::Executor::Role(number),
            })
            .ok()?;
    }
    Some(authority::Authority {
        tools: authority::Tools(value.tools),
        grants: grants.into_boxed(),
        delegation: authority::Delegation {
            kinds: kinds.into_boxed(),
            tasks: value.delegation.tasks,
            depth: value.delegation.depth,
        },
        budget: authority::Budget { spend: value.spend, deadline: value.deadline },
        notes: authority::Scopes(value.notes),
        note_resources: note_resources.into_boxed(),
    })
}

fn authority_to_people(value: &authority::Authority) -> Option<people::Authority> {
    let mut grants = List::with_capacity(u32::try_from(value.grants.len()).ok()?);
    for grant in &value.grants {
        grants
            .push(people::Grant {
                connector: grant.connector,
                kind: grant.kind,
                pattern: pattern_to_people(&grant.pattern),
            })
            .ok()?;
    }
    let mut note_resources = List::with_capacity(u32::try_from(value.note_resources.len()).ok()?);
    for scope in &value.note_resources {
        note_resources
            .push(people::ResourceScope { connector: scope.connector, pattern: pattern_to_people(&scope.pattern) })
            .ok()?;
    }
    let mut kinds = List::with_capacity(u32::try_from(value.delegation.kinds.len()).ok()?);
    for kind in &value.delegation.kinds {
        kinds
            .push(match kind {
                authority::Executor::Charter(number) => people::Executor::Charter(*number),
                authority::Executor::Procedure(number) => people::Executor::Procedure(*number),
                authority::Executor::Role(number) => people::Executor::Role(*number),
            })
            .ok()?;
    }
    Some(people::Authority {
        tools: value.tools.0,
        grants: grants.into_boxed(),
        delegation: people::Delegation {
            kinds: kinds.into_boxed(),
            tasks: value.delegation.tasks,
            depth: value.delegation.depth,
        },
        spend: value.budget.spend,
        deadline: value.budget.deadline,
        notes: value.notes.0,
        note_resources: note_resources.into_boxed(),
    })
}

fn requirement_to_authority(value: people::Requirement) -> authority::Requirement {
    authority::Requirement {
        connector: value.connector,
        kind: value.kind,
        pattern: pattern_to_authority(value.pattern),
        judge: authority::Judge {
            connector: value.judge.connector,
            requirement: value.judge.requirement,
            parameters: value.judge.parameters,
        },
        guard: match value.guard {
            people::Guard::Guarded => authority::Guard::Guarded,
            people::Guard::Observed { freshness } => authority::Guard::Observed { freshness },
        },
        must_be_guarded: value.must_be_guarded,
    }
}

fn requirement_to_people(value: &authority::Requirement) -> people::Requirement {
    people::Requirement {
        connector: value.connector,
        kind: value.kind,
        pattern: pattern_to_people(&value.pattern),
        judge: people::Judge {
            connector: value.judge.connector,
            requirement: value.judge.requirement,
            parameters: value.judge.parameters,
        },
        guard: match value.guard {
            authority::Guard::Guarded => people::Guard::Guarded,
            authority::Guard::Observed { freshness } => people::Guard::Observed { freshness },
        },
        must_be_guarded: value.must_be_guarded,
    }
}

fn role_exists(policy: &authority::Policy, number: u32) -> bool {
    for role in &policy.roles {
        if role.number == number {
            return true;
        }
    }
    false
}

fn map_valid(policy: &authority::Policy, mappings: &[people::PermissionRole]) -> bool {
    for (index, mapping) in mappings.iter().enumerate() {
        if mapping.role > 3 && !role_exists(policy, mapping.role) {
            return false;
        }
        for earlier in mappings.get(..index).expect("enumerated mapping is in bounds") {
            if earlier.connector == mapping.connector && earlier.permission == mapping.permission {
                return false;
            }
        }
    }
    true
}

/// Apply one mutable edit, keeping the ceiling and deployment rules.
pub(crate) fn apply(
    policy: &mut authority::Policy,
    permissions: &mut Box<[people::PermissionRole]>,
    change: people::PolicyChange,
    requirements_limit: u32,
    roles_limit: u32,
) -> Option<()> {
    match change {
        people::PolicyChange::ProjectSpend { period_spend } => policy.period_spend = period_spend,
        people::PolicyChange::Role(edit) => {
            if edit.number <= 3 && !role_exists(policy, edit.number) {
                return None;
            }
            let value = authority_to_authority(edit.authority)?;
            let mut found = false;
            for role in &mut policy.roles {
                if role.number == edit.number {
                    role.authority = value.clone();
                    role.period_spend = edit.period_spend;
                    role.requests = authority::Requests(edit.requests);
                    role.decides = authority::Proposals(edit.decides);
                    found = true;
                }
            }
            if !found {
                let mut roles = List::with_capacity(roles_limit);
                for role in &policy.roles {
                    roles.push(role.clone()).ok()?;
                }
                roles
                    .push(authority::Role {
                        number: edit.number,
                        authority: value,
                        period_spend: edit.period_spend,
                        requests: authority::Requests(edit.requests),
                        decides: authority::Proposals(edit.decides),
                    })
                    .ok()?;
                policy.roles = roles.into_boxed();
            }
        }
        people::PolicyChange::Requirements { requirements } => {
            if u32::try_from(requirements.len()).ok()? > requirements_limit {
                return None;
            }
            let mut translated = List::with_capacity(requirements_limit);
            for requirement in requirements {
                translated.push(requirement_to_authority(requirement)).ok()?;
            }
            policy.requirements = translated.into_boxed();
        }
        people::PolicyChange::Permissions { mappings } => *permissions = mappings,
    }
    if !map_valid(policy, permissions) {
        return None;
    }
    Some(())
}

/// Capture the entire mutable value after a validated policy event.
pub(crate) fn snapshot(
    policy: &authority::Policy,
    permissions: &[people::PermissionRole],
) -> Option<people::PolicyValue> {
    let mut roles = List::with_capacity(u32::try_from(policy.roles.len()).ok()?);
    for role in &policy.roles {
        roles
            .push(people::PolicyRole {
                number: role.number,
                authority: authority_to_people(&role.authority)?,
                period_spend: role.period_spend,
                requests: role.requests.0,
                decides: role.decides.0,
            })
            .ok()?;
    }
    let mut requirements = List::with_capacity(u32::try_from(policy.requirements.len()).ok()?);
    for requirement in &policy.requirements {
        requirements.push(requirement_to_people(requirement)).ok()?;
    }
    Some(people::PolicyValue {
        period_spend: policy.period_spend,
        roles: roles.into_boxed(),
        requirements: requirements.into_boxed(),
        permissions: Box::from(permissions),
    })
}

/// Restore a complete committed mutable value over the configured ceiling.
pub(crate) fn restore(
    policy: &mut authority::Policy,
    permissions: &mut Box<[people::PermissionRole]>,
    value: people::PolicyValue,
    limit: u32,
) -> Option<()> {
    policy.period_spend = value.period_spend;
    let mut roles = List::with_capacity(u32::try_from(value.roles.len()).ok()?);
    for role in value.roles {
        roles
            .push(authority::Role {
                number: role.number,
                authority: authority_to_authority(role.authority)?,
                period_spend: role.period_spend,
                requests: authority::Requests(role.requests),
                decides: authority::Proposals(role.decides),
            })
            .ok()?;
    }
    policy.roles = roles.into_boxed();
    if u32::try_from(value.requirements.len()).ok()? > limit {
        return None;
    }
    let mut requirements = List::with_capacity(limit);
    for requirement in value.requirements {
        requirements.push(requirement_to_authority(requirement)).ok()?;
    }
    policy.requirements = requirements.into_boxed();
    *permissions = value.permissions;
    if !map_valid(policy, permissions) {
        return None;
    }
    Some(())
}

use crate::{Core, Limits};
use skein_lib::Queue;

impl Core {
    /// Apply one authenticated mutable policy edit in the core's authority and party vocabularies.
    pub(crate) fn change_policy(
        &mut self,
        limits: &Limits,
        person: u64,
        project: u32,
        change: people::PolicyChange,
    ) -> Result<people::PolicyValue, people::Refusal> {
        self.role_allowed(person, project, limits.tasks.tree_tasks)?;
        let Some(mut policy) = self.authority.policy(project).cloned() else {
            return Err(people::Refusal::Unknown);
        };
        let mut permissions = match self.permission_roles.get(&project) {
            Some(mappings) => mappings.clone(),
            None => Box::new([]),
        };
        if apply(
            &mut policy,
            &mut permissions,
            change,
            self.authority.limits().requirements,
            self.authority.limits().roles,
        )
        .is_none()
        {
            return Err(people::Refusal::Unknown);
        }
        let Some(snapshot) = snapshot(&policy, &permissions) else {
            return Err(people::Refusal::Limit);
        };
        if match people::policy_bytes(&snapshot) {
            Some(bytes) => bytes > limits.policy_bytes,
            None => true,
        } {
            return Err(people::Refusal::Limit);
        }
        let mut facts = Queue::with_capacity(authority::POLICY_MAX_OUT);
        authority::step(&mut self.authority, authority::Event::Policy { project, policy }, &mut facts);
        match facts.pop().expect("policy update terminal") {
            authority::PolicyFact::Changed { .. } => {
                assert!(self.permission_roles.insert(project, permissions).is_ok(), "admitted permission policy");
                Ok(snapshot)
            }
            authority::PolicyFact::Refused { .. } => Err(people::Refusal::Authority),
            authority::PolicyFact::Added { .. } | authority::PolicyFact::Dropped { .. } => {
                unreachable!("existing project policy replacement")
            }
        }
    }

    /// Restore a committed mutable policy row before admitting new party requests.
    pub fn restore_policy(&mut self, limits: &Limits, project: u32, value: people::PolicyValue) -> bool {
        match people::policy_bytes(&value) {
            Some(bytes) if bytes <= limits.policy_bytes => {}
            Some(_) | None => return false,
        }
        let Some(mut policy) = self.authority.policy(project).cloned() else { return false };
        let mut permissions = match self.permission_roles.get(&project) {
            Some(mappings) => mappings.clone(),
            None => Box::new([]),
        };
        if restore(&mut policy, &mut permissions, value, self.authority.limits().requirements).is_none() {
            return false;
        }
        let mut facts = Queue::with_capacity(authority::POLICY_MAX_OUT);
        authority::step(&mut self.authority, authority::Event::Policy { project, policy }, &mut facts);
        facts.pop() == Some(authority::PolicyFact::Changed { project })
            && self.permission_roles.insert(project, permissions).is_ok()
    }
}
