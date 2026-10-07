use jig_core_tasks::{
    Hold, HoldKind, Holding, Kind, MessageKind, Name, NoticeState, Party, Phase, Refusal, Subscription,
    SubscriptionKind, Taken,
};
use jig_tasks_world::{LIMITS, Reply, World, task};
use skein_lib::{Duration, ReplyTo, Token};

fn resource(value: u8) -> Holding {
    Holding::Write { resource: Name { connector: 1, path: Box::new([Box::new([value])]) }, kind: 1 }
}

fn configured(seed: u64) -> World {
    let mut world = World::new(seed, LIMITS);
    world.configure_holds(1, vec![Kind { connector: 1, kind: 1, hold: HoldKind::Exclusive { taken: Taken::Waits } }]);
    world
}

#[test]
fn two_tasks_each_wanting_two_holds_the_other_wants_never_wait_in_a_circle() {
    let mut world = configured(301);
    let mut first = task(1, &[]);
    first.holdings = Box::new([resource(1), resource(2)]);
    let mut second = task(2, &[]);
    second.holdings = Box::new([resource(2), resource(1)]);
    assert_eq!(world.make(Party::Person(1), vec![first, second]), Reply::Made(vec![1, 2]));
    assert!(world.record(1).holds_taken);
    assert!(!world.record(2).holds_taken);
    assert!(world.activations.contains(&1));
    assert!(!world.activations.contains(&2));
    world.restart();
    assert!(!world.record(2).holds_taken);
    world.claim(1, 1);
    world.finish(1);
    world.settle(1);
    assert!(world.record(2).holds_taken);
    assert!(world.activations.contains(&2));
}

#[test]
fn a_task_waiting_past_its_bound_for_a_hold_is_held_and_its_requester_told() {
    let mut world = configured(302);
    assert_eq!(world.make(Party::Person(1), vec![task(1, &[])]), Reply::Made(vec![1]));
    let mut holder = task(2, &[]);
    holder.holdings = Box::new([resource(1)]);
    let mut waiter = task(3, &[]);
    waiter.holdings = Box::new([resource(1)]);
    assert_eq!(world.make(Party::Task(1), vec![holder, waiter]), Reply::Made(vec![2, 3]));
    world.send(jig_core_tasks::Event::Subscribe {
        reply_to: ReplyTo::new(Token::new(500)),
        task: 1,
        subscription: Subscription {
            number: 10,
            kind: SubscriptionKind::Task { target: 3, held: true, result: false },
        },
    });
    world.restart();
    world.elapse(Duration::from_secs(11));
    assert!(matches!(world.record(3).phase, Phase::Held { why: Hold::HoldsWaited, .. }));
    assert!(!world.record(3).holds_taken);
    assert!(
        world
            .record(1)
            .inbox
            .iter()
            .any(|word| matches!(word.kind, MessageKind::Notice { target: 3, state: NoticeState::Held, .. }))
    );
}

#[test]
fn a_refusing_kind_rejects_the_whole_batch_and_names_the_holder() {
    let mut world = World::new(303, LIMITS);
    world.configure_holds(1, vec![Kind { connector: 1, kind: 1, hold: HoldKind::Exclusive { taken: Taken::Refuses } }]);
    let mut first = task(1, &[]);
    first.holdings = Box::new([resource(1)]);
    assert_eq!(world.make(Party::Person(1), vec![first]), Reply::Made(vec![1]));
    let mut second = task(2, &[]);
    second.holdings = Box::new([resource(1)]);
    assert!(matches!(world.make(Party::Person(1), vec![second]), Reply::Refused(problem)
        if problem.why == Refusal::HoldTaken && problem.blocked_by.as_deref() == Some(&[1][..])));
}

#[test]
fn the_higher_priority_goal_gets_a_freed_hold_first() {
    let mut world = configured(304);
    let mut holder = task(1, &[]);
    holder.holdings = Box::new([resource(1)]);
    let mut low = task(2, &[]);
    low.holdings = Box::new([resource(1)]);
    low.tracked = Some(1);
    let mut high = task(3, &[]);
    high.holdings = Box::new([resource(1)]);
    high.tracked = Some(9);
    assert_eq!(world.make(Party::Person(1), vec![holder]), Reply::Made(vec![1]));
    assert_eq!(world.make(Party::Person(1), vec![low, high]), Reply::Made(vec![2, 3]));
    world.claim(1, 1);
    world.finish(1);
    world.settle(1);
    assert!(world.record(3).holds_taken);
    assert!(!world.record(2).holds_taken);
}
