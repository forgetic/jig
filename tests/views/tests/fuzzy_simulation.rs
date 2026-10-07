use jig_core_views::{Event, Request, Subject};
use jig_views_world::World;
use skein_lib::Token;

#[test]
fn random_live_streams_keep_one_delivery_per_watcher() {
    let mut deliveries = 0_u64;
    for seed in 1..=64_u64 {
        let mut world = World::default();
        world.start(7, 1);
        for watcher in 0..4_u64 {
            world.watch(watcher, Subject::Tree { task: Token::new(7) });
        }
        let mut state = seed;
        for turn in 1..=80_u32 {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            world.step(Event::Turn { task: Token::new(7), attempt: Token::new(1), number: turn });
            for watcher in 0..4_u64 {
                if state.rotate_left(u32::try_from(watcher).expect("small")).is_multiple_of(3) {
                    let output = world.delivered(watcher, state.is_multiple_of(5));
                    deliveries +=
                        u64::try_from(output.iter().filter(|item| matches!(item, Request::Deliver { .. })).count())
                            .expect("small");
                }
            }
        }
        for watcher in 0..4_u64 {
            world.step(Event::Unwatch { watcher: Token::new(watcher) });
            world.delivered(watcher, true);
        }
        assert_eq!(world.referee.live(), 0, "seed {seed}");
    }
    assert!(deliveries > 1000, "the random worlds exercised delivery turnover");
}
