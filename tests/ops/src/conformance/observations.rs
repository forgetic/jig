//! Durable-row translations and independent production evidence for jig's referee.
use jig_conformance::referee as r;
use jig_core as core;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_ops_domain as root;
use jig_ops_domain_infrastructure as infra;
use jig_test_system::SystemKey;
use std::collections::{BTreeMap, BTreeSet};

pub fn key(value: infra::Key) -> SystemKey {
    let (attempt, completion, position) = match value.origin {
        infra::Purpose::Call { attempt, completion, position } => (attempt, completion, position),
        infra::Purpose::Procedure { .. } => (0, 0, 0),
        infra::Purpose::Projection { .. } => (0, 0, 1),
    };
    SystemKey { deployment: value.deployment, task: value.task, purpose: value.purpose, attempt, completion, position }
}

pub fn parts(effect: &infra::Effect) -> (u16, infra::Service, Option<u64>, u64) {
    match effect {
        infra::Effect::Restart { service, operation } => (1, service.clone(), None, *operation),
        infra::Effect::Scale { service, from, to } => (2, service.clone(), Some(u64::from(*from)), u64::from(*to)),
        infra::Effect::Rollback { .. } | infra::Effect::CreateEnvironment { .. } | infra::Effect::TearDown { .. } => {
            panic!("the two stories use restart and scale")
        }
    }
}

pub fn resource(service: &infra::Service, connector: u16) -> r::Name {
    r::Name {
        connector,
        path: vec![b"env".to_vec(), service.environment.to_vec(), b"service".to_vec(), service.name.to_vec()],
    }
}

pub fn policy(seed: u64) -> r::Policy {
    let (configuration, _) = super::config::root(seed);
    let rules = configuration.core.authority.rules();
    let ceiling = jig_core_world::observations::scope(&rules.ceiling);
    let mut policy = r::Policy {
        deployment: ceiling.clone(),
        projects: BTreeMap::from([(1, ceiling)]),
        projections: BTreeMap::new(),
        people: BTreeMap::from([((1, 1), jig_core_world::observations::scope(&super::config::authority(500, true)))]),
        effect_accepters: BTreeSet::from([(1, 1)]),
        requirements: Vec::new(),
        implications: BTreeSet::new(),
        kinds: BTreeMap::from([
            ((2, 1), r::Kind { recovery: r::Recovery::Keyed, price: 0 }),
            ((2, 2), r::Kind { recovery: r::Recovery::Conditional, price: 0 }),
        ]),
        owned: vec![(2, r::Pattern { base: vec![b"env".to_vec()], terminal: Vec::new(), open: true })],
        participating: BTreeSet::new(),
        mechanics: BTreeSet::new(),
        delivery_bound: 60_000_000_000,
        uncertainty_bound: 60_000_000_000,
        story_steps: 2000,
    };
    for kind in [1, 2] {
        policy.requirements.push(r::Requirement {
            project: None,
            connector: 2,
            kind,
            pattern: r::Pattern { base: vec![b"env".to_vec()], terminal: Vec::new(), open: true },
            judge: 1,
            freshness: Some(30_000_000_000),
        });
    }
    policy
}

pub type Store = jig_fake_store::Store<root::Key, root::Record>;

