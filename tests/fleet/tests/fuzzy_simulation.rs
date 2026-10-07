//! The engine's fleet child domain at random: many random worlds, each settled,
//! every ending reached among them.

use std::collections::BTreeMap;

use jig_fleet_world::{ENDINGS, Settings, World};

const ITERATIONS: u32 = 400_000;

fn run(settings: &Settings) -> World {
    let mut world = World::new(*settings);
    world.run(ITERATIONS);
    world
}

#[test]
fn random_worlds_settle_and_reach_every_ending() {
    let mut endings: BTreeMap<&str, u32> = BTreeMap::new();
    for seed in 0..300 {
        let world = run(&Settings::random(seed));
        for (ending, count) in world.stats().endings {
            *endings.entry(ending).or_insert(0) += count;
        }
    }
    for ending in ENDINGS {
        assert!(endings.get(ending).copied().unwrap_or(0) > 0, "{ending} reached in some world: {endings:?}");
    }
}
