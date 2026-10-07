use jig_core_notes::{Event, Scope};
use jig_notes_world::{World, one_scope, task_entry};
use skein_lib::{Rng, Token};
use skein_world::domain::assert_replays;

fn run(seed: u64) -> (Vec<String>, std::collections::BTreeMap<jig_core_notes::Key, jig_core_notes::Record>) {
    let mut rng = Rng::new(seed);
    let mut world = World::default();
    for turn in 0..12u64 {
        let project = if rng.chance(500) { 7 } else { 8 };
        let scope = Scope::Project { project };
        let name = turn.checked_add(100).expect("turn fits");
        let _answer = world.call(Event::Write {
            owner: Token::new(name),
            entry: task_entry(name, scope.clone(), b"warm service", b"wait for warmup", 9),
            recalled: None,
        });
        let _index = world.call(Event::Index {
            owner: Token::new(name.checked_add(1000).expect("token fits")),
            scopes: one_scope(scope),
            most: 2,
        });
        if rng.chance(250) {
            world.restart();
        }
    }
    (world.trace().to_vec(), world.records().clone())
}

#[test]
fn seeded_writes_pages_and_restarts_replay_the_same_durable_entries() {
    for seed in 1..17 {
        let trace = assert_replays(seed, seed + 100, run);
        assert!(!trace.is_empty(), "seed {seed} exercised the world");
    }
}
