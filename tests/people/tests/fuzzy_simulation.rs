use jig_people_world::{ENDINGS, Settings, World};
use std::collections::BTreeSet;

#[test]
fn random_people_settle_and_reach_every_ending() {
    let mut endings = BTreeSet::new();
    for seed in 0..256 {
        let mut world = World::new(Settings::random(seed));
        world.run();
        endings.extend(world.stats().endings.iter().copied());
    }
    for ending in ENDINGS {
        assert!(endings.contains(ending), "{ending}: {endings:?}");
    }
}
