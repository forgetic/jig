//! Scenario actions at outside boundaries; no action reads engine state.
use crate::{Config, Testing};
use jig_conformance::Harness;
use jig_fake_parties::{Act, Ask, Task};
use skein_lib::{ReplyTo, Time, Token, Wall};

/// The testing engine and its independent neighbours.
pub type World = Harness<Testing>;

/// Start a seeded scenario with a permanent engine slot or remote hosts.
#[must_use]
pub fn world(seed: u64, engine: bool) -> World {
    Harness::new(Config { seed, engine, workers: 2 }, seed)
}

/// Ask the assigned task to make one named, permitted keyed write.
pub fn effect(world: &mut World, completion: u32) {
    let (task, attempt) = *world.peers.assignments.last().expect("scripted caller assigned");
    let mut effect = jig_core_world::effects::World::effect();
    effect.resources =
        Box::new([jig_test_connector_world::path(1, u8::try_from(completion).expect("tiny call number"))]);
    world.send(jig_test_domain::Event::EffectCall {
        to: ReplyTo::new(Token::new(77 + u64::from(completion))),
        key: jig_core::CallKey { task, attempt, completion, position: 0 },
        number: 1,
        effect,
        deadline: Wall::from_nanos(world.clock().now + 1_000_000_000),
        proposal: None,
    });
}

/// The same party repeats its last request with the same durable request key.
pub fn duplicate(world: &mut World) {
    world.peers.peers.parties[0].extend(Box::new([Act::Twice]));
}

/// Send words from the independently signed-in party to its last task.
pub fn say(world: &mut World, key: u8) {
    world.peers.peers.parties[0].extend(Box::new([Act::Request {
        key: [key; 16],
        ask: Ask::Say { task: Task::Last, words: b"continue".as_slice().into() },
    }]));
}

/// Apply a host-link fault and feed the host's independently generated report.
pub fn host_fault(world: &mut World, index: usize, fault: jig_fake_workers::Fault) {
    let now = Time::from_nanos(world.clock().now);
    let peers = &mut world.peers.peers;
    let reports = peers.workers[index].fault(fault, now);
    let events: Vec<_> = reports.into_iter().map(|report| peers.worker_event(report)).collect();
    for event in events {
        world.send(event);
    }
}
