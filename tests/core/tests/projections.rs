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
    policy.projections.clone_from(&policy.ceiling.grants);
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

fn delegate_agent(world: &mut World, words: &[u8], spend: u64, tasks_left: u32, depth: u32) -> (u64, u64) {
    let mut authority = world.assigned.as_ref().expect("assigned parent").run.authority.clone();
    authority.grants = Box::new([]);
    authority.budget.spend = spend;
    authority.delegation = tasks::Delegation {
        kinds: if tasks_left == 0 { Box::new([]) } else { Box::new([tasks::AuthorityExecutor::Charter(1)]) },
        tasks: tasks_left,
        depth,
    };
    world.send(root::Event::Core(core::Event::DelegateValidated {
        to: ReplyTo::new(Token::new(9200 + spend)),
        key: world.call_key(1),
        batch: Box::new([core::Delegate {
            executor: tasks::Executor::Agent { charter: 1 },
            spec: tasks::Spec { words: Box::from(words), parameters: Box::new([]), inputs: Box::new([]) },
            contract: tasks::Contract::Report { words: 128 },
            authority,
            symbolic_grants: Box::new([]),
            dependencies: Box::new([]),
            wake: tasks::WakePolicy::DEFAULT,
        }]),
        stubs: Box::new([]),
    }));
    let assigned = world.assigned.as_ref().expect("assigned delegate");
    (assigned.task, assigned.attempt)
}

fn projection_rows(world: &World, goal: u64) -> usize {
    world
        .store
        .rows
        .keys()
        .filter(|key| {
            matches!(key,
                root::Key::Core(core::Key::Core(core::CoreKey::Projection(
                    core::ProjectionKey::Goal(number)
                    | core::ProjectionKey::Task { goal: number, .. }
                    | core::ProjectionKey::Lifecycle { goal: number, .. }
                    | core::ProjectionKey::Revision { goal: number, .. }
                ))) if *number == goal
            )
        })
        .count()
}

fn tree_world() -> (World, root::Config) {
    let (mut configuration, mut limits) = fixture(171, false);
    limits.core.tasks.tasks = 4;
    limits.core.tasks.project_tasks = 4;
    limits.core.tasks.tree_tasks = 3;
    limits.core.tasks.depth = 2;
    limits.core.fleet.slots = 3;
    limits.core.fleet.attempts = 4;
    limits.core.fleet.calls = 3;
    limits.core.call_records = 4;
    limits.core.views.runs = 4;
    limits.core.brief.briefs = 4;
    let room = core::room_max(&limits.core).expect("bounded story");
    limits.journal.writes = (room.writes + jig_test_connector::MAX_OUT * 2) * 4;
    limits.journal.held = (room.held + jig_test_connector::MAX_OUT * 2) * 4;
    limits.routes = 500;
    limits.journal.commits = 8;
    let mut policy = configuration.core.authority.policy(1).expect("fixture policy").clone();
    policy.projections.clone_from(&policy.ceiling.grants);
    policy.ceiling.delegation.depth = 3;
    policy.ceiling.delegation.tasks = 4;
    policy.roles[0].authority.delegation.depth = 3;
    policy.roles[0].authority.delegation.tasks = 4;
    let mut rules = configuration.core.authority.rules().clone();
    rules.ceiling.delegation.depth = 3;
    rules.ceiling.delegation.tasks = 4;
    configuration.core.authority = authority::Domain::new(rules, limits.core.authority).expect("tree policy");
    let mut facts = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(&mut configuration.core.authority, authority::Event::Policy { project: 1, policy }, &mut facts);
    configuration.core.settings.chat_authority.delegation =
        authority::Delegation { kinds: Box::new([authority::Executor::Charter(1)]), tasks: 2, depth: 2 };
    let mut restart_configuration = fixture(171, false).0;
    restart_configuration.core.authority =
        authority::Domain::new(configuration.core.authority.rules().clone(), limits.core.authority)
            .expect("same deployment rules");
    authority::step(
        &mut restart_configuration.core.authority,
        authority::Event::Policy {
            project: 1,
            policy: configuration.core.authority.policy(1).expect("tree policy").clone(),
        },
        &mut Queue::with_capacity(authority::POLICY_MAX_OUT),
    );
    restart_configuration.core.settings.chat_authority.delegation =
        configuration.core.settings.chat_authority.delegation.clone();
    (World::configured(171, false, configuration, limits), restart_configuration)
}

