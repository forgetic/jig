//! Seeded V2 host, parent and committing engine histories across channel loss.

use jig_host_world::turn_world::World;
use skein_lib::Rng;

#[test]
fn turn_windows_and_replays_settle_under_seeded_contact_histories() {
    for seed in 0..512 {
        let mut rng = Rng::new(seed);
        let mut world = World::new();
        let workspace = rng.chance(500);
        world.assign(workspace);
        if workspace && rng.chance(500) {
            world.deliver();
        }
        if rng.chance(500) {
            world.relay();
        }
        let first = vec![u8::try_from(seed % 251).expect("small"); usize::try_from(rng.between(1, 32)).expect("small")];
        world.turn(1, &first);
        if rng.chance(500) {
            world.retry_busy(1);
        }
        let lost = rng.chance(500);
        if lost {
            world.lose_contact();
        }
        let second =
            vec![u8::try_from((seed + 1) % 251).expect("small"); usize::try_from(rng.between(1, 32)).expect("small")];
        world.turn(2, &second);
        let parked = rng.chance(500);
        world.finish(2, parked);
        if lost {
            world.reconnect();
        }
        if rng.chance(500) {
            world.commit_turn(2);
            world.commit_turn(1);
        } else {
            world.commit_turn(1);
            world.commit_turn(2);
        }
        world.acknowledge_answer();
        world.assert_settled();
        let stats = world.stats();
        assert_eq!((stats.commits, stats.turn_acks, stats.answers, stats.answer_acks), (2, 2, 1, 1), "seed {seed}");
    }
}
