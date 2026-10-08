use jig_core as core;
use jig_core_authority as authority;
use jig_core_tasks as tasks;
use jig_core_world::effects::World;
use jig_test_connector as connector;
use jig_test_domain as root;

#[test]
fn a_procedures_effect_waits_for_another_connectors_verdict_then_is_made() {
    let mut world = World::new(81, true);
    world.delegate();
    let task = world.procedure.expect("delegated procedure");
    assert!(world.systems[0].observed().is_empty(), "unknown observer facts authorize no effect");
    assert!(!world.pending_reads.is_empty(), "the second connector reads for its verdict");
    assert!(world.store.rows.values().any(|row| matches!(row, root::Record::Connector { number: 1, record: connector::Record::Procedure(state) } if state.task == task && state.steps == 1 && !state.awaiting)), "refused effect is counted, with no outstanding entry");
    world.observed(13);
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1, "{:?}", world.trace);
    assert_eq!(world.systems[0].observed()[0].key.purpose, 19);
    assert!(
        world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(task)))),
        "procedure finishes after its made outcome"
    );
}

#[test]
fn a_tool_call_whose_answer_is_lost_with_the_channel_and_the_engine_is_asked_again_after_a_restart_and_answered_from_its_record()
 {
    let mut world = World::new(82, false);
    world.lose_answer = true;
    world.call(1, 202);
    let key = world.call_key(1);
    assert_eq!(world.lost_answers, 1);
    assert_eq!(world.spent(), 3, "the effect's maximum is charged in its deciding commit");
    let row = world
        .store
        .rows
        .get(&root::Key::Core(core::Key::Core(core::CoreKey::Call(key))))
        .expect("saved call answer")
        .clone();
    assert!(matches!(
        row,
        root::Record::Core(core::Record::Core(core::CoreRecord::Call(core::CallRecord {
            part: core::CallPart::Effect { outcome: Some(core::connector::OutboxOutcome::Made), .. },
            ..
        })))
    ));
    let commits = world.store.applied;
    world.restart(82, false);
    world.call(1, 203);
    assert_eq!(world.answers.len(), 1);
    assert_eq!(world.answers[0].0.raw(), 203);
    assert!(matches!(
        world.answers[0].2,
        core::CallPart::Effect { outcome: Some(core::connector::OutboxOutcome::Made), .. }
    ));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
    assert_eq!(world.store.applied, commits, "replay decides nothing again");
    assert_eq!(world.spent(), 3, "replay cannot charge the effect twice");
}

#[test]
fn an_effect_call_wait_commits_nothing_and_the_same_name_can_be_asked_again() {
    let mut world = World::new(83, true);
    let commits = world.store.applied;
    world.call(1, 204);
    assert_eq!(world.store.applied, commits);
    assert_eq!(world.spent(), 0, "a waiting effect is not charged");
    assert!(matches!(
        world.answers.last(),
        Some((_, _, core::CallPart::EffectDenied { answer: authority::Answer::Wait, .. }))
    ));
    assert!(world.systems[0].observed().is_empty());
    world.observed(13);
    world.call(1, 205);
    assert!(matches!(
        world.answers.last(),
        Some((_, _, core::CallPart::Effect { outcome: Some(core::connector::OutboxOutcome::Made), .. }))
    ));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}

#[test]
fn an_effect_answers_committed_and_pending_only_past_its_absolute_deadline() {
    let mut world = World::new(84, false);
    world.hold_writes = true;
    world.call(1, 206);
    assert!(world.answers.is_empty(), "keeping an entry does not answer made");
    assert_eq!(world.pending_writes.len(), 1);
    let commits = world.store.applied;
    world.call(1, 207);
    assert!(world.answers.is_empty(), "a pending duplicate replaces its lost reply right");
    assert_eq!(world.store.applied, commits);
    world.deadline();
    assert_eq!(world.answers.len(), 1);
    assert_eq!(world.answers[0].0.raw(), 207);
    assert!(matches!(world.answers[0].2, core::CallPart::Effect { outcome: None, .. }));
    assert_eq!(world.store.applied, commits, "deadline reports the previously committed entry");
    world.release_writes();
    assert_eq!(world.answers.len(), 1, "the expired reply right is consumed once");
    world.call(1, 208);
    assert!(matches!(
        world.answers.last(),
        Some((_, _, core::CallPart::Effect { outcome: Some(core::connector::OutboxOutcome::Made), .. }))
    ));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}

#[test]
fn an_explicit_effect_proposal_is_kept_across_a_restart_and_checked_again_before_acceptance() {
    let mut world = World::new(85, true);
    let proposal = world.propose(1);
    assert!(world.systems[0].observed().is_empty());
    assert!(
        world
            .store
            .rows
            .contains_key(&root::Key::Connector { number: 1, key: connector::RecordKey::Proposal(proposal) })
    );
    world.restart(85, true);
    world.waiting_judge();
    let commits = world.store.applied;
    world.decide_proposal(proposal, true, 1);
    assert!(world.systems[0].observed().is_empty(), "acceptance cannot override an unknown judge");
    assert_eq!(world.store.applied, commits, "a waiting judge commits no accepted proposal");
    world.observed(13);
    world.decide_proposal(proposal, true, 2);
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1, "{:?}", world.trace);
    assert!(
        !world
            .store
            .rows
            .contains_key(&root::Key::Connector { number: 1, key: connector::RecordKey::Proposal(proposal) })
    );
    assert_eq!(world.systems[0].observed()[0].key.attempt, world.call_key(1).attempt);
    assert_eq!(world.systems[0].observed()[0].key.completion, 1);
    assert_eq!(world.spent(), 0, "the proposal's accepter funds the effect");
    assert_eq!(world.holder_spent(), 3);
}

#[test]
fn rejecting_an_effect_proposal_erases_its_payload_without_making_it() {
    let mut world = World::new(86, false);
    let proposal = world.propose(1);
    world.decide_proposal(proposal, false, 3);
    assert!(world.systems[0].observed().is_empty());
    assert!(
        !world
            .store
            .rows
            .contains_key(&root::Key::Connector { number: 1, key: connector::RecordKey::Proposal(proposal) })
    );
}

#[test]
fn a_full_connector_outbox_is_busy_without_a_record_or_charge_and_can_retry_the_same_name() {
    let mut world = World::with_entries(87, false, 1);
    world.hold_writes = true;
    world.call(1, 210);
    let commits = world.store.applied;
    world.call(2, 211);
    assert!(matches!(world.answers.last(), Some((_, _, core::CallPart::Unavailable))));
    assert_eq!(world.store.applied, commits);
    assert_eq!(world.spent(), 3);
    assert!(!world.store.rows.contains_key(&root::Key::Core(core::Key::Core(core::CoreKey::Call(world.call_key(2))))));
    world.release_writes();
    world.call(2, 212);
    assert!(matches!(
        world.answers.last(),
        Some((_, _, core::CallPart::Effect { outcome: Some(core::connector::OutboxOutcome::Failed), .. }))
    ));
    assert_eq!(world.spent(), 6);
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}
