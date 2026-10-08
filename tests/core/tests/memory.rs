use jig_core as core;
use jig_core_accounts as accounts;
use jig_core_fleet as fleet;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_world::world;
use skein_lib::{Duration, Env, Time, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

#[test]
fn core_children_routes_and_an_in_flight_decision_fit_the_checked_bound() {
    let limits = world::limits();
    let bound = core::worst_case(&limits.core).expect("walking limits price every core owner");
    let configuration = world::config(91);
    let mut observer = jig_core_world::observations::Observer::new(&configuration);
    let config = configuration.core;
    let meter = Meter::new();
    meter.start();
    let mut domain = core::Core::new(config, &limits.core);
    let measured = meter.end();
    meter.check(measured, bound, &limits.core);

    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: limits.core };
    for event in [
        core::Event::People(people::Event::Restored),
        core::Event::Tasks(tasks::Event::Restored),
        core::Event::Fleet(fleet::Event::Loaded),
        core::Event::Account(accounts::Event::Add { account: 1, generation: 1, valid: Some(Duration::from_secs(60)) }),
    ] {
        let room = core::room(&limits.core, &event).expect("event route has representable room");
        assert!(room.writes <= limits.journal.writes);
        assert!(room.held <= limits.journal.held);
        meter.start();
        let requests = core::step(&mut domain, &env, event);
        let measured = meter.end();
        drop(requests);
        let held = meter.check(measured, bound, &limits.core);
        observer.observe(0, jig_core_world::referee::Observed::Heap { held, maximum: bound });
    }
    observer.observe(0, jig_core_world::referee::Observed::Heap { held: meter.held(), maximum: bound });
}

#[test]
fn room_and_memory_refuse_unrepresentable_or_missing_limits() {
    let limits = world::limits().core;
    assert!(core::worst_case(&core::Limits { connectors: 0, ..limits }).is_some());
    assert!(core::worst_case(&core::Limits { resume_bytes: 0, ..limits }).is_none());
    assert!(core::worst_case(&core::Limits { call_records: 0, ..limits }).is_none());
    assert!(
        core::worst_case(&core::Limits { tasks: tasks::Limits { tasks: u32::MAX, ..limits.tasks }, ..limits })
            .is_none()
    );
}

#[test]
fn both_worlds_check_the_counted_heap_at_every_iteration() {
    let mut walking = world::World::goal(190);
    walking.run_goal();
    drop(walking);
    let mut effects = jig_core_world::effects::World::new(191, false);
    effects.delegate();
    effects.call(2, 9000);
    effects.restart(191, false);
    assert!(jig_core_world::observations::root_bound(&world::limits()).is_some());
}
