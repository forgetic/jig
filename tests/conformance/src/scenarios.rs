//! The shared scenarios bound to two test connectors and independent hosts.
use crate::{Config, Testing, actions::World};
use jig_conformance::referee::{Promise, Violation};
use jig_conformance::scenarios::{Action, Scenario, ScenarioApplication};
use jig_core as core;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_test_connector as connector;
use jig_test_domain as root;
use skein_lib::{ReplyTo, Token, Wall};

fn check(holds: bool, why: &str) -> Result<(), Violation> {
    if holds { Ok(()) } else { Err(Violation { promise: Promise::Lost, why: why.into() }) }
}

fn parent(world: &World) -> (u64, u64) {
    world.peers.assignments[0]
}

fn authority(world: &World) -> tasks::Authority {
    let task = parent(world).0;
    match world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))) {
        Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) => row.authority.clone(),
        row => panic!("scripted parent's durable authority: {row:?}"),
    }
}

fn ask(world: &mut World, key: u8, ask: people::Ask) {
    world.send(root::Event::Core(core::Event::People(people::Event::Ask {
        reply_to: ReplyTo::new(Token::new(6000 + u64::from(key))),
        sign_in: world.peers.peers.parties[0].sign_in.expect("scripted sign-in"),
        key: [key; 16],
        ask,
    })));
}

fn effect(world: &mut World, completion: u32, proposal: bool) {
    let (task, attempt) = parent(world);
    let mut effect = jig_core_world::effects::World::effect();
    if world.peers.scenario == Some(Scenario::OtherHand) {
        effect.condition = Some(0);
    }
    world.send(root::Event::EffectCall {
        to: ReplyTo::new(Token::new(7000 + u64::from(completion))),
        key: core::CallKey { task, attempt, completion, position: 0 },
        number: 1,
        effect,
        deadline: Wall::from_nanos(world.clock().now + 1_000_000_000),
        proposal: proposal.then(|| Box::from(&b"ask the holder"[..])),
    });
}

fn named_part(world: &World, completion: u32) -> Option<&core::CallPart> {
    let (task, attempt) = parent(world);
    let key = core::CallKey { task, attempt, completion, position: 0 };
    match world.store.rows.get(&root::Key::Core(core::Key::Core(core::CoreKey::Call(key)))) {
        Some(root::Record::Core(core::Record::Core(core::CoreRecord::Call(row)))) => Some(&row.part),
        Some(root::Record::Core(_) | root::Record::Connector { .. }) | None => None,
    }
}

fn pool(world: &mut World, slots: u32) {
    world.send(root::Event::Connector {
        number: 1,
        event: connector::Event::System(connector::SystemEvent::Pool {
            path: jig_test_connector_world::path(1, 2),
            slots,
            lost: Box::new([]),
        }),
    });
}

fn release_writes(world: &mut World) {
    world.peers.hold_writes = false;
    for (number, call) in std::mem::take(&mut world.peers.pending_writes) {
        let event = world.systems[usize::from(number - 1)].answer(call, jig_test_system::Fault::None);
        world.send(root::Event::Connector { number, event: connector::Event::System(event) });
    }
}

fn wake(world: &mut World, task: u64) -> Result<(), Violation> {
    if !world.peers.assignments.iter().any(|(number, _)| *number == task) {
        world.advance(6_000_000_000);
        world.drain()?;
        world.advance(1_000_000_000);
        world.drain()?;
    }
    let (task, attempt) =
        *world.peers.assignments.iter().rev().find(|(number, _)| *number == task).expect("independent assignment");
    let worker = world
        .peers
        .peers
        .workers
        .iter_mut()
        .find(|worker| worker.seen.iter().any(|run| run.task == task && run.attempt == attempt))
        .expect("assigned host");
    worker.message(task, attempt, 100, b"complete scripted work".as_slice().into());
    Ok(())
}

fn ended(world: &World, task: u64) -> bool {
    world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(task))))
}

