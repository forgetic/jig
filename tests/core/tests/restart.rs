use jig_core as core;
use jig_core_fleet as fleet;
use jig_core_tasks as tasks;
use jig_core_world::effects::World;
use jig_test_connector as connector;
use jig_test_domain as root;
use jig_test_system::Fault;
use skein_lib::{Duration, Token};

fn hello(world: &mut World, hosting: Box<[fleet::Hosted]>) {
    world.send(root::Event::Core(core::Event::Fleet(fleet::Event::Hello {
        channel: Token::new(7),
        hello: fleet::Hello { stop_bound: Duration::from_millis(100), slots: 1, workstreams: Box::new([]), hosting },
    })));
}

fn account(world: &mut World) {
    world.send(root::Event::Core(core::Event::Account(jig_core_accounts::Event::Add {
        account: 1,
        generation: 1,
        valid: Some(Duration::from_secs(60)),
    })));
}

fn applied_without_answer(world: &mut World) {
    world.hold_writes = true;
    world.call(1, 400);
    let (number, call) = world.pending_writes.remove(0);
    let _lost = world.systems[usize::from(number - 1)].answer(call, Fault::AfterApply);
    world.hold_writes = false;
}

#[test]
fn the_restart_script_waits_for_each_step_and_each_connector() {
    let limits = jig_core_world::world::limits().core;
    let config = jig_core_world::world::config(111).core;
    let mut core = core::Core::new(config, &limits);
    let steps = [
        core::RestartStep::LoadCore,
        core::RestartStep::RestoreConnector { connector: 1 },
        core::RestartStep::RestoreConnector { connector: 2 },
        core::RestartStep::AdoptRuns,
        core::RestartStep::ReadAfresh { connector: 1 },
        core::RestartStep::ReadAfresh { connector: 2 },
        core::RestartStep::SettleOutbox { connector: 1 },
        core::RestartStep::SettleOutbox { connector: 2 },
        core::RestartStep::Open,
    ];
    let mut request = core.restart_begin();
    for step in steps {
        assert_eq!(request, core::RestartRequest::Step(step));
        assert_eq!(core.restart_step(), Some(step));
        assert!(!core.restart_ready());
        assert_eq!(core.restart_begin(), core::RestartRequest::Idle);
        request = core.restart_done(step);
    }
    assert_eq!(request, core::RestartRequest::Idle);
    assert!(core.restart_ready());
    assert_eq!(core.restart_done(core::RestartStep::Open), core::RestartRequest::Idle);
}

#[test]
fn an_out_of_order_restart_completion_refuses_the_start_naming_its_step() {
    let limits = jig_core_world::world::limits().core;
    let mut core = core::Core::new(jig_core_world::world::config(112).core, &limits);
    assert_eq!(core.restart_begin(), core::RestartRequest::Step(core::RestartStep::LoadCore));
    assert_eq!(
        core.restart_done(core::RestartStep::Open),
        core::RestartRequest::Refused { step: core::RestartStep::LoadCore }
    );
    assert_eq!(core.restart_failure(), Some(core::RestartStep::LoadCore));
    assert!(!core.restart_ready());
}

#[test]
fn an_uncertain_effect_across_restarts_is_made_before_its_retry_deadline_and_once() {
    let mut world = World::new(113, false);
    applied_without_answer(&mut world);
    let (key, deadline) = world
        .store
        .rows
        .values()
        .find_map(|record| match record {
            root::Record::Connector { number: 1, record: connector::Record::Outbox(row) } => {
                Some((row.key, row.attempt.expect("attempt committed").deadline))
            }
            root::Record::Core(_) | root::Record::Connector { .. } => None,
        })
        .expect("sent outbox record");
    world.restart(113, false);
    assert!(world.domain.ready());
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Connector { number: 1, record: connector::Record::Made { key: found, .. } } if *found==key)));
    assert!(
        world
            .store
            .rows
            .values()
            .any(|record| matches!(record, root::Record::Core(core::Record::Core(core::CoreRecord::Deployment(_)))))
    );
    let applied = world.systems[0].observed();
    assert_eq!(applied.iter().filter(|row| row.applied).count(), 1);
    assert!(world.trace.iter().any(|line| line.contains("Look")));
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Core(core::Record::Core(core::CoreRecord::Call(row))) if matches!(row.part, core::CallPart::Effect { outcome: Some(core::connector::OutboxOutcome::Made), .. }))));
    assert!(deadline > world.wall_time(), "lookup resolved long before the saved retry deadline");
    world.restart(113, false);
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}

