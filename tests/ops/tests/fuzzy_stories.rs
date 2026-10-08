use jig_conformance::Cut;
use jig_ops_fake_production as production;
use jig_ops_world::conformance::night;

#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

#[test]
fn every_night_commit_recovers_one_conditional_scale() {
    // Sweep the one procedure-only story, with one pinned seed. The alert's
    // selected cuts stay focused; other ops scenarios belong to later.md.
    let baseline = night(17, Cut::None);
    for commit in 1..=baseline.outcome().commits {
        let world = night(17, Cut::AfterCommit(commit));
        assert_eq!(world.outcome().crashes, vec![commit], "cut {commit}");
        assert!(!world.outcome().stopped, "cut {commit}: {}", world.trace.join("\n"));
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
