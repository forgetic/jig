use jig_core as core;
use jig_core_tasks as tasks;
use jig_core_world::effects::World;
use jig_test_domain as root;
use skein_lib::{ReplyTo, Token};

#[test]
fn a_chat_answers_parks_and_resumes() {
    let mut world = World::new(101, false);
    let first = world.assigned.as_ref().expect("chat assigned").attempt;
    world.turn(1, 3, None, b"first opaque turn");
    world.end(tasks::End::Parked, 3);
    let parked = world.store.applied;
    world.end(tasks::End::Parked, 3);
    assert_eq!(world.store.applied, parked, "duplicate terminal changes nothing");
    let read = world.say(71);
    let second = world.assigned.as_ref().expect("woken chat");
    assert!(second.attempt > first);
    assert_eq!(second.transcript.as_ref(), [Box::<[u8]>::from(&b"first opaque turn"[..])]);
    world.turn(1, 2, Some(read), b"second opaque turn");
    assert!(world.store.rows.values().any(|row| matches!(row, root::Record::Core(core::Record::Core(core::CoreRecord::Turn(turn))) if turn.transcript.as_ref()==b"second opaque turn")), "{:?}", world.trace);
    world.end(tasks::End::Parked, 2);
    world.say(72);
    let third = world.assigned.as_ref().expect("chat resumes across attempts");
    assert!(third.transcript.len() == 2, "{:?}", world.trace);
    assert_eq!(
        third.transcript.as_ref(),
        [Box::<[u8]>::from(&b"first opaque turn"[..]), Box::<[u8]>::from(&b"second opaque turn"[..])]
    );
    let task = third.task;
    assert_eq!(world.spent(), 5, "each attempt's accepted spend is charged once");
    world.end(
        tasks::End::Finished {
            result: tasks::TaskResult::Report { words: b"done".as_slice().into() },
            cancel_delegates: false,
        },
        0,
    );
    assert!(world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(task)))));
}

#[test]
fn a_turn_admits_its_read_fence_and_spend_together_and_duplicates_change_nothing() {
    let mut world = World::new(102, false);
    let message = world.say(73);
    let rows = world.store.rows.clone();
    let commits = world.store.applied;
    world.turn(1, 5, Some(message + 1), b"invalid offered read");
    assert_eq!(world.store.rows, rows, "an unoffered read cannot charge or take words");
    assert_eq!(world.store.applied, commits);
    world.turn(1, 5, Some(message), b"valid turn");
    assert_eq!(world.spent(), 5);
    let rows = world.store.rows.clone();
    let commits = world.store.applied;
    world.turn(1, 5, Some(message), b"valid turn");
    assert_eq!(world.store.rows, rows, "duplicate accepted turn preserves the exact proof and transcript");
    assert_eq!(world.store.applied, commits);
}

#[test]
fn typed_settled_delivery_evidence_survives_restart_and_resume_until_a_turn_carries_it() {
    let mut world = World::new(103, false);
    world.turn(1, 1, None, b"turn");
    let right = world.typed(b"call-name", b"deliver", b"opaque-input", true);
    let key = world.call_key(2);
    assert_eq!(world.typed_calls[0].3.name.as_ref(), b"call-name");
    assert_eq!(world.typed_calls[0].3.tool.as_ref(), b"deliver");
    assert_eq!(world.typed_calls[0].3.input.as_ref(), b"opaque-input");
    assert!(world.typed_calls[0].3.writes);
    world.send(root::Event::Core(core::Event::NamedAnswer {
        to: ReplyTo::new(right),
        key,
        part: core::CallPart::Unavailable,
    }));
    let evidence = Box::<[u8]>::from(&b"\x01opaque delivery evidence\x00"[..]);
    let settled = core::SettledCall {
        serial: 0,
        name: b"call-name".as_slice().into(),
        tool: b"deliver".as_slice().into(),
        answer: core::SettledAnswer::Delivery { outcome: core::DeliveryOutcome::Delivered, evidence: evidence.clone() },
    };
    world.send(root::Event::Core(core::Event::SettledCall { to: ReplyTo::new(right), key, call: settled }));
    assert_eq!(world.typed_answers.len(), 1);
    let saved = world.typed_answers[0].1.clone();
    assert_ne!(saved.serial, 0);
    world.end(tasks::End::Parked, 1);
    world.restart(103, false);
    world.send(root::Event::Core(core::Event::Fleet(jig_core_fleet::Event::Hello {
        channel: Token::new(7),
        hello: jig_core_fleet::Hello {
            stop_bound: skein_lib::Duration::from_millis(100),
            slots: 1,
            workstreams: Box::new([]),
            hosting: Box::new([]),
        },
    })));
    world.send(root::Event::Core(core::Event::Account(jig_core_accounts::Event::Add {
        account: 1,
        generation: 1,
        valid: Some(skein_lib::Duration::from_secs(60)),
    })));
    let read = world.say(74);
    assert!(!world.assigned.as_ref().expect("resumed chat").answered.is_empty(), "{:?}", world.trace);
    assert_eq!(world.assigned.as_ref().expect("resumed chat").answered.as_ref(), [saved]);
    world.turn(1, 1, Some(read), b"answers carried by this turn");
    assert!(!world.store.rows.contains_key(&root::Key::Core(core::Key::Core(core::CoreKey::Call(key)))));
    world.end(tasks::End::Parked, 1);
    world.say(75);
    assert!(world.assigned.as_ref().expect("resumed again").answered.is_empty());
}

