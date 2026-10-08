use jig_conformance::{
    Harness,
    scenarios::{Action, Scenario, ScenarioApplication},
};
use jig_conformance_world::{Config, Testing};
use jig_fake_parties::{Act, Ask};
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn full_live_task_and_host_slots_fit_with_a_decision_held_in_the_journal() {
    let mut world = Harness::<Testing>::new(Config::new(31, false, 3, Some(Scenario::ShrinkingPool)), 31);
    world.drain().expect("startup heap fits");
    Testing::action(&mut world, Action::FillPool).expect("two holders and a waiter fit");
    world.drain().expect("full host slots fit");
    assert_eq!(world.peers.assignments.len(), 3);
    world.store.fault(jig_fake_store::Fault::Hold { commits: 1 });
    world.peers.peers.parties[0].extend(Box::new([Act::Request {
        key: [42; 16],
        ask: Ask::Chat { words: b"fifth live task".as_slice().into() },
    }]));
    world.drain().expect("full live task slots and held journal decision fit");
    let live = world
        .store
        .rows
        .values()
        .filter(|row| {
            matches!(row, jig_test_domain::Record::Core(jig_core::Record::Tasks(jig_core_tasks::Stored::Live(_))))
        })
        .count();
    assert_eq!(live, 5, "the fixture's complete live task capacity is occupied");
    assert_eq!(world.peers.assignments.len(), 3, "held answer and full hosts admit no new assignment");
    assert!(!world.store.held.is_empty());
    world.release_commits();
    world.drain().expect("releasing the decision retains the bound");
}