#[test]
fn an_effect_of_a_kind_that_cannot_be_recovered_is_held_for_a_person_when_uncertain() {
    let mut world = World::new(114, false);
    world.recovery = connector::Recovery::Unrecoverable;
    world.restart(114, false);
    applied_without_answer(&mut world);
    world.restart(114, false);
    assert!(world.domain.ready());
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Connector { record: connector::Record::Outbox(row), .. } if row.phase==connector::EffectPhase::Held)));
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if matches!(row.phase, tasks::Phase::Held { why: tasks::Hold::Uncertain { .. }, .. }))));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}

#[test]
fn nothing_is_decided_before_every_connector_has_settled_its_outbox() {
    let mut world = World::new(115, false);
    applied_without_answer(&mut world);
    world.end(tasks::End::Parked, 0);
    for record in world.store.rows.values_mut() {
        if let root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) = record {
            row.phase = tasks::Phase::Active(tasks::Active::Due);
        }
    }
    world.assigned = None;
    world.hold_lookups = true;
    world.restart(115, false);
    assert!(!world.domain.ready());
    assert_eq!(world.pending_lookups.len(), 1);
    hello(&mut world, Box::new([]));
    account(&mut world);
    assert!(world.assigned.is_none(), "a due run cannot be assigned while recovery waits");
    world.finish_lookups();
    assert!(world.domain.ready());
    assert!(world.assigned.is_some(), "the due run starts after every connector settled");
}

#[test]
fn a_restart_after_a_turn_committed_resumes_from_it() {
    let mut world = World::new(116, false);
    world.turn(1, 3, None, b"committed whole turn");
    let assignment = world.assigned.as_ref().expect("live worker");
    let hosted = fleet::Hosted {
        run: Token::new(assignment.task),
        attempt: Token::new(assignment.attempt),
        phase: fleet::Phase::Active,
    };
    world.restart(116, false);
    hello(&mut world, Box::new([hosted]));
    world.end(tasks::End::Parked, 3);
    account(&mut world);
    world.say(90);
    assert_eq!(
        world.assigned.as_ref().expect("resumed").transcript.as_ref(),
        [Box::<[u8]>::from(&b"committed whole turn"[..])]
    );
    assert_eq!(world.spent(), 3);
}

#[test]
fn an_engine_claim_is_settled_at_once_instead_of_waiting_for_a_worker() {
    let mut world = World::new(117, false);
    world.turn(1, 2, None, b"engine turn");
    for record in world.store.rows.values_mut() {
        if let root::Record::Core(core::Record::Core(core::CoreRecord::RunProof(proof))) = record {
            proof.host = fleet::HostKind::Engine;
        }
    }
    world.restart(117, false);
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if row.tries.lost==1)));
    assert!(world.domain.ready());
}

#[test]
fn an_unfound_uncertain_entry_keeps_its_saved_retry_deadline_across_restarts() {
    let mut world = World::new(118, false);
    world.hold_writes = true;
    world.call(1, 401);
    let attempt = world
        .store
        .rows
        .values()
        .find_map(|record| match record {
            root::Record::Connector { record: connector::Record::Outbox(row), .. } => row.attempt,
            root::Record::Core(_) | root::Record::Connector { .. } => None,
        })
        .expect("durable attempt");
    world.restart(118, false);
    assert!(world.domain.ready());
    assert!(world.systems[0].observed().is_empty(), "not found does not retry before the saved deadline");
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Connector { record: connector::Record::Outbox(row), .. } if row.attempt==Some(attempt))));
    world.restart(118, false);
    assert!(world.systems[0].observed().is_empty());
    assert!(world.store.rows.values().any(|record| matches!(record, root::Record::Connector { record: connector::Record::Outbox(row), .. } if row.attempt==Some(attempt))));
}
