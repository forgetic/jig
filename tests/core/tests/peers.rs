use jig_core as core;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_world::{
    effects::World,
    peers::{Peers, fixture},
};
use jig_fake_parties::{self as parties, Act, Ask, Party, Role, Task};
use jig_fake_workers::{self as workers, Call, Script, Worker};
use jig_test_domain as root;
use skein_lib::{Duration, Time};

fn party(subject: u64, role: Role, first: u64) -> Party {
    Party::new(1, role, first, Box::new([Act::SignIn { provider: 0, subject: subject.to_be_bytes().into() }]))
}

fn scripted(seed: u64, scripts: Vec<Box<[Script]>>, engine: bool) -> World {
    let (config, limits) = fixture(seed, u32::from(engine));
    let workers = if engine {
        vec![
            Worker::new(0, 1, scripts),
            Worker::new(7, 1, Vec::new()),
            Worker::new(8, 1, Vec::new()),
            Worker::new(9, 1, Vec::new()),
        ]
    } else {
        vec![Worker::new(7, 1, scripts)]
    };
    let mut world = World::scripted(
        config,
        limits,
        Peers::new(
            workers,
            vec![party(7, Role::Owner, 10_000), party(8, Role::Maintainer, 20_000), party(9, Role::Member, 30_000)],
        ),
    );
    let parties = &world.peers.as_ref().expect("peers").parties;
    let holdings = parties
        .iter()
        .map(|party| people::Holding {
            person: party.person.expect("party signed in"),
            role: match party.role {
                Role::Owner => people::Role::Owner,
                Role::Maintainer => people::Role::Maintainer,
                Role::Member => people::Role::Member,
                Role::Observer => people::Role::Observer,
            },
        })
        .collect();
    world.send(root::Event::Core(core::Event::People(people::Event::Roles { project: 1, holdings })));
    world
}

fn acts(world: &mut World, party: usize, script: Box<[Act]>) {
    world.peers.as_mut().expect("peers").parties[party].extend(script);
    world.drain();
}

#[test]
fn a_scripted_worker_takes_turns_answers_calls_and_finishes_with_acknowledgements() {
    let mut world = scripted(
        201,
        vec![Box::new([
            Script::Turn { body: Box::from(&b"whole turn"[..]), cost: 2, read: None },
            Script::Call(Call::Opaque {
                name: Box::from(&b"echo-one"[..]),
                tool: Box::from(&b"echo"[..]),
                input: Box::from(&b"hello"[..]),
                writes: false,
            }),
            Script::Finish { report: Box::from(&b"done"[..]) },
        ])],
        false,
    );
    acts(&mut world, 2, Box::new([Act::Request { key: [1; 16], ask: Ask::Chat { words: Box::from(&b"hello"[..]) } }]));
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.quiescent(), "{:?}", world.trace);
    assert_eq!(peers.workers[0].turn_acks, 1);
    assert_eq!(peers.workers[0].answer_acks, 1);
    assert_eq!(peers.workers[0].call_answers.len(), 1);
    assert!(peers.workers[0].call_answers[0].3.windows(5).any(|bytes| bytes == b"hello"));
    assert_eq!(peers.workers[0].actual_spent, 2);
}

#[test]
fn three_workers_and_the_engines_own_slot_use_the_same_host_scripts() {
    let (config, limits) = fixture(202, 1);
    let workers = [0, 7, 8, 9]
        .into_iter()
        .map(|channel| {
            Worker::new(
                channel,
                1,
                vec![Box::new([Script::Wait, Script::Finish { report: Box::from(&b"answer"[..]) }])],
            )
        })
        .collect();
    let mut world = World::scripted(config, limits, Peers::new(workers, vec![party(7, Role::Owner, 10_000)]));
    let requests = (1..=4)
        .map(|key| Act::Request { key: [key; 16], ask: Ask::Chat { words: Box::from(&b"hello"[..]) } })
        .collect();
    acts(&mut world, 0, requests);
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.workers.iter().all(|worker| worker.seen.len() == 1), "all four configured hosts ran work");
    let tasks: Vec<_> = peers.workers.iter().map(|worker| worker.seen[0].task).collect();
    let words = tasks
        .into_iter()
        .enumerate()
        .map(|(index, task)| Act::Request {
            key: [u8::try_from(index).expect("four tasks") + 10; 16],
            ask: Ask::Say { task: Task::Number(task), words: Box::from(&b"finish"[..]) },
        })
        .collect();
    acts(&mut world, 0, words);
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.workers.iter().all(|worker| worker.answer_acks == 1));
    assert!(peers.quiescent(), "{:?}", world.trace);
}

#[test]
fn a_members_chat_proposes_a_goal_it_cannot_fund_and_a_maintainer_accepts_it_as_their_own() {
    let mut world = scripted(
        203,
        vec![
            Box::new([
                Script::Call(Call::ProposeGoal {
                    words: Box::from(&b"fund a larger goal"[..]),
                    budget: 150,
                    priority: 7,
                }),
                Script::Park,
            ]),
            Box::new([Script::Finish { report: Box::from(&b"goal completed"[..]) }]),
        ],
        false,
    );
    acts(
        &mut world,
        2,
        Box::new([Act::Request { key: [3; 16], ask: Ask::Chat { words: Box::from(&b"please propose"[..]) } }]),
    );
    let proposer = world.peers.as_ref().expect("peers").parties[2].last_task.expect("member's chat");
    assert!(world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Tasks(tasks::Stored::Live(task))) if task.number == proposer && task.proposal.is_some()
    )), "{:?}", world.trace);
    acts(&mut world, 1, Box::new([Act::Decide { accept: true }, Act::Twice]));
    let maintainer = world.peers.as_ref().expect("peers").parties[1].person.expect("maintainer");
    assert!(
        world.store.rows.values().any(|row| matches!(row,
            root::Record::Core(core::Record::Tasks(tasks::Stored::Ended(task))) if task.tracked == Some(7)
            && task.requester == tasks::Party::Person(maintainer) && task.numbers.budget == 150
        )),
        "accepted goal belongs to its accepter: {:?}",
        world.trace
    );
}

