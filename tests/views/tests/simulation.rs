use jig_core_views::{Chunk, End, Event, Kind, Refusal, Request, Subject};
use jig_views_world::World;
use skein_lib::Token;

#[test]
fn a_watch_begins_with_its_snapshot_and_streams_a_committed_turn() {
    let mut world = World::default();
    world.start(7, 1);
    let out = world.watch(3, Subject::Run { task: Token::new(7), attempt: Token::new(1) });
    assert!(matches!(out.as_slice(), [Request::Watching { .. }, Request::Deliver { chunks, .. }]
        if matches!(chunks.as_ref(), [Chunk::Snapshot { content, .. }] if content.as_ref() == b"snapshot")));
    world.delivered(3, true);
    let out = world.step(Event::Turn { task: Token::new(7), attempt: Token::new(1), number: 2 });
    assert!(matches!(out.as_slice(), [Request::Deliver { missed: 0, chunks, .. }]
        if matches!(chunks.as_ref(), [Chunk::Report { kind: Kind::Progress, content, .. }]
            if content.as_ref() == 2_u32.to_be_bytes())));
    world.delivered(3, true);
    assert!(matches!(
        world.step(Event::Finished { task: Token::new(7) }).as_slice(),
        [Request::Ended { end: End::Finished, .. }]
    ));
    assert_eq!(world.referee.live(), 0);
}

#[test]
fn a_slow_watcher_is_told_what_it_missed_and_catches_up() {
    let mut world = World::default();
    world.start(7, 1);
    world.watch(3, Subject::Run { task: Token::new(7), attempt: Token::new(1) });
    for number in 1..=4_u32 {
        world.step(Event::Turn { task: Token::new(7), attempt: Token::new(1), number });
    }
    let out = world.delivered(3, true);
    assert!(matches!(out.as_slice(), [Request::Deliver { missed: 2, chunks, .. }]
        if matches!(chunks.as_ref(), [Chunk::Report { .. }, Chunk::Report { content, .. }]
            if content.as_ref() == 4_u32.to_be_bytes())));
    assert_eq!(world.referee.missed, 2);
}

#[test]
fn a_delivery_the_stream_did_not_take_is_told_as_missed() {
    let mut world = World::default();
    world.start(7, 1);
    world.watch(3, Subject::Run { task: Token::new(7), attempt: Token::new(1) });
    world.step(Event::Turn { task: Token::new(7), attempt: Token::new(1), number: 1 });
    let out = world.delivered(3, false);
    assert!(matches!(out.as_slice(), [Request::Deliver { missed: 1, .. }]));
}

#[test]
fn watches_past_limits_or_of_runs_not_followed_are_refused() {
    let mut world = World::default();
    assert!(matches!(
        world.watch(1, Subject::Run { task: Token::new(7), attempt: Token::new(1) }).as_slice(),
        [Request::Refused { refusal: Refusal::Unknown, .. }]
    ));
    world.start(1, 1);
    world.start(2, 1);
    world.start(3, 1);
    world.start(4, 1);
    assert!(matches!(
        world.watch(2, Subject::Run { task: Token::new(4), attempt: Token::new(1) }).as_slice(),
        [Request::Refused { refusal: Refusal::Unfollowed, .. }]
    ));
    for watcher in 3..7 {
        world.watch(watcher, Subject::Goals { project: 1 });
    }
    assert!(matches!(
        world.watch(7, Subject::Goals { project: 1 }).as_slice(),
        [Request::Refused { refusal: Refusal::Busy, .. }]
    ));
}

#[test]
fn task_trees_goals_and_inboxes_are_separate() {
    let mut world = World::default();
    world.watch(1, Subject::Tree { task: Token::new(7) });
    world.watch(2, Subject::Goals { project: 2 });
    world.watch(3, Subject::Inbox { party: 9 });
    for watcher in 1..=3 {
        world.delivered(watcher, true);
    }
    let phase = world.step(Event::TaskPhase {
        task: Token::new(8),
        trees: Box::new([Token::new(8), Token::new(7)]),
        project: 2,
        phase: 3,
        priority: Some(5),
    });
    assert_eq!(phase.len(), 2);
    assert!(phase.iter().any(|request| matches!(request,
        Request::Deliver { watcher, chunks, .. } if *watcher == Token::new(1)
            && matches!(chunks.as_ref(), [Chunk::Phase { task, phase: 3, .. }] if *task == Token::new(8)))));
    assert!(phase.iter().any(|request| matches!(request,
        Request::Deliver { watcher, .. } if *watcher == Token::new(2))));
    let inbox = world.step(Event::Inbox { party: 9, content: b"new".as_slice().into() });
    assert!(matches!(inbox.as_slice(), [Request::Deliver { watcher, chunks, .. }]
        if *watcher == Token::new(3) && matches!(chunks.as_ref(), [Chunk::Inbox { content, .. }] if content.as_ref() == b"new")));
}

#[test]
fn ended_watch_names_can_be_reused_after_reclaim() {
    let mut world = World::default();
    world.watch(1, Subject::Goals { project: 2 });
    world.delivered(1, true);
    assert!(matches!(
        world.step(Event::Unwatch { watcher: Token::new(1) }).as_slice(),
        [Request::Ended { end: End::Unwatched, .. }]
    ));
    world.reclaim();
    assert!(matches!(
        world.watch(1, Subject::Goals { project: 2 }).as_slice(),
        [Request::Watching { .. }, Request::Deliver { .. }]
    ));
}

#[test]
fn stale_attempts_and_oversized_reports_do_not_reach_watchers() {
    let mut world = World::default();
    world.start(7, 1);
    world.watch(1, Subject::Tree { task: Token::new(7) });
    world.delivered(1, true);
    world.start(7, 2);
    assert!(
        world
            .step(Event::Reported {
                task: Token::new(7),
                attempt: Token::new(1),
                kind: Kind::Text,
                content: b"stale".as_slice().into()
            })
            .is_empty()
    );
    assert!(
        world
            .step(Event::Reported {
                task: Token::new(7),
                attempt: Token::new(2),
                kind: Kind::Text,
                content: vec![0; 33].into_boxed_slice()
            })
            .is_empty()
    );
    assert_eq!(world.domain.lost().reports, 2);
}

#[test]
fn the_same_script_replays_and_facts_change_nothing() {
    fn run() -> Vec<String> {
        let mut world = World::default();
        world.start(7, 1);
        world.watch(1, Subject::Tree { task: Token::new(7) });
        world.delivered(1, true);
        world.step(Event::Turn { task: Token::new(7), attempt: Token::new(1), number: 1 });
        world.log
    }
    assert_eq!(run(), run());
    let mut a = World::default();
    let mut b = World::default();
    a.start(7, 1);
    b.start(7, 1);
    while a.domain.pop_fact().is_some() {}
    let event = Event::TaskPhase {
        task: Token::new(7),
        trees: Box::new([Token::new(7)]),
        project: 1,
        phase: 2,
        priority: None,
    };
    let output_a = a.step(event);
    let output_b = b.step(Event::TaskPhase {
        task: Token::new(7),
        trees: Box::new([Token::new(7)]),
        project: 1,
        phase: 2,
        priority: None,
    });
    assert_eq!(output_a, output_b);
}
