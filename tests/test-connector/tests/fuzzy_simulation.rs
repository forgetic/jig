use std::collections::{BTreeMap, VecDeque};

use jig_test_connector::{Effect, Event, Outcome, Record, RecordKey, Request, ResourceRole, SystemEvent};
use jig_test_connector_world::{World, kind, path};
use jig_test_system::{Fault, Observed};
use skein_lib::Token;

fn run(seed: u8) -> (Vec<Request>, Vec<Observed>, BTreeMap<RecordKey, Record>, u64) {
    let mut world = World::new(seed);
    world.system.preload_history(u32::from(seed) * 10);
    let local = u16::from(seed % 4) + 1;
    let target = u64::from(seed) + 5;
    let condition = if local == 2 { Some(1) } else { None };
    if condition.is_some() {
        world.system.other_hand(&path(seed, 1), 1);
    }
    let token = Token::new(1);
    let effect = Effect {
        kind: kind(seed, local),
        resources: Box::from([path(seed, 1)]),
        purpose: 10,
        condition,
        target,
        state: target,
    };
    assert!(matches!(world.event(Event::Describe { token, effect }).as_slice(), [Request::Described { .. }]));
    let kept = world.event(Event::Keep { token, entry: 1, task: 4, key: world.key(4, 10) });
    if seed.is_multiple_of(5) {
        world.cold_restart();
        world.settle_restart(&mut VecDeque::new());
    } else {
        let fault = match seed % 4 {
            0 => Fault::None,
            1 => Fault::BeforeSend,
            2 => Fault::AfterApply,
            3 => Fault::Late,
            _ => unreachable!(),
        };
        world.release(&kept, &mut VecDeque::from([fault]));
        if seed.is_multiple_of(3) {
            world.cold_restart();
            world.settle_restart(&mut VecDeque::new());
        }
    }
    world.wall = 7;
    for _ in 0..3 {
        let due = world.fire();
        world.release(&due, &mut VecDeque::new());
    }
    if let Some(late) = world.system.deliver_late() {
        world.event(Event::System(late));
    }
    world.event(Event::Adopt { project: 1, resource: path(seed, 2), role: ResourceRole::Owned });
    world.event(Event::Names { task: 2, project: 1, resources: Box::from([path(seed, 2)]) });
    world.event(Event::Subscribe { task: 2, topic: kind(seed, 1), wake_at: 5, keep_at: 2 });
    world.event(Event::System(SystemEvent::Pool {
        path: path(seed, 2),
        slots: u32::from(seed % 3),
        lost: Box::from([2]),
    }));
    world.event(Event::System(SystemEvent::News {
        topic: kind(seed, 1),
        importance: u16::from(seed % 7),
        origin: jig_test_connector::Origin::Other,
    }));
    assert!(
        world
            .trace()
            .iter()
            .filter(|request| matches!(request, Request::Outcome { outcome: Outcome::Made { .. }, .. }))
            .count()
            <= 1
    );
    assert!(world.system.calls() < 20);
    (world.seen, world.system.observed().to_vec(), world.records, world.system.calls())
}

#[test]
fn random_faults_restarts_and_news_replay_with_the_same_committed_records() {
    for seed in 1..=64 {
        assert_eq!(run(seed), run(seed), "seed {seed}");
    }
}