#[test]
fn a_person_chooses_between_two_reports_and_the_result_reaches_its_requester() {
    let mut world = scripted(
        204,
        vec![Box::new([
            Script::Call(Call::Choose {
                role: 1,
                first: Box::from(&b"first report"[..]),
                second: Box::from(&b"second report"[..]),
            }),
            Script::Wait,
            Script::Finish { report: Box::from(&b"choice received"[..]) },
        ])],
        false,
    );
    acts(
        &mut world,
        2,
        Box::new([Act::Request { key: [4; 16], ask: Ask::Chat { words: Box::from(&b"ask a maintainer"[..]) } }]),
    );
    acts(
        &mut world,
        1,
        Box::new([
            Act::Request { key: [5; 16], ask: Ask::Take { task: Task::Waiting } },
            Act::AnswerWaiting { code: 2, words: Box::from(&b"second report"[..]) },
        ]),
    );
    assert!(world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Tasks(tasks::Stored::Ended(task))) if matches!(&task.phase,
            tasks::Phase::Ended(tasks::Ending::Done(tasks::TaskResult::Verdict { code: 2, words })) if words.as_ref() == b"second report")
    )), "{:?}", world.trace);
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.workers[0].messages.iter().any(|(_, _, _, words)| words.as_ref() == b"second report"));
    assert!(peers.quiescent(), "{:?}", world.trace);
}

#[test]
fn a_person_stops_a_scripted_run_and_releases_it_for_a_new_attempt() {
    let mut world = scripted(
        205,
        vec![Box::new([Script::Wait]), Box::new([Script::Finish { report: Box::from(&b"after release"[..]) }])],
        false,
    );
    acts(&mut world, 2, Box::new([Act::Request { key: [6; 16], ask: Ask::Chat { words: Box::from(&b"start"[..]) } }]));
    acts(&mut world, 2, Box::new([Act::Request { key: [7; 16], ask: Ask::Stop { task: Task::Last } }]));
    assert_eq!(world.cancellations.len(), 1);
    acts(&mut world, 2, Box::new([Act::Request { key: [8; 16], ask: Ask::Release { task: Task::Last } }]));
    let peers = world.peers.as_ref().expect("peers");
    assert_eq!(peers.workers[0].seen.len(), 2);
    assert!(peers.workers[0].seen[1].attempt > peers.workers[0].seen[0].attempt);
    assert!(peers.quiescent(), "{:?}", world.trace);
}

#[test]
fn a_request_sent_twice_creates_one_chat_and_replays_the_same_answer() {
    let mut world = scripted(206, vec![Box::new([Script::Finish { report: Box::from(&b"done"[..]) }])], false);
    acts(
        &mut world,
        2,
        Box::new([Act::Request { key: [9; 16], ask: Ask::Chat { words: Box::from(&b"hello"[..]) } }, Act::Twice]),
    );
    let peers = world.peers.as_ref().expect("peers");
    assert_eq!(peers.workers[0].seen.len(), 1);
    let replies: Vec<_> = peers.parties[2]
        .replies
        .iter()
        .filter_map(|(_, reply)| match reply {
            parties::Reply::Started { task } => Some(*task),
            parties::Reply::SignedIn { .. }
            | parties::Reply::Proposed { .. }
            | parties::Reply::Changed { .. }
            | parties::Reply::Done
            | parties::Reply::Refused => None,
        })
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0], replies[1]);
}

#[test]
fn a_worker_reconnects_and_resends_retained_turns_after_hello() {
    let mut world = scripted(
        207,
        vec![Box::new([
            Script::Silence { for_: Duration::from_millis(100) },
            Script::Turn { body: Box::from(&b"retained turn"[..]), cost: 2, read: None },
            Script::Wait,
        ])],
        false,
    );
    acts(&mut world, 2, Box::new([Act::Request { key: [10; 16], ask: Ask::Chat { words: Box::from(&b"hello"[..]) } }]));
    // Hold the next turn's completion while the worker is still live.
    world.store.fault(jig_fake_store::Fault::Hold { commits: 1 });
    world.advance(Duration::from_millis(100));
    let now = Time::from_nanos(world.wall_time().as_nanos());
    let reports = world.peers.as_mut().expect("peers").workers[0]
        .fault(workers::Fault::DropChannel { for_: Duration::from_millis(2) }, now);
    for report in reports {
        let event = world.peers.as_mut().expect("peers").worker_event(report);
        world.send(event);
    }
    world.advance(Duration::from_millis(2));
    world.release_commits();
    let peers = world.peers.as_ref().expect("peers");
    assert!(peers.upstream.iter().filter(|up| matches!(up, workers::Up::Hello { .. })).count() >= 2);
    assert_eq!(peers.workers[0].turn_acks, 1);
    assert_eq!(
        peers.upstream.iter().filter(|up| matches!(up, workers::Up::Turn { .. })).count(),
        2,
        "retained turn follows the new hello"
    );
}
