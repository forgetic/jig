use jig_conformance::Harness;
use jig_conformance_world::{Config, Testing};
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn a_testing_engine_runs_against_independent_parties_workers_and_two_systems() {
    for engine in [false, true] {
        let mut world = Harness::<Testing>::new(Config { seed: 17, engine, workers: 2 }, 17);
        let result = world.drain().expect("application conforms");
        assert!(!result.stopped);
        assert!(result.commits > 0);
        assert_eq!(world.peers.assignments.len(), 1, "outcome {result:?}, trace {:?}", world.trace);
        assert_eq!(world.peers.answers, 2);
        assert!(world.peers.restart_steps.contains(&jig_core::RestartStep::RestoreConnector { connector: 1 }));
        assert!(world.peers.restart_steps.contains(&jig_core::RestartStep::RestoreConnector { connector: 2 }));
    }
}

#[test]
fn the_system_observes_the_committed_effect_of_the_calling_task() {
    use skein_lib::{ReplyTo, Token, Wall};
    let mut world = Harness::<Testing>::new(Config { seed: 18, engine: false, workers: 1 }, 18);
    world.drain().expect("assigned conforming caller");
    let (task, attempt) = world.peers.assignments[0];
    world.send(jig_test_domain::Event::EffectCall {
        to: ReplyTo::new(Token::new(77)),
        key: jig_core::CallKey { task, attempt, completion: 1, position: 0 },
        number: 1,
        effect: jig_core_world::effects::World::effect(),
        deadline: Wall::from_nanos(1_000_000_000),
        proposal: None,
    });
    world.drain().expect("effect conforms");
    assert_eq!(world.systems[0].observed().len(), 1);
    assert!(world.systems[0].observed()[0].applied);
    assert_eq!(world.systems[0].observed()[0].kind, 5);
}
