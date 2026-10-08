use jig_conformance::Cut;
use jig_ops_fake_production as production;
use jig_ops_world::conformance::night;

#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn the_night_scales_staging_under_an_observed_requirement_without_an_agent() {
    let world = night(17, Cut::None);
    assert!(world.peers.assignments.is_empty());
    assert_eq!(
        world
            .systems
            .production
            .observed()
            .iter()
            .filter(|effect| matches!(effect, production::ObservedEffect::Scale { applied: true, from: 3, to: 1, .. }))
            .count(),
        1,
        "{}",
        world.trace.join("\n")
    );
}

#[test]
fn the_nights_scale_entry_survives_the_commits_around_its_admission() {
    let baseline = night(17, Cut::None);
    let entry = baseline
        .peers
        .entries
        .iter()
        .filter_map(|(commit, kind)| (*kind == 2).then_some(*commit))
        .min()
        .expect("scale entry");
    for commit in [entry - 1, entry, entry + 1] {
        let world = night(17, Cut::AfterCommit(commit));
        assert_eq!(world.outcome().crashes, vec![commit]);
        assert!(world.peers.assignments.is_empty());
        assert_eq!(
            world
                .systems
                .production
                .observed()
                .iter()
                .filter(|effect| matches!(
                    effect,
                    production::ObservedEffect::Scale { applied: true, from: 3, to: 1, .. }
                ))
                .count(),
            1,
            "cut {commit}: {}",
            world.trace.join("\n")
        );
    }
}

#[test]
fn the_alert_storm_reaches_triage_and_the_person_accepts_the_production_restart() {
    let world = jig_ops_world::conformance::alert(17, Cut::None);
    assert_alert(&world);
    let acceptance = world.peers.accepted[0];
    let entry = world
        .peers
        .entries
        .iter()
        .filter_map(|(commit, kind)| (*kind == 1).then_some(*commit))
        .min()
        .expect("restart entry");
    for commit in [acceptance, entry, entry + 1] {
        let recovered = jig_ops_world::conformance::alert(17, Cut::AfterCommit(commit));
        assert_eq!(recovered.outcome().crashes, vec![commit]);
        assert_alert(&recovered);
    }
}

fn assert_alert(world: &jig_conformance::Harness<jig_ops_world::conformance::Ops>) {
    assert!(!world.outcome().stopped, "crashes {:?}: {}", world.outcome().crashes, world.trace.join("\n"));
    assert!(world.peers.results.contains(&5), "on-call read the incident report");
    let ended = |number| {
        world.store.rows.get(&jig_ops_domain::Key::Core(jig_core::Key::Tasks(jig_core_tasks::Key::Ended(number))))
    };
    for task in [3, 4, 5, 6] {
        assert!(ended(task).is_some(), "task {task} missing: {}", world.trace.join("\n"));
    }
    assert_eq!(world.peers.assignments.iter().filter(|(task, _)| *task == 3).count(), 1, "one triage for the storm");
    assert_eq!(
        world
            .systems
            .production
            .observed()
            .iter()
            .filter(|effect| matches!(effect, production::ObservedEffect::Restart { applied: true, operation: 77, .. }))
            .count(),
        1,
        "{}",
        world.trace.join("\n")
    );
}
