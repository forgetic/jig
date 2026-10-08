use jig_conformance_world::actions;
use jig_fake_store::Fault as StoreFault;
use jig_fake_workers::Fault;
use skein_lib::Duration;
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn held_commits_keep_assignments_and_party_answers_behind_their_barrier() {
    let mut world = actions::world(24, false);
    world.store.fault(StoreFault::Hold { commits: 1 });
    world.drain().expect("held commit conforms");
    assert!(world.peers.assignments.is_empty());
    assert_eq!(world.peers.answers, 0);
    world.release_commits();
    world.drain().expect("released commits conform");
    assert_eq!(world.peers.assignments.len(), 1);
}

#[test]
fn a_failed_commit_stops_the_engine_before_a_peer_sees_its_outputs() {
    let mut world = actions::world(25, false);
    world.store.fault(StoreFault::Fail { commit: 2 });
    assert!(world.drain().expect("failure conforms").stopped);
    assert!(world.peers.assignments.is_empty());
    assert_eq!(world.peers.answers, 0);
    world.crash().expect("cold store retains only durable evidence");
    world.drain().expect("restart after failed commit conforms");
    assert_eq!(world.peers.assignments.len(), 1);
}

#[test]
fn duplicate_party_requests_keep_one_task_and_one_reserved_activation() {
    let mut world = actions::world(26, false);
    world.drain().expect("startup conforms");
    actions::duplicate(&mut world);
    world.drain().expect("duplicate conforms");
    assert_eq!(world.peers.assignments.len(), 1);
    assert_eq!(world.peers.answers, 3);
}

#[test]
fn slow_lost_and_late_hosts_keep_their_independent_flights_and_fences() {
    for fault in [
        Fault::Slow { by: Duration::from_millis(250) },
        Fault::DropChannel { for_: Duration::from_secs(6) },
        Fault::Vanish,
    ] {
        let mut world = actions::world(27, false);
        world.drain().expect("startup conforms");
        actions::host_fault(&mut world, 0, fault);
        world.drain().expect("link fault conforms");
        actions::say(&mut world, 2);
        world.advance(7_000_000_000);
        world.drain().expect("grace and late host conform");
    }
}

#[test]
fn the_engine_decides_ahead_while_effect_commits_are_held() {
    let mut world = actions::world(28, false);
    world.drain().expect("startup conforms");
    let before = world.store.applied;
    world.store.fault(StoreFault::Hold { commits: 2 });
    actions::effect(&mut world, 1);
    world.drain().expect("first held effect conforms");
    actions::effect(&mut world, 2);
    world.drain().expect("second held effect conforms");
    assert!(world.store.applied >= before + 2);
    assert!(world.systems[0].observed().is_empty());
    world.release_commits();
    world.drain().expect("released decisions conform");
    world.release_commits();
    world.drain().expect("released attempts conform");
    assert_eq!(world.systems[0].observed().iter().filter(|effect| effect.applied).count(), 2);
}

#[test]
fn a_slow_store_pages_the_last_durable_rows_after_a_cold_restart() {
    let mut world = actions::world(29, false);
    world.drain().expect("startup conforms");
    world.store.fault(StoreFault::SlowPages { by: 3 });
    world.crash().expect("cold receipts conform");
    world.drain().expect("slow restore pages conform");
    assert_eq!(world.peers.assignments.len(), 1);
}
