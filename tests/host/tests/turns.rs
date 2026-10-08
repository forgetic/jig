//! Version-two worker host peers: engine commits, a lossy link, and an
//! agent and workspace that settle through the host's requests.

use jig_host::{AnsweredCall, DeliveryOutcome, Ending, Failure, Reason, SettledAnswer};
use jig_host_world::turn_world::World;
use skein_lib::{Duration, Token};

#[test]
fn a_host_call_reaches_the_engine_with_its_tool_write_flag_and_input() {
    let mut world = World::new();
    world.assign_conversation(Box::new([]), Box::new([]));
    world.call(true);
    let Some(call) = world.relayed_call() else { panic!("the engine received the call") };
    assert_eq!(&*call.name, b"call-one");
    assert_eq!(&*call.tool, b"inspect");
    assert!(call.writes);
    assert_eq!(&*call.input, b"input words");
    assert_eq!(call.deadline, Duration::from_nanos(37));
    assert_eq!(world.answered_call(), Some(&b"call-one"[..]));
}

#[test]
fn a_message_reaches_the_agent_with_its_label_and_words() {
    let mut world = World::new();
    world.assign_conversation(Box::new([]), Box::new([]));
    world.message_from();
    let Some(message) = world.received_message() else { panic!("the agent received the message") };
    assert_eq!(message.name, Token::new(17));
    assert_eq!(&*message.sender, b"requester");
    assert_eq!(&*message.words, b"please check");
}

#[test]
fn a_resumed_run_starts_with_its_turn_bodies_and_answered_calls() {
    let mut world = World::new();
    let turns: Box<[Box<[u8]>]> = vec![Box::from(&b"first"[..]), Box::from(&b"second"[..])].into_boxed_slice();
    let answered = vec![
        AnsweredCall {
            name: Box::from(&b"call one"[..]),
            tool: Box::from(&b"inspect"[..]),
            answer: SettledAnswer::Host { error: false, body: Box::from(&b"found"[..]) },
        },
        AnsweredCall {
            name: Box::from(&b"call two"[..]),
            tool: Box::from(&b"deliver"[..]),
            answer: SettledAnswer::Delivery {
                outcome: DeliveryOutcome::Delivered,
                evidence: Box::from(&b"receipt for workspace item"[..]),
            },
        },
    ];
    world.assign_conversation(turns, answered.into_boxed_slice());
    let Some(start) = world.started_state() else { panic!("the agent started") };
    assert_eq!(start.activation, 932);
    assert_eq!(&*start.turns, &[Box::from(&b"first"[..]), Box::from(&b"second"[..])]);
    assert_eq!(&start.answered[0].name[..], b"call one");
    assert_eq!(&start.answered[0].tool[..], b"inspect");
    assert_eq!(start.answered[0].answer, SettledAnswer::Host { error: false, body: Box::from(&b"found"[..]) });
    assert_eq!(
        start.answered[1].answer,
        SettledAnswer::Delivery {
            outcome: DeliveryOutcome::Delivered,
            evidence: Box::from(&b"receipt for workspace item"[..]),
        }
    );
}

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
    assert!(world.reading(), "the hub ACK leaves room in the agent window");
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
    assert!(matches!(answer.ending, Ending::Ended { .. }));
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
    assert!(matches!(&answer.ending, Ending::Failed { failure: Failure::Cancelled(Reason::Contact), .. }));
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
