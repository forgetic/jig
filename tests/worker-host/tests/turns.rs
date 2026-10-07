//! Version-two worker host peers: engine commits, a lossy link, and an
//! agent and workspace that settle through the host's requests.

use jig_worker_host::{EndingV2, Failure, Reason};
use jig_worker_host_world::turn_world::World;

#[test]
fn committed_turns_replay_after_busy_and_reconnect_then_restore_credit() {
    let mut world = World::new();
    world.assign(true);
    world.deliver();
    world.relay();
    world.turn(1, b"one");
    world.retry_busy(1);
    world.lose_contact();
    world.turn(2, b"two");
    assert!(!world.reading(), "the full window pauses its agent");
    world.reconnect();
    world.commit_turn(1);
    assert!(world.reading(), "the exact commit returns one read credit");
    world.commit_turn(2);
    world.finish(2, true);
    world.acknowledge_answer();
    world.assert_settled();
    let stats = world.stats();
    assert!(stats.transmissions >= 4, "busy and hello resent the original turns: {stats:?}");
    assert_eq!((stats.commits, stats.turn_acks, stats.deliveries, stats.relays, stats.replies), (2, 2, 1, 1, 2));
    assert_eq!((stats.saves, stats.releases), (1, 1));
}

#[test]
fn an_answer_made_without_contact_follows_retained_turns_after_hello() {
    let mut world = World::new();
    world.assign(false);
    world.lose_contact();
    world.turn(1, b"kept during outage");
    world.finish(1, false);
    world.reconnect();
    let Some(answer) = world.answer() else { panic!("the host answered") };
    assert!(matches!(answer.ending, EndingV2::Ended { .. }));
    world.commit_turn(1);
    world.acknowledge_answer();
    world.assert_settled();
    assert_eq!((world.stats().saves, world.stats().releases), (0, 0));
}

#[test]
fn contact_cancel_waits_for_the_agent_then_saves_and_answers() {
    let mut world = World::new();
    world.assign(true);
    world.turn(1, b"kept before cancel");
    world.hold_stop();
    world.lose_contact();
    world.cancel_contact();
    assert!(world.answer().is_none(), "a live agent blocks workspace save and answer");
    world.gone();
    let Some(answer) = world.answer() else { panic!("the cancelled run answers after its agent is gone") };
    assert!(matches!(&answer.ending, EndingV2::Failed { failure: Failure::Cancelled(Reason::Contact), .. }));
    world.reconnect();
    world.commit_turn(1);
    world.acknowledge_answer();
    world.assert_settled();
    assert_eq!((world.stats().saves, world.stats().releases), (1, 1));
}

#[test]
fn shutdown_and_best_effort_facts_leave_no_live_run() {
    let mut world = World::new();
    world.assign(true);
    world.lose_contact();
    world.fact(b"one");
    world.fact(b"two");
    world.fact(b"three");
    assert_eq!(world.lost_facts(), 1, "the full fact queue drops and counts");
    world.shutdown();
    world.reconnect();
    world.acknowledge_answer();
    world.assert_settled();
    assert_eq!(world.stats().facts, 2);
}
