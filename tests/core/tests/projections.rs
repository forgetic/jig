use jig_core as core;
use jig_core_authority as authority;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_world::effects::{World, fixture};
use jig_test_domain as root;
use skein_lib::{Queue, ReplyTo, Token};

#[test]
fn projections_use_project_grants_require_verdicts_and_observe_a_policy_narrowing_after_restart() {
    let (mut configuration, limits) = fixture(170, true);
    let mut policy = configuration.core.authority.policy(1).expect("fixture policy").clone();
    policy.projections = policy.ceiling.grants.clone();
    policy.roles[0].authority.grants = Box::new([]);
    configuration.core.settings.chat_authority.grants = Box::new([]);
    let mut facts = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(&mut configuration.core.authority, authority::Event::Policy { project: 1, policy }, &mut facts);
    assert_eq!(facts.pop(), Some(authority::PolicyFact::Changed { project: 1 }));
    let mut world = World::configured(170, true, configuration, limits);
    world.send(root::Event::Core(core::Event::People(people::Event::Ask {
        reply_to: ReplyTo::new(Token::new(9000)),
        sign_in: world.sign_in.expect("signed in party"),
        key: [31; 16],
        ask: people::Ask::SetGoal {
            project: 1,
            spec: Box::from(&b"Project this goal"[..]),
            charter: 1,
            budget: 20,
            priority: 1,
        },
    })));
    let goal = world
        .people_answers
        .iter()
        .find_map(|reply| match reply {
            people::Reply::Outcome(people::Outcome::GoalStarted { task }) => Some(*task),
            people::Reply::SignedIn { .. }
            | people::Reply::SignedOut
            | people::Reply::Outcome(_)
            | people::Reply::Refused(_) => None,
        })
        .expect("tracked goal admitted");
    let record = world
        .store
        .rows
        .values()
        .find_map(|row| match row {
            root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if row.number == goal => Some(row),
            root::Record::Core(_) | root::Record::Connector { .. } => None,
        })
        .expect("durable goal");
    assert!(record.authority.grants.is_empty());
    assert_eq!(record.numbers.spent, 0);
    for number in 40..48 {
        world.send(root::Event::Core(core::Event::People(people::Event::Ask {
            reply_to: ReplyTo::new(Token::new(9100 + u64::from(number))),
            sign_in: world.sign_in.expect("signed in party"),
            key: [number; 16],
            ask: people::Ask::Say { project: 1, task: goal, words: Box::from(&b"waiting words"[..]) },
        })));
    }
    world.send(root::Event::ProjectionEffect { goal, number: 1, effect: World::effect() });
    assert!(world.systems[0].observed().is_empty(), "a missing verdict keeps no projection effect");
    world.observed(13);
    world.send(root::Event::ProjectionEffect { goal, number: 1, effect: World::effect() });
    assert_eq!(world.systems[0].observed().iter().filter(|effect| effect.applied).count(), 1);
    let saved = world
        .store
        .rows
        .values()
        .find_map(|row| match row {
            root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if row.number == goal => Some(row),
            root::Record::Core(_) | root::Record::Connector { .. } => None,
        })
        .expect("durable goal");
    assert_eq!(saved.numbers.spent, 0, "the priced projection charges no task budget");
    world.send(root::Event::Core(core::Event::People(people::Event::Ask {
        reply_to: ReplyTo::new(Token::new(9001)),
        sign_in: world.sign_in.expect("signed in party"),
        key: [32; 16],
        ask: people::Ask::ChangePolicy {
            project: 1,
            change: people::PolicyChange::Projections { grants: Box::new([]) },
        },
    })));
    assert!(
        world
            .people_answers
            .iter()
            .any(|reply| matches!(reply, people::Reply::Outcome(people::Outcome::PolicyChanged { project: 1 })))
    );
    world.restart(170, true);
    let mut next = World::effect();
    next.purpose = 20;
    next.target = 14;
    world.send(root::Event::ProjectionEffect { goal, number: 1, effect: next });
    assert_eq!(
        world.systems[0].observed().iter().filter(|effect| effect.applied).count(),
        1,
        "a narrowed policy refuses the next projection"
    );
    assert!(
        world.store.rows.values().any(|row| matches!(row,
            root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if row.number == goal
                && row.inbox.iter().any(|word| word.words.as_ref() == b"Projection write refused")
        )),
        "the goal hears the refusal as durable news"
    );
}
