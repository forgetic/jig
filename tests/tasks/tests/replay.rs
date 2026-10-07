#[test]
fn seed_replays_the_full_frozen_world_and_fact_consumption_changes_no_behavior() {
    for seed in [23, 24] {
        assert_eq!(jig_tasks_world::run_story(seed), jig_tasks_world::run_story(seed));
    }
    let (trace, consumed) = jig_tasks_world::run_story_facts(23, true);
    let (other, retained) = jig_tasks_world::run_story_facts(23, false);
    assert_eq!(trace, other);
    assert_eq!(consumed.records, retained.records);
    assert_eq!(consumed.replies, retained.replies);
    assert_eq!(consumed.contexts, retained.contexts);
    assert_eq!(consumed.activations, retained.activations);
    assert_eq!(consumed.runs, retained.runs);
    assert_eq!(consumed.stops, retained.stops);
    assert_eq!(consumed.closing, retained.closing);
    assert_eq!(consumed.results, retained.results);
}
