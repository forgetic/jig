use jig_conformance::Cut;
use jig_conformance_world::actions;
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

fn story(cut: u64) -> actions::World {
    let mut world = actions::world(23, false);
    world.cut(Cut::AfterCommit(cut));
    world.drain().expect("cold startup conforms");
    if world.peers.assignments.is_empty() {
        world.advance(6_000_000_000);
        world.drain().expect("unseen claim passes its adoption grace");
        world.advance(1_000_000_000);
        world.drain().expect("replacement passes its retry backoff");
    }
    assert!(
        !world.peers.assignments.is_empty(),
        "cut {cut}, final trace {:?}",
        &world.trace[world.trace.len().saturating_sub(15)..]
    );
    actions::effect(&mut world, 1);
    world.drain().expect("effect after cut conforms");
    if !world.systems[0].observed().iter().any(|effect| effect.applied) {
        world.advance(2_000_000_000);
        world.drain().expect("effect retry deadline passes");
    }
    world
}

#[test]
fn cuts_around_the_party_answer_claim_and_effect_restart_from_durable_rows() {
    // Two surrounds the party's answer, four the first claim, five through
    // seven the effect admission, attempted write and its settled answer.
    for cut in [2, 4, 5, 6, 7] {
        let world = story(cut);
        assert_eq!(world.outcome().crashes, vec![cut], "chosen cut {cut}");
        assert_eq!(world.systems[0].observed().iter().filter(|effect| effect.applied).count(), 1);
        assert!(!world.outcome().stopped);
    }
}
