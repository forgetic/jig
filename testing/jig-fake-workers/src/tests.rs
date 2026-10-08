use super::*;

fn assigned() -> Assignment {
    Assignment { task: 1, attempt: 1, budget: 10, transcript: Box::new([]) }
}

#[test]
fn hello_precedes_retained_turns_and_answers_after_every_reconnection() {
    let mut worker = Worker::new(
        7,
        1,
        vec![Box::new([
            Script::Turn { body: Box::from(&b"turn"[..]), cost: 2, read: None },
            Script::Finish { report: Box::from(&b"done"[..]) },
        ])],
    );
    assert!(matches!(worker.tick(Time::ZERO).as_slice(), [Up::Hello { .. }]));
    worker.assign(assigned());
    let turn = worker.tick(Time::ZERO);
    let answer = worker.tick(Time::ZERO);
    assert!(matches!(answer.as_slice(), [Up::Answer { .. }]));
    assert_eq!(
        worker.fault(Fault::DropChannel { for_: Duration::from_millis(1) }, Time::ZERO),
        [Up::Lost { channel: 7 }]
    );
    let replay = worker.tick(Time::from_nanos(1_000_000));
    assert!(matches!(replay.first(), Some(Up::Hello { .. })));
    assert_eq!(replay[1], turn[0]);
    assert_eq!(replay[2], answer[0]);
    worker.acknowledge_turn(1, 1, 1);
    worker.acknowledge_answer(1, 1);
    assert!(worker.quiescent());
    assert_eq!(worker.turn_acks, 1);
    assert_eq!(worker.answer_acks, 1);
}

#[test]
fn a_wait_keeps_its_slot_and_a_message_resumes_the_script() {
    let mut worker = Worker::new(0, 1, vec![Box::new([Script::Wait, Script::Park])]);
    worker.assign(assigned());
    assert!(worker.tick(Time::ZERO).is_empty());
    assert!(worker.tick(Time::ZERO).is_empty());
    worker.message(1, 1, 3, Box::from(&b"continue"[..]));
    assert!(matches!(worker.tick(Time::ZERO).as_slice(), [Up::Answer { end: End::Parked, .. }]));
    assert!(!worker.quiescent());
    worker.acknowledge_answer(1, 1);
    assert!(worker.quiescent());
}

#[test]
fn a_call_blocks_script_progress_until_one_answer_reaches_it() {
    let call = Call::Opaque {
        name: Box::from(&b"one"[..]),
        tool: Box::from(&b"echo"[..]),
        input: Box::from(&b"input"[..]),
        writes: false,
    };
    let mut worker = Worker::new(0, 1, vec![Box::new([Script::Call(call.clone()), Script::Crash])]);
    worker.assign(assigned());
    assert!(matches!(worker.tick(Time::ZERO).as_slice(), [Up::Call { call: found, .. }] if *found == call));
    assert!(worker.tick(Time::ZERO).is_empty());
    worker.call_answer(1, 1, 1, Box::from(&b"answer"[..]));
    assert!(matches!(worker.tick(Time::ZERO).as_slice(), [Up::Answer { end: End::Failed(Failure::Agent), .. }]));
}

#[test]
fn a_link_back_after_the_stop_bound_lists_a_retained_parked_answer() {
    let mut worker = Worker::new(7, 1, vec![Box::new([Script::Wait])]);
    drop(worker.tick(Time::ZERO));
    worker.assign(assigned());
    drop(worker.fault(Fault::DropChannel { for_: Duration::from_secs(1) }, Time::ZERO));
    let reports = worker.tick(Time::from_nanos(1_000_000_000));
    assert!(matches!(reports.as_slice(), [Up::Hello { .. }, Up::Answer { end: End::Parked, .. }]));
}

#[test]
fn silence_and_slowness_preserve_the_live_run_but_vanishing_loses_its_flights() {
    let mut worker = Worker::new(
        7,
        1,
        vec![Box::new([
            Script::Silence { for_: Duration::from_millis(2) },
            Script::Turn { body: Box::from(&b"turn"[..]), cost: 2, read: None },
        ])],
    );
    drop(worker.tick(Time::ZERO));
    worker.assign(assigned());
    assert!(worker.tick(Time::ZERO).is_empty());
    assert!(worker.tick(Time::from_nanos(1_000_000)).is_empty());
    drop(worker.fault(Fault::Slow { by: Duration::from_millis(3) }, Time::ZERO));
    assert!(worker.tick(Time::from_nanos(2_000_000)).is_empty());
    assert!(matches!(worker.tick(Time::from_nanos(3_000_000)).as_slice(), [Up::Turn { .. }]));
    drop(worker.fault(Fault::Vanish, Time::from_nanos(3_000_000)));
    assert!(worker.quiescent());
    assert_eq!(worker.lost_spent, 2);
    assert!(worker.tick(Time::from_nanos(4_000_000)).is_empty());
}

#[test]
fn a_vanished_workers_lost_spend_excludes_acknowledged_turns() {
    let mut worker = Worker::new(
        7,
        1,
        vec![Box::new([
            Script::Turn { body: Box::from(&b"kept"[..]), cost: 2, read: None },
            Script::Turn { body: Box::from(&b"lost"[..]), cost: 3, read: None },
        ])],
    );
    drop(worker.tick(Time::ZERO));
    worker.assign(assigned());
    drop(worker.tick(Time::ZERO));
    worker.acknowledge_turn(1, 1, 1);
    drop(worker.tick(Time::ZERO));
    drop(worker.fault(Fault::Vanish, Time::ZERO));
    assert_eq!(worker.actual_spent, 5);
    assert_eq!(worker.lost_spent, 3);
}