impl ScenarioApplication for Testing {
    fn configuration(scenario: Scenario, seed: u64) -> Option<Config> {
        Some(Config::new(seed, false, if scenario == Scenario::ShrinkingPool { 3 } else { 2 }, Some(scenario)))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive binding keeps shared story actions at outside boundaries"
    )]
    fn action(world: &mut World, action: Action) -> Result<(), Violation> {
        match action {
            Action::Ready => {
                if world.peers.assignments.is_empty() {
                    world.advance(6_000_000_000);
                    world.drain()?;
                    world.advance(1_000_000_000);
                    world.drain()?;
                }
                check(!world.peers.assignments.is_empty(), "caller was not assigned")?;
                world.peers.initial_assignments = world.peers.assignments.len();
            }
            Action::Late => world.peers.next_fault = jig_test_system::Fault::Late,
            Action::Effect(completion) => effect(world, completion, false),
            Action::DeliverLate => {
                for index in 0..2 {
                    while let Some(event) = world.systems[index].deliver_late() {
                        world.send(root::Event::Connector {
                            number: u16::try_from(index + 1).expect("two systems"),
                            event: connector::Event::System(event),
                        });
                    }
                }
            }
            Action::OtherHand => world.systems[0].other_hand(&jig_test_connector_world::path(1, 1), 13),
            Action::HoldWrites => world.peers.hold_writes = true,
            Action::ReleaseWrites => release_writes(world),
            Action::CheckDeadline => {
                let deadline = world
                    .store
                    .rows
                    .values()
                    .find_map(|row| match row {
                        root::Record::Connector { number: 1, record: connector::Record::Outbox(entry) } => {
                            entry.attempt.map(|attempt| attempt.deadline)
                        }
                        root::Record::Core(_) | root::Record::Connector { .. } => None,
                    })
                    .expect("durable uncertain attempt");
                if let Some(saved) = world.peers.saved_deadline {
                    check(deadline == saved, "cold restart changed the uncertain deadline")?;
                    check(world.clock().now < saved.as_nanos(), "restarts passed the uncertain deadline")?;
                } else {
                    world.peers.saved_deadline = Some(deadline);
                }
            }
            Action::ChangeJudge => {
                world.systems[1].other_hand(&jig_test_connector_world::path(1, 1), 14);
                let event = world.systems[1].answer(
                    connector::SystemRequest::ReadFact {
                        resource: jig_test_connector_world::path(1, 1),
                        observed: Wall::from_nanos(world.clock().now),
                    },
                    jig_test_system::Fault::None,
                );
                world.send(root::Event::Connector { number: 2, event: connector::Event::System(event) });
            }
            Action::FillPool => {
                world.send(root::Event::Connector {
                    number: 1,
                    event: connector::Event::Adopt {
                        project: 1,
                        resource: jig_test_connector_world::path(1, 2),
                        role: connector::ResourceRole::Owned,
                    },
                });
                pool(world, 2);
                world.drain()?;
                for completion in 1..=3 {
                    for _ in 0..2 {
                        let mut authority = authority(world);
                        authority.budget.spend = 10;
                        authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
                        let (task, attempt) = parent(world);
                        world.send(root::Event::Core(core::Event::DelegateValidated {
                            to: ReplyTo::new(Token::new(8000 + u64::from(completion))),
                            key: core::CallKey { task, attempt, completion, position: 0 },
                            batch: Box::new([core::Delegate {
                                executor: tasks::Executor::Agent { charter: 1 },
                                spec: tasks::Spec {
                                    words: b"pool work".as_slice().into(),
                                    parameters: Box::new([tasks::Parameter::Resource {
                                        name: 1,
                                        connector: 1,
                                        resource: 2,
                                    }]),
                                    inputs: Box::new([]),
                                },
                                contract: tasks::Contract::Report { words: 128 },
                                authority,
                                symbolic_grants: Box::new([]),
                                dependencies: Box::new([]),
                                wake: tasks::WakePolicy::DEFAULT,
                            }]),
                            stubs: Box::new([]),
                        }));
                        world.drain()?;
                        if named_part(world, completion).is_some() {
                            break;
                        }
                    }
                    let core::CallPart::Delegated(members) =
                        named_part(world, completion).expect("durable delegate reply")
                    else {
                        panic!("delegate refused: {:?}", named_part(world, completion));
                    };
                    world.peers.children.push(members[0]);
                }
                if world
                    .peers
                    .children
                    .iter()
                    .filter(|child| world.peers.assignments.iter().any(|(task, _)| task == *child))
                    .count()
                    < 2
                {
                    world.advance(6_000_000_000);
                    world.drain()?;
                    world.advance(1_000_000_000);
                    world.drain()?;
                }
                check(world.peers.assignments.len() == 3, "two holders and parent were not assigned")?;
            }
            Action::ShrinkPool => {
                pool(world, 1);
            }
            Action::FinishHolder(index) => {
                let task = world.peers.children[usize::try_from(index).expect("three holders")];
                wake(world, task)?;
                world.drain()?;
                check(ended(world, task), "pool holder did not end")?;
                if index == 0 {
                    check(world.peers.assignments.len() == 3, "shrinking pool admitted its waiter too soon")?;
                }
            }
            Action::Propose => {
                effect(world, 2, true);
                world.drain()?;
                if named_part(world, 2).is_none() {
                    effect(world, 2, true);
                    world.drain()?;
                }
                let core::CallPart::Proposed { proposal } = named_part(world, 2).expect("durable proposal reply")
                else {
                    panic!("proposal refused: {:?}", world.peers.calls);
                };
                world.peers.pending_proposal = Some(*proposal);
            }
            Action::NarrowPolicy => ask(
                world,
                43,
                people::Ask::ChangePolicy {
                    project: 1,
                    change: people::PolicyChange::Requirements {
                        requirements: Box::new([people::Requirement {
                            connector: 1,
                            kind: 5,
                            pattern: people::Pattern { segments: Box::new([]), last: people::Last::Open(Box::new([])) },
                            judge: people::Judge { connector: 2, requirement: 9, parameters: 0 },
                            guard: people::Guard::Guarded,
                            must_be_guarded: true,
                        }]),
                    },
                },
            ),
            Action::AcceptProposal => {
                let proposal = world.peers.pending_proposal.expect("waiting proposal");
                ask(
                    world,
                    44,
                    people::Ask::DecideProposal {
                        project: 1,
                        proposer: parent(world).0,
                        proposal,
                        decision: people::ProposalDecision::Accept,
                    },
                );
            }
            Action::StartStanding => {
                let (task, attempt) = parent(world);
                let worker = world
                    .peers
                    .peers
                    .workers
                    .iter_mut()
                    .find(|worker| worker.seen.iter().any(|run| run.task == task))
                    .expect("parent host");
                worker.cancel(task, attempt);
                world.drain()?;
                let mut authority = authority(world);
                authority.budget.spend = 30;
                authority.delegation =
                    tasks::Delegation { kinds: Box::new([tasks::AuthorityExecutor::Charter(1)]), tasks: 1, depth: 1 };
                let mut child = authority.clone();
                child.budget.spend = 10;
                child.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
                world.send(root::Event::Core(core::Event::StartRecurring {
                    project: 1,
                    authority,
                    template: tasks::RecurringTemplate {
                        key: 1,
                        overlap: tasks::RecurringOverlap::Skip,
                        batch: Box::new([tasks::New {
                            number: 1,
                            project: 1,
                            executor: tasks::Executor::Agent { charter: 1 },
                            spec: tasks::Spec {
                                words: b"period work".as_slice().into(),
                                parameters: Box::new([]),
                                inputs: Box::new([]),
                            },
                            contract: tasks::Contract::Report { words: 128 },
                            authority: child,
                            numbers: tasks::Numbers { budget: 10, spent: 0, spent_below: 0, reserved: 0 },
                            funder: tasks::Funder::Period { project: 1, period: 0 },
                            dependencies: Box::new([]),
                            holdings: Box::new([]),
                            wake: tasks::WakePolicy::DEFAULT,
                            recurring: None,
                            tracked: None,
                        }]),
                    },
                }));
            }
            Action::Period(period) => {
                let task = world
                    .store
                    .rows
                    .values()
                    .filter_map(|row| match row {
                        root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))
                            if row.spec.words.as_ref() == b"period work" =>
                        {
                            Some(row.number)
                        }
                        root::Record::Core(_) | root::Record::Connector { .. } => None,
                    })
                    .max()
                    .expect("durable standing batch");
                wake(world, task)?;
                world.drain()?;
                check(ended(world, task), "standing batch did not finish")?;
                let before = world.peers.assignments.len();
                for _ in 0..2 {
                    world.send(root::Event::Core(core::Event::Period { project: 1, period, budget: 1000 }));
                    world.drain()?;
                }
                if world.peers.assignments.len() == before {
                    world.advance(6_000_000_000);
                    world.drain()?;
                    world.advance(1_000_000_000);
                    world.drain()?;
                }
                check(world.peers.assignments.len() == before + 1, "period did not make exactly one batch")?;
            }
            Action::Resume => wake(world, parent(world).0)?,
        }
        Ok(())
    }

    fn check(world: &World, scenario: Scenario) -> Result<(), Violation> {
        let applied = world.systems[0].observed().iter().filter(|effect| effect.applied).count();
        match scenario {
            Scenario::LateCopy => check(
                applied == 1 && world.systems[0].observed().len() == 2,
                "late and retried copies did not make one effect",
            ),
            Scenario::OtherHand => check(
                applied == 0 && world.systems[0].state(&jig_test_connector_world::path(1, 1)) == Some(13),
                "conditional retry overwrote another participant",
            ),
            Scenario::UncertainRestarts => {
                check(applied == 1, "repeated cold starts did not resolve the uncertain entry")
            }
            Scenario::ChangedJudge => check(
                applied == 1
                    && world.peers.calls.iter().any(|part| matches!(part, core::CallPart::EffectDenied { .. })),
                "a new effect ignored the changed judge",
            ),
            Scenario::NarrowingPolicy => check(
                applied == 1
                    && world.peers.pending_proposal.is_some_and(|number| {
                        world.store.rows.contains_key(&root::Key::Connector {
                            number: 1,
                            key: connector::RecordKey::Proposal(number),
                        })
                    }),
                "narrowing changed a committed flight or consumed its waiting proposal",
            ),
            Scenario::ShrinkingPool => check(
                world.peers.assignments.len() == 4 && world.peers.children.iter().all(|task| ended(world, *task)),
                "pool did not retain its holders and eventually serve its waiter",
            ),
            Scenario::StandingTask => {
                check(world.peers.assignments.len() == 6, "standing task did not run across five periods")
            }
            Scenario::RunBudget => {
                let spent: u64 = world.peers.peers.workers.iter().map(|worker| worker.actual_spent).sum();
                check(
                    spent == 20 && world.peers.peers.workers.iter().map(|worker| worker.turn_acks).sum::<u32>() == 12,
                    &format!(
                        "sessions and sub-agents did not consume the one run allowance: spent={spent}, acks={}, assignments={:?}",
                        world.peers.peers.workers.iter().map(|worker| worker.turn_acks).sum::<u32>(),
                        world.peers.assignments
                    ),
                )
            }
        }
    }
}
