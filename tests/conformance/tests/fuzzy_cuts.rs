use jig_conformance::Cut;
use jig_conformance_world::actions;
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn every_durable_cut_and_random_cuts_replay_the_same_effect_story() {
    for seed in [1, 29, 74, 233] {
        let mut baseline = actions::world(seed, false);
        baseline.drain().expect("baseline startup");
        actions::effect(&mut baseline, 1);
        let count = baseline.drain().expect("baseline effect").commits;
        for cut in (1..=count).map(Cut::AfterCommit).chain([Cut::Random { seed }]) {
            let mut world = actions::world(seed, false);
            world.cut(cut);
            world.drain().unwrap_or_else(|error| panic!("seed {seed}, cut {cut:?}: {error:?}"));
            if world.peers.assignments.is_empty() {
                world.advance(6_000_000_000);
                world.drain().expect("unseen claim passes its adoption grace");
                world.advance(1_000_000_000);
                world.drain().expect("replacement passes its retry backoff");
            }
            actions::effect(&mut world, 1);
            world.drain().unwrap_or_else(|error| panic!("seed {seed}, cut {cut:?}: {error:?}"));
            if !world.systems[0].observed().iter().any(|effect| effect.applied) {
                world.advance(2_000_000_000);
                world.drain().expect("effect retry deadline passes");
            }
            assert_eq!(
                world.systems[0].observed().iter().filter(|effect| effect.applied).count(),
                1,
                "seed {seed}, cut {cut:?}"
            );
        }
    }
}
