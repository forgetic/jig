//! Translations between the core's child vocabularies (domain/engine.md, 3).

use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use skein_lib::List;

/// Move a party's bounded pattern into authority's equivalent vocabulary.
#[must_use]
pub(crate) fn pattern_to_authority(value: people::Pattern) -> authority::Pattern {
    authority::Pattern {
        segments: value.segments,
        last: match value.last {
            people::Last::Exact(word) => authority::Last::Exact(word),
            people::Last::Open(word) => authority::Last::Open(word),
        },
    }
}

#[must_use]
pub(crate) fn authority_numbers(numbers: tasks::Numbers) -> authority::Numbers {
    authority::Numbers {
        budget: numbers.budget,
        spent: numbers.spent,
        spent_below: numbers.spent_below,
        reserved: numbers.reserved,
    }
}

#[must_use]
pub(crate) fn authority_value(value: &tasks::Authority) -> authority::Authority {
    let mut grants = List::with_capacity(u32::try_from(value.grants.len()).expect("validated task grants"));
    for grant in &value.grants {
        let last = match &grant.pattern.last {
            tasks::Last::Exact(bytes) => authority::Last::Exact(bytes.clone()),
            tasks::Last::Open(bytes) => authority::Last::Open(bytes.clone()),
        };
        grants
            .push(authority::Grant {
                connector: grant.connector,
                kind: grant.kind,
                pattern: authority::Pattern { segments: grant.pattern.segments.clone(), last },
            })
            .expect("grant capacity");
    }
    let mut note_resources =
        List::with_capacity(u32::try_from(value.note_resources.len()).expect("validated task note scopes"));
    for scope in &value.note_resources {
        note_resources
            .push(authority::ResourceScope {
                connector: scope.connector,
                pattern: authority::Pattern {
                    segments: scope.pattern.segments.clone(),
                    last: match &scope.pattern.last {
                        tasks::Last::Exact(bytes) => authority::Last::Exact(bytes.clone()),
                        tasks::Last::Open(bytes) => authority::Last::Open(bytes.clone()),
                    },
                },
            })
            .expect("note scope capacity");
    }
    let mut kinds = List::with_capacity(u32::try_from(value.delegation.kinds.len()).expect("validated task executors"));
    for kind in &value.delegation.kinds {
        kinds
            .push(match kind {
                tasks::AuthorityExecutor::Charter(number) => authority::Executor::Charter(*number),
                tasks::AuthorityExecutor::Procedure(number) => authority::Executor::Procedure(*number),
                tasks::AuthorityExecutor::Role(number) => authority::Executor::Role(*number),
            })
            .expect("executor capacity");
    }
    authority::Authority {
        tools: authority::Tools(value.tools.0),
        grants: grants.into_boxed(),
        delegation: authority::Delegation {
            kinds: kinds.into_boxed(),
            tasks: value.delegation.tasks,
            depth: value.delegation.depth,
        },
        budget: authority::Budget { spend: value.budget.spend, deadline: value.budget.deadline },
        notes: authority::Scopes(value.notes.0),
        note_resources: note_resources.into_boxed(),
    }
}
