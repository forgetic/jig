use jig_core_world::{
    effects::{Cut, World},
    faults,
};
use jig_fake_store::Fault as StoreFault;
use jig_fake_workers::Fault;
use skein_lib::Duration;

fn effect_scenario(world: &mut World) {
    world.turn(1, 2, None, b"before effect");
    if world.reached_cut.is_some() {
        return;
    }
    world.call(1, 100);
    if world.reached_cut.is_some() {
        return;
    }
    world.end(jig_core_tasks::End::Parked, 2);
}

#[test]
fn every_commit_of_a_turn_effect_and_terminal_survives_both_crash_cuts() {
    let mut reference = World::new(300, false);
    let first = reference.store.applied + 1;
    effect_scenario(&mut reference);
    let last = reference.store.applied;
    assert!(last >= first + 2);
    for number in first..=last {
        for cut in [Cut::Submitted(number), Cut::Durable(number)] {
            let mut world = World::new(300, false);
            world.cut = Some(cut);
            effect_scenario(&mut world);
            assert_eq!(world.reached_cut, Some(cut));
            let applied = world.systems[0].observed().iter().filter(|row| row.applied).count();
            world.store.fault(StoreFault::SlowPages { by: 2 });
            world.restart(300, false);
            assert!(world.domain.ready());
            assert!(!world.stopped);
            assert!(world.store.pending.is_empty());
            let recovered = world.systems[0].observed().iter().filter(|row| row.applied).count();
            assert!(recovered >= applied && recovered <= 1, "cut {cut:?}");
            world.restart(300, false);
            assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), recovered);
        }
    }
}

#[test]
fn a_failed_commit_at_every_scenario_boundary_stops_before_any_later_output() {
    let mut reference = World::new(301, false);
    let first = reference.store.applied + 1;
    effect_scenario(&mut reference);
    for number in first..=reference.store.applied {
        let mut world = World::new(301, false);
        world.store.fault(StoreFault::Fail { commit: number });
        effect_scenario(&mut world);
        assert!(world.stopped, "failed commit {number}");
        assert_eq!(world.store.applied, number - 1);
        let rows = world.store.rows.clone();
        let outputs = world.trace.len();
        world.drain();
        assert_eq!(world.store.rows, rows);
        assert_eq!(world.trace.len(), outputs);
        world.restart(301, false);
        assert!(world.domain.ready());
        assert!(world.systems[0].observed().iter().filter(|row| row.applied).count() <= 1);
    }
}

#[test]
fn a_slow_worker_keeps_its_assignment_and_resumes_after_the_delay() {
    let mut world = faults::workers(302, 1, false);
    faults::worker_fault(&mut world, 0, Fault::Slow { by: Duration::from_secs(1) });
    faults::say(&mut world, 2);
    assert_eq!(world.peers.as_ref().expect("peers").workers[0].turn_acks, 0);
    world.advance(Duration::from_secs(1));
    let worker = &world.peers.as_ref().expect("peers").workers[0];
    assert_eq!(worker.seen.len(), 1);
    assert_eq!(worker.turn_acks, 1);
    assert_eq!(worker.answer_acks, 1);
}

#[test]
fn a_worker_back_after_its_stop_bound_replays_a_parked_answer() {
    let mut world = faults::workers(303, 1, false);
    faults::worker_fault(&mut world, 0, Fault::DropChannel { for_: Duration::from_secs(1) });
    world.advance(Duration::from_secs(1));
    let worker = &world.peers.as_ref().expect("peers").workers[0];
    assert_eq!(worker.answer_acks, 1);
    assert_eq!(worker.seen.len(), 1);
    faults::say(&mut world, 2);
    assert_eq!(world.peers.as_ref().expect("peers").workers[0].seen.len(), 2);
}

#[test]
fn a_worker_returning_past_grace_cannot_charge_or_finish_its_old_attempt() {
    let mut world = faults::workers(304, 1, false);
    faults::worker_fault(&mut world, 0, Fault::DropChannel { for_: Duration::from_secs(7) });
    world.advance(Duration::from_secs(6));
    world.advance(Duration::from_secs(1));
    world.advance(Duration::from_secs(2));
    let worker = &world.peers.as_ref().expect("peers").workers[0];
    assert_eq!(worker.actual_spent, 0);
    assert_eq!(worker.seen.len(), 2, "the stale listing does not occupy the retry slot");
    assert!(worker.seen[1].attempt > worker.seen[0].attempt);
    assert!(worker.answer_acks >= 1, "the stale parked answer is fenced and acknowledged");
}

#[test]
fn a_vanished_worker_is_lost_and_another_worker_takes_the_retry() {
    let mut world = faults::workers(305, 2, false);
    let owner = world
        .peers
        .as_ref()
        .expect("peers")
        .workers
        .iter()
        .position(|worker| !worker.seen.is_empty())
        .expect("assigned host");
    let first = world.peers.as_ref().expect("peers").workers[owner].seen[0].clone();
    faults::worker_fault(&mut world, owner, Fault::Vanish);
    world.advance(Duration::from_secs(6));
    world.advance(Duration::from_secs(2));
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.workers[1 - owner].seen.iter().any(|run| run.task == first.task && run.attempt > first.attempt));
    assert_eq!(peers.workers[owner].seen.len(), 1);
}

#[test]
fn a_cold_restart_retries_a_party_request_and_retained_host_traffic() {
    let mut world = faults::workers(306, 1, false);
    let cut = Cut::Durable(world.store.applied + 1);
    world.cut = Some(cut);
    faults::say(&mut world, 2);
    assert_eq!(world.reached_cut, Some(cut));
    faults::restart(&mut world, 306, false);
    assert!(world.domain.ready());
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.parties[0].quiescent(), "request reply retried");
    assert!(peers.upstream.iter().filter(|up| matches!(up, jig_fake_workers::Up::Hello { .. })).count() >= 2);
    let task = peers.parties[0].last_task.expect("started chat");
    assert!(
        world.store.rows.values().any(|row| matches!(row,
            jig_test_domain::Record::Core(jig_core::Record::Tasks(jig_core_tasks::Stored::Live(row)))
                if row.number == task && row.inbox.iter().any(|word| word.words.as_ref() == b"continue")
        )),
        "the unanswered relay stays in the durable whole inbox"
    );
}

#[test]
fn every_startup_sign_in_chat_and_assignment_commit_restarts_on_either_side_of_durability() {
    for engine in [false, true] {
        let reference = faults::workers(307, 1, engine);
        for number in 1..=reference.store.applied {
            for cut in [Cut::Submitted(number), Cut::Durable(number)] {
                let mut world = faults::workers_at(307, 1, engine, Some(cut));
                assert_eq!(world.reached_cut, Some(cut));
                faults::restart(&mut world, 307, engine);
                world.advance(Duration::from_secs(6));
                world.advance(Duration::from_secs(2));
                assert!(world.domain.ready());
                let peers = world.peers.as_ref().expect("peers");
                assert!(peers.parties[0].person.is_some());
                assert!(peers.parties[0].last_task.is_some());
                assert!(peers.parties[0].quiescent());
                let made = world
                    .store
                    .rows
                    .values()
                    .filter(|row| {
                        matches!(
                            row,
                            jig_test_domain::Record::Core(jig_core::Record::Tasks(jig_core_tasks::Stored::Live(_)))
                        )
                    })
                    .count();
                assert_eq!(made, 1, "one keyed chat at {cut:?}, engine {engine}");
            }
        }
    }
}
