use jig_core as core;
use jig_core_tasks as tasks;
use jig_core_world::{effects::World, faults::plan_fixture};
use jig_test_connector as connector;
use jig_test_domain as root;
use skein_lib::{ReplyTo, Rng, Token};

fn finish(world: &mut World, task: u64) {
    let run = *world.assignments.iter().rev().find(|(number, _)| *number == task).expect("assigned task");
    world.send(root::Event::Answer {
        channel: Token::new(7),
        task,
        attempt: run.1,
        cumulative: 0,
        end: tasks::End::Finished {
            result: tasks::TaskResult::Report { words: b"done".as_slice().into() },
            cancel_delegates: false,
        },
    });
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

#[test]
fn random_pool_sizes_shrink_and_grow_while_finishes_wait_for_their_commits() {
    for seed in 161..=192 {
        let replay = std::panic::catch_unwind(|| {
            let mut rng = Rng::new(seed);
            let (config, limits) = plan_fixture(seed);
            let mut world = World::configured(seed, false, config, limits);
            world.configure_holds(
                1,
                Box::new([tasks::Kind {
                    connector: 1,
                    kind: 2,
                    hold: tasks::HoldKind::Pooled { taken: tasks::Taken::Waits },
                }]),
            );
            world.send(root::Event::Connector {
                number: 1,
                event: connector::Event::Adopt {
                    project: 1,
                    resource: jig_test_connector_world::path(1, 2),
                    role: connector::ResourceRole::Owned,
                },
            });
            let initial = u32::try_from(1 + rng.below(2)).expect("small pool");
            pool(&mut world, initial);
            let parent = world.assignments[0];
            let mut children = Vec::new();
            for completion in 1..=3 {
                let mut authority = world.assigned.as_ref().expect("parent").run.authority.clone();
                authority.budget.spend = 10 + rng.below(8);
                authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
                world.send(root::Event::Core(core::Event::DelegateValidated {
                    to: ReplyTo::new(Token::new(9000 + u64::from(completion))),
                    key: core::CallKey { task: parent.0, attempt: parent.1, completion, position: 0 },
                    batch: Box::new([core::Delegate {
                        executor: tasks::Executor::Agent { charter: 1 },
                        spec: tasks::Spec {
                            words: b"pool work".as_slice().into(),
                            parameters: Box::new([tasks::Parameter::Resource { name: 1, connector: 1, resource: 2 }]),
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
                let core::CallPart::Delegated(numbers) = &world.answers.last().expect("admitted delegate").2 else {
                    panic!("delegate not admitted");
                };
                children.push(numbers[0]);
            }
            assert_eq!(world.assignments.len(), 1 + usize::try_from(initial).expect("small pool"));
            pool(&mut world, 0);
            for child in children.iter().take(usize::try_from(initial).expect("small pool")) {
                world.store.fault(jig_fake_store::Fault::Hold { commits: 1 });
                finish(&mut world, *child);
                assert_eq!(world.assignments.len(), 1 + usize::try_from(initial).expect("small pool"));
                world.release_commits();
            }
            pool(&mut world, 1);
            for child in children.iter().skip(usize::try_from(initial).expect("small pool")) {
                finish(&mut world, *child);
            }
            finish(&mut world, parent.0);
            assert!(children.iter().all(|number| {
                world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(*number))))
            }));
        });
        assert!(replay.is_ok(), "replay pool seed {seed}");
    }
}

#[test]
fn random_period_budgets_reset_before_a_standing_template_creates_its_next_batch() {
    for seed in 193..=208 {
        let replay = std::panic::catch_unwind(|| {
            let mut rng = Rng::new(seed);
            let (config, limits) = plan_fixture(seed);
            let mut world = World::configured(seed, false, config, limits);
            world.end(tasks::End::Parked, 0);
            let mut authority = world.assigned.as_ref().expect("chat").run.authority.clone();
            authority.budget.spend = 30;
            authority.delegation =
                tasks::Delegation { kinds: Box::new([tasks::AuthorityExecutor::Charter(1)]), tasks: 1, depth: 1 };
            let mut member_authority = authority.clone();
            member_authority.budget.spend = 10 + rng.below(10);
            member_authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
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
                        numbers: tasks::Numbers {
                            budget: member_authority.budget.spend,
                            spent: 0,
                            spent_below: 0,
                            reserved: 0,
                        },
                        authority: member_authority,
                        funder: tasks::Funder::Period { project: 1, period: 0 },
                        dependencies: Box::new([]),
                        holdings: Box::new([]),
                        wake: tasks::WakePolicy::DEFAULT,
                        recurring: None,
                        tracked: None,
                    }]),
                },
            }));
            let first = world.assignments.last().expect("first batch").0;
            finish(&mut world, first);
            for period in 2..=3 {
                let count = world.assignments.len();
                world.store.fault(jig_fake_store::Fault::Hold { commits: 1 });
                world.send(root::Event::Core(core::Event::Period { project: 1, period, budget: 140 }));
                assert_eq!(world.assignments.len(), count, "a new batch waits for the reset commit");
                world.release_commits();
                assert_eq!(world.assignments.len(), count + 1);
                let task = world.assignments.last().expect("period batch").0;
                assert!(matches!(world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))),
                    Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))))
                        if matches!(row.funder, tasks::Funder::Recurring { period: funded, .. } if funded == period)));
                world.send(root::Event::Core(core::Event::Period { project: 1, period, budget: 140 }));
                assert_eq!(world.assignments.len(), count + 1);
                finish(&mut world, task);
            }
        });
        assert!(replay.is_ok(), "replay recurring seed {seed}");
    }
}

#[test]
fn random_policy_changes_leave_a_committed_flight_whole_and_recheck_waiting_proposals() {
    for seed in 209..=224 {
        let replay = std::panic::catch_unwind(|| {
            let mut rng = Rng::new(seed);
            let (config, mut limits) = jig_core_world::effects::fixture(seed, true);
            limits.core.call_records = 8;
            limits.journal.writes = 10_000;
            limits.journal.held = 10_000;
            let mut world = World::configured(seed, true, config, limits);
            world.call(1, 9200);
            world.observed(13);
            world.hold_writes = true;
            world.call(1, 9201);
            let proposal = world.propose(2);
            world.send(root::Event::Core(core::Event::People(jig_core_people::Event::Ask {
                reply_to: ReplyTo::new(Token::new(9400)),
                sign_in: world.sign_in.expect("person"),
                key: [43; 16],
                ask: jig_core_people::Ask::ChangePolicy {
                    project: 1,
                    change: jig_core_people::PolicyChange::Requirements {
                        requirements: Box::new([jig_core_people::Requirement {
                            connector: 1,
                            kind: 5,
                            pattern: jig_core_people::Pattern {
                                segments: Box::new([]),
                                last: jig_core_people::Last::Open(Box::new([])),
                            },
                            judge: jig_core_people::Judge {
                                connector: 2,
                                requirement: 9 + u16::try_from(rng.below(4)).expect("requirement"),
                                parameters: 0,
                            },
                            guard: jig_core_people::Guard::Guarded,
                            must_be_guarded: true,
                        }]),
                    },
                },
            })));
            world.release_writes();
            world.decide_proposal(proposal, true, 44);
            world.call(3, 9203);
            assert!(matches!(
                world.answers.last(),
                Some((_, _, core::CallPart::EffectDenied { answer: jig_core_authority::Answer::Refuse, .. }))
            ));
            assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
            assert_eq!(world.spent(), 3);
        });
        assert!(replay.is_ok(), "replay policy seed {seed}");
    }
}