#[test]
fn a_delegate_of_a_delegate_ending_feeds_the_whole_plan_once_and_closing_waits_for_its_write() {
    let (mut world, restart_configuration) = tree_world();
    world.end(
        tasks::End::Finished {
            result: tasks::TaskResult::Report { words: Box::from(&b"chat done"[..]) },
            cancel_delegates: false,
        },
        0,
    );
    world.send(root::Event::Core(core::Event::People(people::Event::Ask {
        reply_to: ReplyTo::new(Token::new(9200)),
        sign_in: world.sign_in.expect("signed in party"),
        key: [33; 16],
        ask: people::Ask::SetGoal {
            project: 1,
            spec: Box::from(&b"Whole tree"[..]),
            charter: 1,
            budget: 300,
            priority: 1,
        },
    })));
    let goal = {
        let row = world.assigned.as_ref().expect("goal assigned");
        (row.task, row.attempt)
    };
    let child = delegate_agent(&mut world, b"Delegate", 150, 1, 1);
    assert_ne!(child.0, goal.0, "child admitted: {:?}", world.answers);
    let grandchild = delegate_agent(&mut world, b"Delegate's delegate", 20, 0, 0);
    assert_ne!(grandchild.0, child.0, "grandchild admitted: {:?}", world.answers);
    world.projections.clear();
    world.end(
        tasks::End::Finished {
            result: tasks::TaskResult::Report { words: Box::from(&b"leaf done"[..]) },
            cancel_delegates: false,
        },
        0,
    );
    let terminal = world
        .projections
        .iter()
        .filter(|(_, _, feed)| {
            feed.plan.iter().any(|task| task.number == grandchild.0 && matches!(task.phase, tasks::Phase::Ended(_)))
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 2, "one terminal feed to each connector");
    assert_eq!(terminal[0].0, terminal[1].0, "one decision");
    for (_, _, feed) in terminal {
        assert_eq!(feed.plan.len(), 3, "goal, delegate, and ended grandchild");
        let ended = feed
            .milestones
            .iter()
            .filter(|milestone| {
                matches!(milestone.identity, core::MilestoneId::Lifecycle { task, .. } if task == grandchild.0)
                    && matches!(milestone.phase, Some(tasks::Phase::Ended(_)))
            })
            .collect::<Vec<_>>();
        assert_eq!(ended.len(), 1, "one immutable terminal milestone");
        assert!(world.store.rows.values().any(|row| matches!(row, root::Record::Core(core::Record::Tasks(tasks::Stored::Milestone(row))) if matches!(ended[0].identity, core::MilestoneId::Lifecycle { task, position } if task == row.task && position == row.position))));
    }
    settle_tree(&mut world, goal, child, restart_configuration);
}

fn settle_tree(world: &mut World, goal: (u64, u64), child: (u64, u64), restart_configuration: root::Config) {
    let read = world
        .store
        .rows
        .values()
        .find_map(|row| match row {
            root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if row.number == child.0 => {
                Some(row.last_message)
            }
            root::Record::Core(_) | root::Record::Connector { .. } => None,
        })
        .expect("live parent");
    world.send(root::Event::Turn {
        channel: Token::new(7),
        task: child.0,
        attempt: child.1,
        turn: 1,
        cumulative: 0,
        read: Some(read),
        transcript: Box::from(&b"Read delegate result"[..]),
    });
    world.send(root::Event::Answer {
        channel: Token::new(7),
        task: child.0,
        attempt: child.1,
        cumulative: 0,
        end: tasks::End::Finished {
            result: tasks::TaskResult::Report { words: Box::from(&b"delegate done"[..]) },
            cancel_delegates: false,
        },
    });
    world.hold_projection_settlements = true;
    world.projections.clear();
    world.send(root::Event::Answer {
        channel: Token::new(7),
        task: goal.0,
        attempt: goal.1,
        cumulative: 0,
        end: tasks::End::Finished {
            result: tasks::TaskResult::Report { words: Box::from(&b"goal done"[..]) },
            cancel_delegates: false,
        },
    });
    assert_eq!(world.projections.iter().filter(|(_, _, feed)| feed.closing).count(), 2);
    assert_eq!(projection_rows(world, goal.0), 4, "header and full ended plan retained");
    world.restart_with(restart_configuration);
    assert_eq!(projection_rows(world, goal.0), 4, "closing retention survives restart");
    world.hold_writes = true;
    world.send(root::Event::ProjectionEffect { goal: goal.0, number: 1, effect: World::effect() });
    assert_eq!(world.pending_writes.len(), 1, "closing write admitted after the task ended");
    assert_eq!(projection_rows(world, goal.0), 4);
    world.release_writes();
    assert_eq!(world.systems[0].observed().iter().filter(|effect| effect.applied).count(), 1);
    world.send(root::Event::Core(core::Event::ProjectionSettled { goal: goal.0, connector: 1 }));
    assert_eq!(projection_rows(world, goal.0), 4, "other connector still owns its last feed");
    world.send(root::Event::Core(core::Event::ProjectionSettled { goal: goal.0, connector: 2 }));
    assert_eq!(projection_rows(world, goal.0), 0, "last settlement releases projection state");
}
