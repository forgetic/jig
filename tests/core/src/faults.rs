//! Replayable fault scenarios for `domain/testing.md`, 4, 7. Configuration
//! comes from the scenario, choices from a seed, and the referee observes every
//! drain. Conformance can reuse the independent peers and the chosen cuts.
use crate::{effects::World, peers::Peers};
use jig_fake_parties::{Act, Ask, Party, Role, Task};
use jig_fake_workers::{Fault, Script, Worker};
use skein_lib::{Duration, Time};

/// One owner, one to three workers and optionally one permanent engine slot.
/// Every activation waits for a message before taking its turn and parking.
#[must_use]
pub fn workers(seed: u64, count: u32, engine: bool) -> World {
    workers_at(seed, count, engine, None)
}

/// The same actors with a crash cut armed before startup.
#[must_use]
pub fn workers_at(seed: u64, count: u32, engine: bool, cut: Option<crate::effects::Cut>) -> World {
    assert!((1..=3).contains(&count));
    let (config, limits) = crate::peers::fixture(seed, u32::from(engine));
    let script = || {
        Box::new([
            Script::Wait,
            Script::Turn { body: b"kept whole turn".as_slice().into(), cost: 2, read: None },
            Script::Park,
        ]) as Box<[Script]>
    };
    let mut hosts: Vec<_> =
        (0..count).map(|index| Worker::new(7 + u64::from(index), 1, vec![script(), script()])).collect();
    if engine {
        hosts.insert(0, Worker::new(0, 1, vec![script(), script()]));
    }
    World::scripted_at(
        config,
        limits,
        Peers::new(
            hosts,
            vec![Party::new(
                1,
                Role::Owner,
                10_000,
                Box::new([
                    Act::SignIn { provider: 0, subject: 7_u64.to_be_bytes().into() },
                    Act::Request { key: [1; 16], ask: Ask::Chat { words: b"work".as_slice().into() } },
                ]),
            )],
        ),
        cut,
    )
}

/// Resume the owner's last chat, retaining the key for reproducible retries.
pub fn say(world: &mut World, key: u8) {
    world.peers.as_mut().expect("scripted peers").parties[0].extend(Box::new([Act::Request {
        key: [key; 16],
        ask: Ask::Say { task: Task::Last, words: b"continue".as_slice().into() },
    }]));
    world.drain();
}

/// Apply a link fault to the selected independent host and route its own report.
pub fn worker_fault(world: &mut World, index: usize, fault: Fault) {
    let now = Time::from_nanos(world.wall_time().as_nanos());
    let peers = world.peers.as_mut().expect("scripted peers");
    let reports = peers.workers[index].fault(fault, now);
    let events: Vec<_> = reports.into_iter().map(|up| peers.worker_event(up)).collect();
    for event in events {
        world.send(event);
    }
    world.drain();
}

/// Cold restart with the scenario's original charter and host capacities.
pub fn restart(world: &mut World, seed: u64, engine: bool) {
    world.restart_with(crate::peers::fixture(seed, u32::from(engine)).0);
    // Account usability is an independently refreshed runtime capability.
    world.send(jig_test_domain::Event::Core(jig_core::Event::Account(jig_core_accounts::Event::Add {
        account: 1,
        generation: 1,
        valid: Some(Duration::from_secs(60)),
    })));
}

/// The bounded two-level plan fixture used by resource and failure stories.
#[must_use]
pub fn plan_fixture(seed: u64) -> (jig_test_domain::Config, jig_test_domain::Limits) {
    let (mut configuration, mut limits) = crate::effects::fixture(seed, false);
    limits.core.tasks.tasks = 5;
    limits.core.tasks.project_tasks = 5;
    limits.core.tasks.tree_tasks = 5;
    limits.core.tasks.depth = 2;
    limits.core.tasks.delegates = 3;
    limits.core.tasks.funders = 16;
    limits.core.tasks.inbox_bytes = 512;
    limits.core.tasks.inbox_messages = 8;
    limits.core.views.runs = 5;
    limits.core.fleet.slots = 3;
    limits.core.fleet.attempts = 5;
    limits.core.call_records = 8;
    limits.journal.writes = 10_000;
    limits.journal.held = 10_000;
    configuration.core.settings.chat_authority.delegation = jig_core_authority::Delegation {
        kinds: Box::new([jig_core_authority::Executor::Charter(1), jig_core_authority::Executor::Procedure(1)]),
        tasks: 4,
        depth: 2,
    };
    let mut rules = configuration.core.authority.rules().clone();
    rules.ceiling.delegation.tasks = 6;
    rules.ceiling.delegation.depth = 3;
    rules.maximum_run_spend = 10;
    let mut policy = configuration.core.authority.policy(1).expect("plan policy").clone();
    policy.ceiling.delegation = rules.ceiling.delegation.clone();
    policy.roles[0].authority.delegation = rules.ceiling.delegation.clone();
    let mut checked = jig_core_authority::Domain::new(rules, limits.core.authority).expect("bounded plan policy");
    let mut out = skein_lib::Queue::with_capacity(jig_core_authority::POLICY_MAX_OUT);
    jig_core_authority::step(&mut checked, jig_core_authority::Event::Policy { project: 1, policy }, &mut out);
    assert_eq!(out.pop(), Some(jig_core_authority::PolicyFact::Added { project: 1 }));
    configuration.core.authority = checked;
    (configuration, limits)
}
