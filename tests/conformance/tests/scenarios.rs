use jig_conformance::{
    Cut,
    scenarios::{ALL, Scenario, run},
};
use jig_conformance_world::Testing;
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn every_shared_adversarial_story_runs_on_the_testing_application() {
    for scenario in ALL {
        run::<Testing>(scenario, Cut::None, 31)
            .unwrap_or_else(|error| panic!("{scenario:?}: {error:?}"))
            .expect("application binds all required kinds");
    }
}

#[test]
fn chosen_effect_admission_and_completion_cuts_end_every_shared_story() {
    for scenario in ALL {
        let commit = if scenario == Scenario::RunBudget { 6 } else { 7 };
        run::<Testing>(scenario, Cut::AfterCommit(commit), 31)
            .unwrap_or_else(|error| panic!("{scenario:?}, commit {commit}: {error:?}"))
            .expect("application binds all required kinds");
    }
}
