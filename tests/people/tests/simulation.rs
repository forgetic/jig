use jig_core_people::{
    Ask, Entry, EntryKind, Holding, IdentityKey, InitialOwner, Outcome, ProposalChoice, ProposalDecision, Refusal,
    Reply, Role, Seed, Whom,
};
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
fn a_request_sent_twice_is_made_once_across_a_restart() {
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
fn an_observer_is_refused_a_request_and_told_why() {
    let mut world = World::new(Settings::calm(13));
    let call = world.signin(10, 1);
    world.commit_all();
    let Reply::SignedIn { person, .. } = world.reply(call).expect("person signed in") else { panic!("signed in") };
    world.roles(1, Box::new([Holding { person, role: Role::Observer }]));
    world.commit_all();
    let before = world.stats().routes;
    let call = world.request(
        10,
        [3; 16],
        Ask::SetGoal { project: 1, spec: b"goal".to_vec().into_boxed_slice(), charter: 1, budget: 1, priority: 1 },
    );
    assert!(world.reply(call).is_none(), "refusal waits for its answer commit");
    world.commit_all();
    assert_eq!(world.reply(call), Some(Reply::Outcome(Outcome::Refused(Refusal::Role))));
    assert_eq!(world.stats().routes, before);
    world.assert_settled();
}

#[test]
fn a_service_sets_a_goal_its_role_allows_and_is_refused_a_chat() {
    let owners = Box::new([InitialOwner {
        project: 1,
        identity: IdentityKey { provider: 0, subject: 1_u64.to_be_bytes().into() },
    }]);
    let mut world = World::with_owners(Settings::calm(14), owners);
    world.roles(1, Box::new([]));
    world.commit_all();
    let owner = world.signin(10, 1);
    world.commit_all();
    assert!(matches!(world.reply(owner), Some(Reply::SignedIn { .. })));
    let made = world.request(
        10,
        [1; 16],
        Ask::MakeService { project: 1, name: b"build".to_vec().into_boxed_slice(), role: Role::Member },
    );
    world.commit_all();
    let Some(Reply::Outcome(Outcome::ServiceMade { person })) = world.reply(made) else { panic!("service made") };
    let signed_in = world.signin_service(11, person);
    world.commit_all();
    assert!(matches!(world.reply(signed_in), Some(Reply::SignedIn { person: found, .. }) if found == person));
    let goal = world.request(
        11,
        [2; 16],
        Ask::SetGoal { project: 1, spec: b"goal".to_vec().into_boxed_slice(), charter: 1, budget: 1, priority: 1 },
    );
    world.commit_all();
    assert!(matches!(world.reply(goal), Some(Reply::Outcome(Outcome::GoalStarted { .. }))));
    let routes = world.stats().routes;
    let chat = world.ask(11, [3; 16], b"hello");
    world.commit_all();
    assert_eq!(world.reply(chat), Some(Reply::Outcome(Outcome::Refused(Refusal::Role))));
    assert_eq!(world.stats().routes, routes);
    assert_eq!(world.tasks(), 1);
    world.assert_settled();
}

#[test]
fn two_maintainers_decide_one_proposal_and_the_second_is_told() {
    let mut world = World::new(Settings::calm(15));
    let first = world.signin(10, 1);
    let second = world.signin(11, 2);
    world.commit_all();
    let Some(Reply::SignedIn { person: first_person, .. }) = world.reply(first) else { panic!("first signed in") };
    let Some(Reply::SignedIn { person: second_person, .. }) = world.reply(second) else { panic!("second signed in") };
    world.roles(
        1,
        Box::new([
            Holding { person: first_person, role: Role::Maintainer },
            Holding { person: second_person, role: Role::Maintainer },
        ]),
    );
    world.commit_all();
    let first = world.request(
        10,
        [1; 16],
        Ask::DecideProposal { project: 1, proposer: 7, proposal: 8, decision: ProposalDecision::Accept },
    );
    world.commit_all();
    let expected = Some(Reply::Outcome(Outcome::ProposalDecided {
        proposer: 7,
        proposal: 8,
        by: first_person,
        choice: ProposalChoice::Accepted,
    }));
    assert_eq!(world.reply(first), expected);
    let second = world.request(
        11,
        [2; 16],
        Ask::DecideProposal {
            project: 1,
            proposer: 7,
            proposal: 8,
            decision: ProposalDecision::Reject { reason: b"no".to_vec().into_boxed_slice() },
        },
    );
    world.commit_all();
    assert_eq!(world.reply(second), expected);
    world.assert_settled();
}

#[test]
fn roles_are_seeded_from_an_adopted_resources_parties() {
    let mut world = World::new(Settings::calm(16));
    world.roles(1, Box::new([]));
    world.commit_all();
    let identity = IdentityKey { provider: 0, subject: 77_u64.to_be_bytes().into() };
    world.seed(1, Box::new([Seed { identity: identity.clone(), candidate: 42, role: Role::Maintainer }]));
    world.commit_all();
    assert_eq!(world.role_for_identity(identity.clone(), 1), Some((42, Role::Maintainer)));
    let call = world.signin(10, 77);
    world.commit_all();
    assert!(matches!(world.reply(call), Some(Reply::SignedIn { person: 42, .. })));
    assert_eq!(world.role_for_identity(identity, 1), Some((42, Role::Maintainer)));
    world.assert_settled();
}

#[test]
fn a_role_task_leaves_every_inbox_after_one_holder_takes_it() {
    let mut world = World::new(Settings::calm(17));
    let first = world.signin(10, 1);
    let second = world.signin(11, 2);
    world.commit_all();
    let Some(Reply::SignedIn { person: first_person, .. }) = world.reply(first) else { panic!("first signed in") };
    let Some(Reply::SignedIn { person: second_person, .. }) = world.reply(second) else { panic!("second signed in") };
    world.roles(
        1,
        Box::new([
            Holding { person: first_person, role: Role::Maintainer },
            Holding { person: second_person, role: Role::Maintainer },
        ]),
    );
    world.commit_all();
    let task = 99;
    world.waiting(
        task,
        Box::new([Entry {
            task,
            project: 1,
            whom: Whom::Role { project: 1, role: 1 },
            kind: EntryKind::PersonTask,
            at: skein_lib::Wall::EPOCH,
        }]),
    );
    world.check_role_waiting(task, [first_person, second_person], true);
    let call = world.request(10, [3; 16], Ask::TakePerson { project: 1, task });
    world.commit_all();
    assert_eq!(world.reply(call), Some(Reply::Outcome(Outcome::PersonTaken { task })));
    world.check_role_waiting(task, [first_person, second_person], false);
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
