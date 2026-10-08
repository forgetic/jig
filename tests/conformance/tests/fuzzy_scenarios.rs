use jig_conformance::{
    Cut,
    scenarios::{ALL, run},
};
use jig_conformance_world::Testing;
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn every_shared_story_survives_every_commit_and_seeded_random_cuts() {
    let mut random = skein_lib::Rng::new(31);
    for scenario in ALL {
        let seed = random.below(1000);
        let baseline = run::<Testing>(scenario, Cut::None, seed).expect("baseline").expect("bound kinds");
        for cut in (1..=baseline.commits).map(Cut::AfterCommit).chain([Cut::Random { seed }]) {
            run::<Testing>(scenario, cut, seed)
                .unwrap_or_else(|error| panic!("{scenario:?}, {cut:?}: {error:?}"))
                .expect("bound kinds");
        }
    }
}
