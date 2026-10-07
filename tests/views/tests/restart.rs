use jig_core_views::{Event, Refusal, Request, Subject};
use jig_views_world::World;
use skein_lib::Token;

#[test]
fn a_restart_drops_every_live_watch_and_starts_from_a_new_snapshot() {
    let mut before = World::default();
    before.start(7, 1);
    before.watch(3, Subject::Run { task: Token::new(7), attempt: Token::new(1) });
    assert_eq!(before.referee.live(), 1);

    let mut after = World::default();
    assert!(matches!(
        after.watch(4, Subject::Run { task: Token::new(7), attempt: Token::new(1) }).as_slice(),
        [Request::Refused { refusal: Refusal::Unknown, .. }]
    ));
    after.start(7, 2);
    assert!(matches!(
        after.watch(3, Subject::Run { task: Token::new(7), attempt: Token::new(2) }).as_slice(),
        [Request::Watching { .. }, Request::Deliver { .. }]
    ));
    assert!(after.step(Event::Turn { task: Token::new(7), attempt: Token::new(1), number: 1 }).is_empty());
}