#[expect(clippy::too_many_lines, reason = "one outside translation covers the durable row vocabulary")]
pub fn snapshot(store: &Store) -> r::Snapshot {
    let mut snapshot = r::Snapshot::default();
    let mut decisions = Vec::new();
    for record in store.rows.values() {
        match record {
            root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row) | tasks::Stored::Ended(row))) => {
                snapshot.tasks.insert(row.number, jig_core_world::observations::task(row));
                snapshot.receipts.insert(r::Receipt::Task(row.number));
                if matches!(row.phase, tasks::Phase::Ended(_)) {
                    snapshot.receipts.insert(r::Receipt::Ended(row.number));
                }
                for word in &row.inbox {
                    snapshot.receipts.insert(r::Receipt::Word(row.number, word.number));
                }
            }
            root::Record::Core(core::Record::Tasks(tasks::Stored::Writer(row))) => {
                let (task, attempt) = match row.writer {
                    tasks::Writer::Run { task, attempt } => (task, attempt),
                    tasks::Writer::Effect { entry } => (0, entry),
                };
                snapshot.writers.push((
                    r::Name {
                        connector: row.resource.connector,
                        path: row.resource.path.iter().map(|x| x.to_vec()).collect(),
                    },
                    task,
                    attempt,
                ));
            }
            root::Record::Core(core::Record::Tasks(tasks::Stored::Ledger(row))) => {
                snapshot.balances.push((
                    row.numbers.budget,
                    row.numbers.spent,
                    row.numbers.spent_below,
                    row.numbers.reserved,
                ));
                if let tasks::Funder::Pool { person, .. } = row.funder {
                    *snapshot.person_spend.entry(person).or_default() += row.numbers.spent;
                }
            }
            root::Record::Core(core::Record::People(people::Stored::SignIn { number, .. })) => {
                snapshot.receipts.insert(r::Receipt::SignIn(*number));
            }
            root::Record::Core(core::Record::People(people::Stored::Answer { key, .. })) => {
                snapshot.receipts.insert(r::Receipt::Party(key.person, key.key));
            }
            root::Record::Core(core::Record::Core(core::CoreRecord::RunProof(row))) => {
                snapshot.receipts.insert(r::Receipt::Claim(row.task, row.attempt));
            }
            root::Record::Core(core::Record::Core(core::CoreRecord::Turn(row))) => {
                snapshot.receipts.insert(r::Receipt::Turn(row.task, row.attempt, row.turn));
            }
            root::Record::Core(core::Record::Core(core::CoreRecord::Terminal(row))) => {
                snapshot.receipts.insert(r::Receipt::Terminal(row.task, row.attempt));
            }
            root::Record::Core(core::Record::Core(core::CoreRecord::Call(row))) => {
                snapshot.receipts.insert(r::Receipt::Call(
                    row.key.task,
                    row.key.attempt,
                    row.key.completion,
                    row.key.position,
                ));
                if let core::CallPart::Delegated(members) = &row.part {
                    snapshot.batches.push(members.to_vec());
                }
            }
            root::Record::Core(core::Record::Core(core::CoreRecord::ProposalDecision(row)))
                if row.choice == people::ProposalChoice::Accepted =>
            {
                decisions.push(*row);
            }
            root::Record::Infrastructure(infra::Record::Outbox(row)) => {
                let key = key(row.key);
                let (kind, service, condition, target) = parts(&row.effect);
                if row.attempt > 0 {
                    snapshot.receipts.insert(r::Receipt::Outbox(2, key, row.attempt));
                    snapshot.attempts.insert((2, key), (row.attempt, row.deadline * 1_000_000_000));
                }
                if row.phase == infra::Phase::Held {
                    snapshot.held_effects.insert((2, key));
                }
                snapshot.decisions.push(r::Decision {
                    projection: false,
                    connector: 2,
                    key,
                    kind,
                    resources: vec![resource(&service, 2)],
                    condition,
                    target,
                    judged_state: target,
                    accepted_by: None,
                });
            }
            root::Record::Core(_) | root::Record::Infrastructure(_) | root::Record::Observability(_) => {}
        }
    }
    for accepted in decisions {
        if let Some((first, last)) = accepted.created {
            for task in first..=last {
                snapshot
                    .accepted_tasks
                    .insert(task, r::Acceptance { proposal: accepted.proposal, by: r::Source::Person(accepted.by) });
            }
        }
        for decision in &mut snapshot.decisions {
            if let tasks::Party::Task(task) = accepted.proposer
                && decision.key.task == task
            {
                decision.accepted_by = Some(r::Source::Person(accepted.by));
                snapshot.acceptances.insert(
                    (2, decision.key),
                    r::Acceptance { proposal: accepted.proposal, by: r::Source::Person(accepted.by) },
                );
            }
        }
    }
    snapshot
}
