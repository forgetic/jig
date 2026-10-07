use jig_core_people::{Holding, IdentityKey, InitialOwner, Outcome, Refusal, Reply, Role};
use jig_people_world::{Settings, World};
use skein_lib::Duration;
use skein_world::domain::assert_replays;

fn member(world: &mut World) -> u64 {
    let call = world.signin(10, 1);
    assert!(world.reply(call).is_none());
    world.commit_all();
    let Reply::SignedIn { person, .. } = world.reply(call).expect("test values are admitted and fit") else {
        panic!("signed in")
    };
    world.roles(1, Box::new([Holding { person, role: Role::Member }]));
    world.commit_all();
    person
}

#[test]
fn the_people_stories_and_both_restart_cuts() {
    for seed in 0..3 {
        let mut world = World::new(Settings::random(seed));
        world.run();
    }
}

#[test]
fn state_runs_ahead_while_duplicate_replies_wait_for_the_same_commit() {
    let mut world = World::new(Settings::calm(4));
    member(&mut world);
    let first = world.ask(10, [1; 16], b"hello");
    let duplicate = world.ask(10, [1; 16], b"hello");
    assert!(world.reply(first).is_none() && world.reply(duplicate).is_none());
    assert_eq!(world.stats().routes, 1);
    assert_eq!(world.tasks(), 0);
    world.commit_all();
    assert_eq!(world.reply(first), world.reply(duplicate));
    assert_eq!(world.tasks(), 1);
    world.assert_settled();
}

#[test]
fn a_commit_failed_or_lost_releases_no_reply_and_the_retry_makes_one_task() {
    let mut world = World::new(Settings::calm(5));
    member(&mut world);
    let first = world.ask(10, [1; 16], b"hello");
    assert!(world.reply(first).is_none());
    world.restart();
    assert_eq!(world.tasks(), 0);
    let retry = world.ask(10, [1; 16], b"hello");
    world.commit_all();
    assert!(matches!(world.reply(retry), Some(Reply::Outcome(Outcome::Started { .. }))));
    assert_eq!(world.tasks(), 1);
    world.assert_settled();
}

#[test]
fn a_durable_decision_with_its_reply_lost_is_replayed_after_restart() {
    let mut world = World::new(Settings::calm(6));
    member(&mut world);
    world.hold_replies(true);
    let first = world.ask(10, [1; 16], b"hello");
    world.commit_all();
    assert!(world.reply(first).is_none());
    assert_eq!(world.tasks(), 1);
    let routes = world.stats().routes;
    world.restart();
    world.hold_replies(false);
    let retry = world.ask(10, [1; 16], b"hello");
    world.commit_all();
    assert!(world.reply(retry).is_some());
    assert_eq!(world.stats().routes, routes);
    assert_eq!(world.tasks(), 1);
    world.assert_settled();
}

#[test]
fn transient_root_wait_keeps_one_route_and_each_waiter_receives_one_reply() {
    let mut world = World::new(Settings::calm(7));
    member(&mut world);
    world.defer_routes(true);
    let calls = (0..3).map(|_| world.ask(10, [1; 16], b"hello")).collect::<Vec<_>>();
    let extra = world.ask(10, [1; 16], b"hello");
    world.commit_all();
    assert_eq!(world.reply(extra), Some(Reply::Refused(Refusal::Busy)));
    assert_eq!(world.stats().routes, 1);
    world.decide_deferred();
    world.commit_all();
    for call in calls {
        assert!(matches!(world.reply(call), Some(Reply::Outcome(Outcome::Started { .. }))));
    }
    assert_eq!(world.tasks(), 1);
    world.assert_settled();
}

#[test]
fn pending_authorisation_can_be_lost_and_retried_without_repeating_durable_work() {
    let mut world = World::new(Settings::calm(8));
    member(&mut world);
    world.defer_routes(true);
    let first = world.ask(10, [1; 16], b"hello");
    assert!(world.reply(first).is_none());
    world.restart();
    world.defer_routes(false);
    let retry = world.ask(10, [1; 16], b"hello");
    world.commit_all();
    assert!(world.reply(retry).is_some());
    assert_eq!(world.tasks(), 1);
    world.assert_settled();
}

#[test]
fn an_expired_sign_in_is_refused() {
    let owners = Box::new([InitialOwner {
        project: 1,
        identity: IdentityKey { provider: 0, subject: 1_u64.to_be_bytes().into() },
    }]);
    let mut world = World::with_owners(Settings::calm(9), owners);
    world.roles(1, Box::new([]));
    world.commit_all();
    let call = world.signin(10, 1);
    world.commit_all();
    assert!(world.reply(call).is_some());
    world.advance(Duration::from_secs(30));
    world.restart();
    let call = world.ask(10, [1; 16], b"owner");
    world.commit_all();
    assert!(matches!(world.reply(call), Some(Reply::Outcome(Outcome::Started { .. }))));
    world.advance(Duration::from_secs(31));
    world.restart();
    let call = world.ask(10, [2; 16], b"expired");
    world.commit_all();
    assert_eq!(world.reply(call), Some(Reply::Refused(Refusal::SignIn)));
    world.assert_settled();
}

#[test]
fn facts_change_nothing_and_a_seed_replays() {
    let run = |seed| {
        let mut world = World::new(Settings::random(seed));
        world.run();
        (world.trace().to_vec(), world.stats().clone())
    };
    assert_replays(3, 4, run);
    let mut none = World::new(Settings {
        limits: jig_core_people::Limits { facts: 0, ..jig_people_world::LIMITS },
        ..Settings::calm(3)
    });
    none.run();
    let mut many = World::new(Settings::calm(3));
    many.run();
    assert_eq!(none.trace(), many.trace());
}
