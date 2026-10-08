use jig_core_world::world::World;

#[test]
fn a_chats_run_answers_once_and_its_turn_is_acknowledged_after_its_commit() {
    let mut world = World::new(41);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| world.run_chat()));
    assert!(result.is_ok(), "{:?}", world.trace);
    assert_eq!(world.turn_acks, 1);
    assert_eq!(world.answer_acks, 1);
    assert_eq!(world.results, 1);
}

#[test]
fn a_person_sets_a_goal_a_procedure_carries_out_and_hears_its_result() {
    let mut world = World::goal(42);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| world.run_goal()));
    assert!(result.is_ok(), "{:?}", world.trace);
    assert_eq!(world.results, 1);
    assert!(world.procedure.is_some());
}

#[test]
fn a_batch_beyond_the_limits_is_refused_whole_and_the_party_told_why() {
    let mut world = World::batch(43);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| world.run_batch()));
    assert!(result.is_ok(), "{:?}", world.trace);
    assert_eq!(world.batch_refusals, 1);
    assert!(world.procedure.is_none(), "no member of the refused batch received an ID");
    assert!(
        !world.store.rows.values().any(|row| matches!(row,
            jig_test_domain::Record::Core(jig_core::Record::Tasks(jig_core_tasks::Stored::Live(task)))
                if task.requester == jig_core_tasks::Party::Task(world.chat.expect("parent goal"))
        )),
        "the batch left no child row"
    );
}