#[test]
fn a_chat_past_the_resume_limit_starts_fresh() {
    let mut world = World::new(104, false);
    world.turn(1, 1, None, &[b'a'; 200]);
    world.turn(2, 2, None, &[b'b'; 100]);
    world.end(tasks::End::Parked, 2);
    world.say(76);
    assert!(world.assigned.as_ref().expect("fresh chat").transcript.is_empty());
}

#[test]
fn loaded_transcript_pages_reject_repeated_and_reversed_turns() {
    let limits = jig_core_world::world::limits().core;
    let config = jig_core_world::world::config(105).core;
    let mut core = core::Core::new(config, &limits);
    core.begin_transcript(1, 3);
    let turn = core::TurnRecord {
        task: 1,
        attempt: 2,
        turn: 1,
        spent: 0,
        read: None,
        at: skein_lib::Wall::EPOCH,
        transcript: b"turn".as_slice().into(),
    };
    assert!(core.append_transcript(1, turn.clone()));
    assert!(!core.append_transcript(1, turn));
    assert!(!core.append_transcript(
        1,
        core::TurnRecord {
            task: 1,
            attempt: 1,
            turn: 2,
            spent: 0,
            read: None,
            at: skein_lib::Wall::EPOCH,
            transcript: b"old".as_slice().into()
        }
    ));
    assert_eq!(core.transcripts.get(&1).expect("loaded transcript").bytes, 4);
}

#[test]
fn an_unusable_transcript_fails_transient_and_the_next_conversation_starts_fresh() {
    let mut world = World::new(106, false);
    world.turn(1, 1, None, b"unusable bytes");
    world.end(tasks::End::Parked, 1);
    let read = world.say(77);
    assert_eq!(world.assigned.as_ref().expect("resumed attempt").transcript.len(), 1);
    world.end(tasks::End::Failed(tasks::Class::Transient), 0);
    world.retry_due();
    let fresh = world.assigned.as_ref().expect("fresh retry");
    assert!(!fresh.run.policy.resume);
    assert!(fresh.transcript.is_empty());
    world.turn(1, 1, Some(read), b"fresh conversation");
    world.end(tasks::End::Parked, 1);
    world.say(78);
    assert_eq!(
        world.assigned.as_ref().expect("resumed usable conversation").transcript.as_ref(),
        [Box::<[u8]>::from(&b"fresh conversation"[..])]
    );
}

#[test]
fn settled_answers_resume_in_commit_order_and_a_replay_preserves_exact_bytes() {
    let mut world = World::new(107, false);
    for (completion, name) in [(4, &b"first"[..]), (2, &b"second"[..])] {
        let right = world.typed(name, b"read", b"{}", false);
        let key = world.call_key(completion);
        world.send(root::Event::Core(core::Event::NamedAnswer {
            to: ReplyTo::new(right),
            key,
            part: core::CallPart::Unavailable,
        }));
        world.send(root::Event::Core(core::Event::SettledCall {
            to: ReplyTo::new(right),
            key,
            call: core::SettledCall {
                serial: 0,
                name: name.into(),
                tool: b"read".as_slice().into(),
                answer: core::SettledAnswer::Host { error: true, body: b"exact original".as_slice().into() },
            },
        }));
    }
    let first = world.typed_answers[0].1.clone();
    let key = world.call_key(4);
    let right = world.typed(b"first", b"read", b"{}", false);
    let before = world.store.applied;
    world.send(root::Event::Core(core::Event::SettledCall {
        to: ReplyTo::new(right),
        key,
        call: core::SettledCall {
            serial: 0,
            name: b"first".as_slice().into(),
            tool: b"read".as_slice().into(),
            answer: core::SettledAnswer::Host { error: false, body: b"different rendering".as_slice().into() },
        },
    }));
    assert_eq!(world.typed_answers.last().expect("replayed answer").1, first);
    assert_eq!(world.store.applied, before);
    world.end(tasks::End::Parked, 0);
    world.say(79);
    let answers = &world.assigned.as_ref().expect("resumed answers").answered;
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].name.as_ref(), b"first");
    assert_eq!(answers[1].name.as_ref(), b"second");
    assert!(answers[0].serial < answers[1].serial);
}
