use jig_core as core;
use jig_core_tasks as tasks;
use jig_core_world::effects::World;
use jig_fake_store::Fault;
use jig_test_domain as root;
use skein_lib::{ReplyTo, Token};

#[test]
fn a_held_effect_commit_holds_the_system_and_the_call_answer() {
    let mut world = World::new(121, false);
    let answers = world.answers.len();
    world.store.fault(Fault::Hold { commits: 1 });
    world.call(1, 800);
    assert!(!world.store.held.is_empty());
    assert!(world.systems[0].observed().is_empty());
    assert_eq!(world.answers.len(), answers);
    world.release_commits();
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
    assert_eq!(world.answers.len(), answers + 1);
}

#[test]
fn a_failed_commit_stops_the_root_before_its_effect_or_answer_leaves() {
    let mut world = World::new(122, false);
    let durable = world.store.applied;
    let rows = world.store.rows.clone();
    let answers = world.answers.len();
    world.store.fault(Fault::Fail { commit: durable + 1 });
    world.call(1, 801);
    assert!(world.stopped);
    assert_eq!(world.store.applied, durable);
    assert_eq!(world.store.rows, rows);
    assert!(world.systems[0].observed().is_empty());
    assert_eq!(world.answers.len(), answers);
    world.restart(122, false);
    assert!(world.domain.ready());
    assert!(!world.stopped);
    assert!(world.systems[0].observed().is_empty());
}

#[test]
fn slow_pages_delay_a_cold_start_without_skipping_the_live_rows() {
    let mut world = World::new(123, false);
    world.turn(1, 3, None, b"kept before slow restart");
    world.call(1, 802);
    let durable = world.store.applied;
    let started = world.wall_time();
    world.store.fault(Fault::SlowPages { by: 3 });
    world.restart(123, false);
    assert!(world.domain.ready());
    assert!(world.wall_time().as_nanos() > started.as_nanos() + 10_000_000);
    assert!(world.store.applied >= durable);
    assert!(world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Core(core::CoreRecord::Turn(turn))) if turn.transcript.as_ref() == b"kept before slow restart"
    )));
    assert!(world.trace.iter().any(|line| line.starts_with("store page LoadCore")));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}

#[test]
fn a_crash_before_a_turn_is_durable_loses_only_its_uncommitted_turn() {
    let mut world = World::new(124, false);
    let assignment = world.assigned.as_ref().expect("assigned caller");
    let task = assignment.task;
    let attempt = assignment.attempt;
    world.store_delay = 500;
    // One iteration is driven directly so the crash precedes storage application.
    let env = skein_lib::Env {
        now: skein_lib::Time::from_nanos(world.wall_time().as_nanos()),
        wall: skein_lib::Wall::from_nanos(world.wall_time().as_nanos()),
        limits: jig_core_world::world::limits(),
    };
    root::step(
        &mut world.domain,
        &env,
        root::Event::Turn {
            channel: Token::new(7),
            task,
            attempt,
            turn: 1,
            cumulative: 3,
            read: None,
            transcript: b"not durable".as_slice().into(),
        },
    );
    let mut out = skein_lib::Queue::with_capacity(128);
    root::release(&mut world.domain, &env, &mut out);
    while let Some(request) = out.pop() {
        match request {
            root::Request::Commit { number, mut writes } => {
                let mut rows = Vec::new();
                while let Some(row) = writes.pop() {
                    rows.push(row);
                }
                world.store.submit(number, jig_core_world::store_writes(rows), 500);
            }
            other @ (root::Request::Restart(_)
            | root::Request::Deliver(_)
            | root::Request::Now(_)
            | root::Request::Stop) => panic!("nothing leaves before the turn commit: {other:?}"),
        }
    }
    assert!(!world.store.pending.is_empty());
    world.store_delay = 0;
    world.restart(124, false);
    assert!(world.store.pending.is_empty());
    assert!(!world.store.rows.contains_key(&root::Key::Core(core::Key::Core(core::CoreKey::Turn {
        task,
        attempt,
        turn: 1
    }))));
    assert!(world.domain.ready());
}

#[test]
fn a_crash_after_durability_before_acknowledgement_restores_the_turn() {
    let mut world = World::new(125, false);
    world.store.fault(Fault::Hold { commits: 1 });
    world.turn(1, 3, None, b"durable without acknowledgement");
    assert!(!world.store.held.is_empty());
    let durable = world.store.applied;
    world.restart(125, false);
    assert!(world.store.held.is_empty());
    assert!(world.store.applied >= durable);
    assert!(world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Core(core::CoreRecord::Turn(turn))) if turn.transcript.as_ref() == b"durable without acknowledgement"
    )));
}

#[test]
fn parties_run_ahead_of_held_completions_but_every_answer_waits() {
    let mut world = World::new(126, false);
    let task = world.call_key(1).task;
    let answers = world.people_answers.len();
    world.store.fault(Fault::Hold { commits: 1 });
    for request in [70, 71] {
        world.send(root::Event::Core(core::Event::People(jig_core_people::Event::Ask {
            reply_to: ReplyTo::new(Token::new(u64::from(request))),
            sign_in: world.sign_in.expect("signed in"),
            key: [request; 16],
            ask: jig_core_people::Ask::Say { project: 1, task, words: Box::from([request]) },
        })));
    }
    assert!(world.store.held.len() >= 2);
    assert_eq!(world.people_answers.len(), answers);
    assert!(
        !world
            .store
            .rows
            .values()
            .any(|row| matches!(row, root::Record::Core(core::Record::Tasks(tasks::Stored::Ended(_)))))
    );
    world.release_commits();
    assert_eq!(world.people_answers.len(), answers + 2);
}

#[test]
fn a_runs_read_is_answered_while_an_unrelated_commit_is_in_flight() {
    let mut world = World::new(128, false);
    world.store.fault(Fault::Hold { commits: 1 });
    world.turn(1, 1, None, b"unrelated turn");
    let held = world.store.held.clone();
    assert!(!held.is_empty());
    let commits = world.commits.len();
    let right = world.host_call(b"read-now", b"read", b"{}", false);
    let answer = core::SettledCall {
        serial: 0,
        name: b"read-now".as_slice().into(),
        tool: b"read".as_slice().into(),
        answer: core::SettledAnswer::Host { error: false, body: b"fresh result".as_slice().into() },
    };
    world.send(root::Event::ReadAnswer { to: ReplyTo::new(right), call: answer.clone() });
    assert_eq!(world.store.held, held, "the unrelated commit still waits");
    assert_eq!(world.commits.len(), commits, "a read commits nothing");
    assert_eq!(world.host_answers, [(b"read-now".as_slice().into(), answer)]);
    world.release_commits();
    assert_eq!(world.host_answers.len(), 1, "the read is answered once");
}

#[test]
fn a_runs_write_answer_waits_for_an_unrelated_commit_in_flight() {
    let mut world = World::new(129, false);
    world.store.fault(Fault::Hold { commits: 1 });
    world.turn(1, 1, None, b"unrelated turn");
    let right = world.host_call(b"write-held", b"write", b"{}", true);
    let key = world.call_key(2);
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
            name: b"write-held".as_slice().into(),
            tool: b"write".as_slice().into(),
            answer: core::SettledAnswer::Host { error: false, body: b"done".as_slice().into() },
        },
    }));
    assert!(world.host_answers.is_empty(), "writes wait behind the earlier commit");
    world.release_commits();
    assert_eq!(world.host_answers.len(), 1);
}
