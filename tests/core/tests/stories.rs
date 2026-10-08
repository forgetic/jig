use jig_core as core;
use jig_core_authority as authority;
use jig_core_notes as notes;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_world::effects::{World, fixture};
use jig_test_connector as connector;
use jig_test_domain as root;
use skein_lib::{List, Queue, ReplyTo, Token};

fn ask(world: &mut World, request: u8, ask: people::Ask) {
    world.send(root::Event::Core(core::Event::People(people::Event::Ask {
        reply_to: ReplyTo::new(Token::new(6000 + u64::from(request))),
        sign_in: world.sign_in.expect("signed-in person"),
        key: [request; 16],
        ask,
    })));
}

fn action(world: &mut World, completion: u32, action: core::NamedAction) {
    world.send(root::Event::Core(core::Event::NamedAction {
        to: ReplyTo::new(Token::new(7000 + u64::from(completion))),
        key: world.call_key(completion),
        action,
    }));
}

fn running(world: &World, task: u64) -> (u64, u64) {
    *world.assignments.iter().rev().find(|(number, _)| *number == task).expect("task assigned")
}

fn finish(world: &mut World, run: (u64, u64), words: &[u8]) {
    world.send(root::Event::Answer {
        channel: Token::new(7),
        task: run.0,
        attempt: run.1,
        cumulative: 0,
        end: tasks::End::Finished {
            result: tasks::TaskResult::Report { words: words.into() },
            cancel_delegates: false,
        },
    });
}

fn delegate(world: &mut World, run: (u64, u64), completion: u32, member: core::Delegate) -> u64 {
    world.send(root::Event::Core(core::Event::DelegateValidated {
        to: ReplyTo::new(Token::new(9000 + u64::from(completion))),
        key: core::CallKey { task: run.0, attempt: run.1, completion, position: 0 },
        batch: Box::new([member]),
        stubs: Box::new([]),
    }));
    let core::CallPart::Delegated(numbers) = &world.answers.last().expect("delegate answer").2 else {
        panic!("delegation was not admitted");
    };
    numbers[0]
}

fn member(world: &World, executor: tasks::Executor, words: &[u8]) -> core::Delegate {
    let mut authority = world.assigned.as_ref().expect("task assigned").run.authority.clone();
    authority.budget.spend = 20;
    authority.delegation =
        tasks::Delegation { kinds: Box::new([tasks::AuthorityExecutor::Charter(1)]), tasks: 1, depth: 1 };
    core::Delegate {
        executor,
        spec: tasks::Spec { words: words.into(), parameters: Box::new([]), inputs: Box::new([]) },
        contract: tasks::Contract::Report { words: 128 },
        authority,
        symbolic_grants: Box::new([]),
        dependencies: Box::new([]),
        wake: tasks::WakePolicy::DEFAULT,
    }
}

fn plan_world(seed: u64) -> World {
    let (configuration, limits) = jig_core_world::faults::plan_fixture(seed);
    World::configured(seed, false, configuration, limits)
}

#[test]
fn a_plan_of_reports_and_a_procedure_is_revised_midway_and_done() {
    let mut world = plan_world(124);
    let parent = world.assignments[0];
    let mut report = member(&world, tasks::Executor::Agent { charter: 1 }, b"first report");
    report.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    let report = delegate(&mut world, parent, 1, report);
    let report_run = running(&world, report);
    world.send(root::Event::Core(core::Event::NamedAction {
        to: ReplyTo::new(Token::new(9101)),
        key: core::CallKey { task: parent.0, attempt: parent.1, completion: 2, position: 0 },
        action: core::NamedAction::Amend {
            target: report,
            amendment: tasks::Amendment {
                spec: Some(tasks::Spec {
                    words: b"revised report".as_slice().into(),
                    parameters: Box::new([]),
                    inputs: Box::new([]),
                }),
                wake: None,
                dependencies: None,
                authority: None,
                reason: b"new evidence".as_slice().into(),
            },
        },
    }));
    assert!(world.inbound.iter().any(
        |word| matches!(word.kind, tasks::MessageKind::Amendment { .. }) && word.words.as_ref() == b"new evidence"
    ));
    finish(&mut world, report_run, b"revised result");
    let mut second = member(&world, tasks::Executor::Agent { charter: 1 }, b"second report");
    second.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    let second = delegate(&mut world, parent, 3, second);
    let second_run = running(&world, second);
    finish(&mut world, second_run, b"alternative result");
    let mut choice = member(&world, tasks::Executor::Agent { charter: 1 }, b"choose between the reports");
    choice.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    choice.contract =
        tasks::Contract::Verdict { choices: Box::new([tasks::Verdict { code: 1, words: 128, followups: 0 }]) };
    let choice = delegate(&mut world, parent, 4, choice);
    let choice_run = running(&world, choice);
    world.send(root::Event::Answer {
        channel: Token::new(7),
        task: choice_run.0,
        attempt: choice_run.1,
        cumulative: 0,
        end: tasks::End::Finished {
            result: tasks::TaskResult::Verdict { code: 1, words: b"choose the revised report".as_slice().into() },
            cancel_delegates: false,
        },
    });
    let mut procedure = member(&world, tasks::Executor::Procedure { connector: 1, code: 1 }, b"carry out the choice");
    procedure.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    let procedure = delegate(&mut world, parent, 5, procedure);
    assert!(world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(procedure)))));
    finish(&mut world, parent, b"plan done");
    for task in [report, second, choice, procedure, parent.0] {
        assert!(world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(task)))));
    }
    assert!(world.store.rows.values().any(|row| matches!(row, root::Record::Core(core::Record::Tasks(tasks::Stored::History(history))) if history.task==report && history.change==tasks::Change::Amended)));
}

#[test]
fn a_recurring_task_creates_one_batch_per_period_after_the_reset() {
    let mut world = plan_world(125);
    world.end(tasks::End::Parked, 0);
    let mut authority = member(&world, tasks::Executor::Agent { charter: 1 }, b"template").authority;
    authority.budget.spend = 30;
    let mut member_authority = authority.clone();
    member_authority.budget.spend = 10;
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
                authority: member_authority,
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
    let first = *world.assignments.last().expect("first period run");
    assert_ne!(first.0, world.assignments[0].0);
    finish(&mut world, first, b"first period done");
    world.send(root::Event::Core(core::Event::Period { project: 1, period: 2, budget: 1000 }));
    let second = *world.assignments.last().expect("second period run");
    assert_ne!(second.0, first.0);
    assert!(
        matches!(world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(second.0)))), Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) if matches!(row.funder, tasks::Funder::Recurring { period: 2, .. }))
    );
    let count = world.assignments.len();
    world.send(root::Event::Core(core::Event::Period { project: 1, period: 2, budget: 1000 }));
    assert_eq!(world.assignments.len(), count);
}

#[test]
fn a_policy_narrowed_while_a_proposal_waits_keeps_the_committed_entry_and_checks_the_live_run() {
    let (configuration, mut limits) = fixture(126, true);
    limits.core.call_records = 8;
    limits.journal.writes = 10_000;
    limits.journal.held = 10_000;
    let mut world = World::configured(126, true, configuration, limits);
    world.call(1, 9200);
    world.observed(13);
    world.hold_writes = true;
    world.call(1, 9201);
    assert_eq!(world.pending_writes.len(), 1);
    let proposal = world.propose(2);
    let run = world.assignments[0];
    ask(
        &mut world,
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
    );
    assert!(
        world
            .people_answers
            .iter()
            .any(|reply| matches!(reply, people::Reply::Outcome(people::Outcome::PolicyChanged { project: 1 })))
    );
    world.release_writes();
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
    world.decide_proposal(proposal, true, 44);
    assert!(
        world
            .store
            .rows
            .contains_key(&root::Key::Connector { number: 1, key: connector::RecordKey::Proposal(proposal) })
    );
    world.call(3, 9203);
    assert!(matches!(
        world.answers.last(),
        Some((_, _, core::CallPart::EffectDenied { answer: authority::Answer::Refuse, .. }))
    ));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
    assert!(world.cancellations.is_empty());
    assert_eq!(*world.assignments.last().expect("run remains live"), run);
    assert_eq!(world.spent(), 3);
}

#[test]
fn drift_holds_a_task_and_the_client_reads_why_it_needs_a_decision() {
    let mut world = World::new(127, false);
    let task = world.call_key(1).task;
    let resource = jig_test_connector_world::path(1, 1);
    world.send(root::Event::Connector {
        number: 1,
        event: connector::Event::Adopt { project: 1, resource: resource.clone(), role: connector::ResourceRole::Owned },
    });
    world.send(root::Event::Connector {
        number: 1,
        event: connector::Event::Names { task, project: 1, resources: Box::new([resource.clone()]) },
    });
    for state in [10, 11] {
        world.systems[0].other_hand(&resource, state);
        world.send(root::Event::Connector {
            number: 1,
            event: connector::Event::System(connector::SystemEvent::Fact {
                resource: resource.clone(),
                fact: connector::Fact { state: Some(state), observed: world.wall_time(), pending: false },
                origin: connector::Origin::Other,
            }),
        });
    }
    assert_eq!(world.domain.escalation(1, task).expect("client held-task context").why, tasks::Hold::Drift);
    assert!(world.domain.escalation(99, task).is_none(), "another party cannot read the held reason");
    assert!(
        matches!(world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))), Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) if matches!(row.phase, tasks::Phase::Held { why: tasks::Hold::Drift, .. }))
    );
    assert_eq!(world.cancellations, [world.assignments[0]]);
}

#[test]
fn a_goal_cancelled_with_live_runs_and_an_effect_in_flight_closes_deepest_first() {
    let mut world = plan_world(128);
    let parent = world.assignments[0];
    let mut child = member(&world, tasks::Executor::Agent { charter: 1 }, b"child");
    child.authority.budget.spend = 40;
    let child = delegate(&mut world, parent, 1, child);
    let child_run = running(&world, child);
    let mut grandchild = member(&world, tasks::Executor::Agent { charter: 1 }, b"grandchild");
    grandchild.authority.budget.spend = 15;
    grandchild.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    let grandchild = delegate(&mut world, child_run, 1, grandchild);
    let grandchild_run = running(&world, grandchild);
    world.hold_writes = true;
    world.call(1, 9301);
    assert_eq!(world.pending_writes.len(), 1, "the effect is committed and in flight");
    assert_eq!(world.call_key(1).task, grandchild);
    ask(&mut world, 45, people::Ask::Cancel { project: 1, task: child, reason: b"goal withdrawn".as_slice().into() });
    assert_eq!(world.cancellations, [grandchild_run, child_run]);
    for run in [grandchild_run, child_run] {
        world.send(root::Event::Answer {
            channel: Token::new(7),
            task: run.0,
            attempt: run.1,
            cumulative: 0,
            end: tasks::End::Parked,
        });
    }
    assert!(!world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(grandchild)))));
    assert!(!world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(child)))));
    world.release_writes();
    let ended: Vec<_> = world
        .commits
        .iter()
        .flatten()
        .filter_map(|write| match write {
            root::Write::Save(root::Record::Core(core::Record::Tasks(tasks::Stored::Ended(row)))) => Some(row.number),
            root::Write::Save(_) | root::Write::Erase(_) => None,
        })
        .collect();
    assert_eq!(ended, [grandchild, child]);
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
}

#[test]
fn a_note_written_corrected_by_a_person_and_recalled_keeps_the_corrected_body() {
    let (mut configuration, limits) = fixture(121, false);
    let mut rules = configuration.core.authority.rules().clone();
    rules.ceiling.notes = authority::Scopes::PROJECT;
    let mut policy = configuration.core.authority.policy(1).expect("project policy").clone();
    policy.ceiling.notes = authority::Scopes::PROJECT;
    policy.roles[0].authority.notes = authority::Scopes::PROJECT;
    let mut checked = authority::Domain::new(rules, limits.core.authority).expect("bounded note policy");
    let mut out = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(&mut checked, authority::Event::Policy { project: 1, policy }, &mut out);
    assert_eq!(out.pop(), Some(authority::PolicyFact::Added { project: 1 }));
    configuration.core.authority = checked;
    configuration.core.settings.chat_authority.notes = authority::Scopes::PROJECT;
    let mut world = World::configured(121, false, configuration, limits);
    let key = world.call_key(1);
    action(
        &mut world,
        1,
        core::NamedAction::Note {
            entry: notes::New {
                name: 0,
                scope: notes::Scope::Project { project: 1 },
                description: b"original".as_slice().into(),
                body: b"first body".as_slice().into(),
                references: List::with_capacity(1),
                author: notes::Author::Task { task: key.task, attempt: key.attempt },
            },
            recalled: None,
        },
    );
    let core::CallPart::NoteWritten { name, revision: 1 } = world.answers.last().expect("written answer").2 else {
        panic!("note write was not admitted");
    };
    ask(
        &mut world,
        41,
        people::Ask::EditNote {
            project: 1,
            name,
            scope: Box::new(people::NoteScope::Project),
            change: Box::new(people::NoteChange::Correct {
                description: b"corrected".as_slice().into(),
                body: b"second body".as_slice().into(),
                references: Box::new([]),
                recalled: 1,
            }),
        },
    );
    assert!(world.people_answers.iter().any(
        |reply| matches!(reply, people::Reply::Outcome(people::Outcome::NoteEdited { name: found }) if *found==name)
    ));
    action(&mut world, 2, core::NamedAction::Recall { by: notes::Recall::Name { name }, page: 0 });
    assert!(
        matches!(world.answers.last(), Some((_, _, core::CallPart::NoteRecalled { entries, more: false })) if entries.len()==1 && entries[0].revision==2 && entries[0].body.as_ref()==b"second body")
    );
    assert!(
        matches!(world.store.rows.get(&root::Key::Core(core::Key::Notes(notes::Key::Entry { name }))), Some(root::Record::Core(core::Record::Notes(notes::Record::Entry(entry)))) if entry.revision==2 && entry.author==notes::Author::Party { party: 1 })
    );
}

#[test]
fn a_persons_whole_words_reach_a_running_run_and_retire_only_with_its_committed_read() {
    let mut world = World::new(122, false);
    let message = world.say(42);
    let inbound = world.inbound.last().expect("running run received words");
    assert_eq!(inbound.number, message);
    assert_eq!(inbound.words.as_ref(), [42]);
    assert!(matches!(inbound.kind, tasks::MessageKind::Words));
    let task = world.call_key(1).task;
    assert!(
        matches!(world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))), Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) if row.inbox.iter().any(|word| word.number==message))
    );
    world.turn(1, 1, Some(message), b"read the person's complete words");
    assert!(
        matches!(world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))), Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) if row.inbox.is_empty())
    );
    assert!(world.store.rows.values().any(|row| matches!(row, root::Record::Core(core::Record::Core(core::CoreRecord::Turn(turn))) if turn.read==Some(message))));
}

#[test]
fn a_judges_observed_facts_may_change_after_a_verdict_but_new_effects_use_the_new_facts() {
    let mut world = World::new(123, true);
    world.call(1, 8000);
    world.observed(13);
    world.hold_writes = true;
    world.call(1, 8001);
    assert_eq!(world.pending_writes.len(), 1);
    let committed = world.store.applied;
    world.systems[1].other_hand(&jig_test_connector_world::path(1, 1), 14);
    world.send(root::Event::Connector {
        number: 2,
        event: connector::Event::System(connector::SystemEvent::Fact {
            resource: jig_test_connector_world::path(1, 1),
            fact: connector::Fact { state: Some(14), observed: world.wall_time(), pending: false },
            origin: connector::Origin::Other,
        }),
    });
    assert_eq!(world.store.applied, committed, "a new observation does not rewrite the committed entry");
    world.release_writes();
    assert_eq!(world.systems[0].observed().iter().filter(|entry| entry.applied).count(), 1);
    world.call(2, 8002);
    assert!(matches!(
        world.answers.last(),
        Some((_, _, core::CallPart::EffectDenied { answer: authority::Answer::Refuse, .. }))
    ));
    assert_eq!(world.systems[0].observed().iter().filter(|entry| entry.applied).count(), 1);
    assert_eq!(world.spent(), 3);
}

#[test]
fn an_exhausted_delegate_escalates_to_a_person_who_releases_it_to_finish() {
    let mut world = plan_world(128);
    let parent = world.assignments[0];
    let mut child = member(&world, tasks::Executor::Agent { charter: 1 }, b"retry this delegate");
    child.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    let child = delegate(&mut world, parent, 1, child);
    for index in 0..2 {
        let run = running(&world, child);
        world.send(root::Event::Answer {
            channel: Token::new(7),
            task: run.0,
            attempt: run.1,
            cumulative: 0,
            end: tasks::End::Failed(tasks::Class::Run),
        });
        if index == 0 {
            world.advance(skein_lib::Duration::from_secs(2));
            assert!(running(&world, child).1 > run.1);
        }
    }
    assert!(matches!(world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(child)))),
        Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))))
            if matches!(row.phase, tasks::Phase::Held { why: tasks::Hold::Failures(tasks::Class::Run), .. })));
    // The parent receives the escalation first, then passes its current revision
    // to the person. The person decides through their authenticated client face.
    let revision = match world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(child)))) {
        Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) => match row.escalation {
            tasks::Escalation::Waiting { revision, .. } => revision,
            tasks::Escalation::Unheld { .. }
            | tasks::Escalation::Routing { .. }
            | tasks::Escalation::Rejected { .. } => panic!("escalation not routed"),
        },
        row => panic!("held child missing: {row:?}"),
    };
    world.send(root::Event::Core(core::Event::NamedAction {
        to: ReplyTo::new(Token::new(9300)),
        key: core::CallKey { task: parent.0, attempt: parent.1, completion: 2, position: 0 },
        action: core::NamedAction::DecideEscalation { task: child, revision, choice: core::EscalationChoice::Pass },
    }));
    let revision = match world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(child)))) {
        Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) => match row.escalation {
            tasks::Escalation::Waiting { revision, holder: tasks::EscalationHolder::Person(_), .. } => revision,
            ref escalation @ (tasks::Escalation::Unheld { .. }
            | tasks::Escalation::Routing { .. }
            | tasks::Escalation::Waiting { .. }
            | tasks::Escalation::Rejected { .. }) => panic!("not passed to a person: {escalation:?}"),
        },
        row => panic!("held child missing: {row:?}"),
    };
    ask(
        &mut world,
        44,
        people::Ask::DecideEscalation {
            project: 1,
            task: child,
            revision,
            decision: people::EscalationDecision::Release,
        },
    );
    assert!(
        world
            .people_answers
            .iter()
            .any(|reply| matches!(reply, people::Reply::Outcome(people::Outcome::EscalationDecided { .. })))
    );
    let released = running(&world, child);
    finish(&mut world, released, b"delegate recovered");
    finish(&mut world, parent, b"parent done");
    for task in [child, parent.0] {
        assert!(world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(task)))));
    }
}

fn resource_member(world: &World, id: u64, words: &[u8]) -> core::Delegate {
    let mut child = member(world, tasks::Executor::Agent { charter: 1 }, words);
    child.spec.parameters = Box::new([tasks::Parameter::Resource { name: 1, connector: 1, resource: id }]);
    child.authority.budget.spend = 10;
    child.authority.delegation = tasks::Delegation { kinds: Box::new([]), tasks: 0, depth: 0 };
    child
}

fn configure_resources(world: &mut World) {
    world.configure_holds(
        1,
        Box::new([
            tasks::Kind { connector: 1, kind: 1, hold: tasks::HoldKind::Exclusive { taken: tasks::Taken::Waits } },
            tasks::Kind { connector: 1, kind: 2, hold: tasks::HoldKind::Pooled { taken: tasks::Taken::Waits } },
        ]),
    );
}

fn pool_size(world: &mut World, slots: u32) {
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
fn two_tasks_waiting_for_the_last_pool_slot_take_it_one_at_a_time() {
    let mut world = plan_world(129);
    configure_resources(&mut world);
    pool_size(&mut world, 1);
    let parent = world.assignments[0];
    let mut children = Vec::new();
    for completion in 1..=3 {
        let spec = resource_member(&world, 2, b"pool work");
        children.push(delegate(&mut world, parent, completion, spec));
    }
    assert_eq!(world.assignments.len(), 2, "one pool holder and the parent");
    for (index, child) in children.iter().enumerate() {
        let run = running(&world, *child);
        assert_eq!(world.assignments.len(), index + 2);
        finish(&mut world, run, b"pool slot released");
    }
    finish(&mut world, parent, b"all pool work done");
}

#[test]
fn a_shrinking_pool_keeps_its_holders_and_the_waiter_waits_until_it_drains() {
    let mut world = plan_world(130);
    configure_resources(&mut world);
    pool_size(&mut world, 2);
    let parent = world.assignments[0];
    let mut children = Vec::new();
    for completion in 1..=3 {
        let spec = resource_member(&world, 2, b"pool work");
        children.push(delegate(&mut world, parent, completion, spec));
    }
    assert_eq!(world.assignments.len(), 3);
    pool_size(&mut world, 1);
    let first = running(&world, children[0]);
    finish(&mut world, first, b"first done");
    assert_eq!(world.assignments.len(), 3, "the second holder still occupies the sole remaining slot");
    let second = running(&world, children[1]);
    finish(&mut world, second, b"second done");
    assert_eq!(world.assignments.len(), 4);
    let third = running(&world, children[2]);
    finish(&mut world, third, b"waiter done");
    finish(&mut world, parent, b"pool drained");
}

#[test]
fn a_run_hands_a_resource_to_a_procedure_while_retaining_its_writer_slot() {
    let mut world = plan_world(131);
    configure_resources(&mut world);
    let parent = world.assignments[0];
    let mut holder = resource_member(&world, 1, b"hold the write resource");
    holder.authority.budget.spend = 20;
    holder.authority.delegation =
        tasks::Delegation { kinds: Box::new([tasks::AuthorityExecutor::Procedure(1)]), tasks: 1, depth: 1 };
    let holder = delegate(&mut world, parent, 1, holder);
    let holder_run = running(&world, holder);
    let mut procedure = resource_member(&world, 1, b"carry out the write");
    procedure.executor = tasks::Executor::Procedure { connector: 1, code: 1 };
    procedure.authority.budget.spend = 5;
    let procedure = delegate(&mut world, holder_run, 1, procedure);
    let row = |task| match world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(task)))) {
        Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) => row,
        row => panic!("live task missing: {row:?}"),
    };
    assert!(row(holder).holdings.is_empty(), "the task hold was handed to the procedure");
    assert!(row(procedure).holds_taken);
    assert!(world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Tasks(tasks::Stored::Writer(slot)))
            if slot.writer == tasks::Writer::Run { task: holder, attempt: holder_run.1 })));
    assert!(world.systems[0].observed().is_empty(), "the procedure's effect waits for the writer");
    world.send(root::Event::Answer {
        channel: Token::new(7),
        task: holder,
        attempt: holder_run.1,
        cumulative: 0,
        end: tasks::End::Parked,
    });
    world.advance(skein_lib::Duration::from_secs(2));
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
    assert!(world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(procedure)))));
    let holder_run = running(&world, holder);
    finish(&mut world, holder_run, b"handoff done");
    finish(&mut world, parent, b"parent done");
}

#[test]
fn a_lost_writer_is_read_afresh_only_after_its_loss_is_committed() {
    let mut world = plan_world(132);
    configure_resources(&mut world);
    let parent = world.assignments[0];
    let spec = resource_member(&world, 1, b"writer that disappears");
    let child = delegate(&mut world, parent, 1, spec);
    let run = running(&world, child);
    let reads = world.systems[0].reads().len();
    world.store.fault(jig_fake_store::Fault::Hold { commits: 1 });
    world.send(root::Event::Core(core::Event::Fleet(jig_core_fleet::Event::Lost { channel: Token::new(7) })));
    world.advance(skein_lib::Duration::from_secs(6));
    assert_eq!(world.systems[0].reads().len(), reads, "fresh reads wait for the loss commit's completion");
    world.release_commits();
    world.advance(skein_lib::Duration::from_secs(1));
    assert_eq!(world.systems[0].reads().len(), reads + 1);
    assert!(!world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Tasks(tasks::Stored::Writer(slot)))
            if slot.writer == tasks::Writer::Run { task: child, attempt: run.1 })));
}

#[test]
fn a_procedures_effect_writer_survives_a_cold_restart_and_frees_after_settlement() {
    let mut world = plan_world(133);
    configure_resources(&mut world);
    let parent = world.assignments[0];
    let mut procedure = resource_member(&world, 1, b"durable write flight");
    procedure.executor = tasks::Executor::Procedure { connector: 1, code: 1 };
    procedure.authority.budget.spend = 5;
    world.hold_writes = true;
    let procedure = delegate(&mut world, parent, 1, procedure);
    assert_eq!(world.pending_writes.len(), 1);
    assert!(world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Tasks(tasks::Stored::Writer(slot)))
            if matches!(slot.writer, tasks::Writer::Effect { .. }))));
    world.restart_with(jig_core_world::faults::plan_fixture(133).0);
    world.release_writes();
    assert!(world.domain.ready(), "restart trace: {:#?}", world.trace.iter().rev().take(16).collect::<Vec<_>>());
    world.advance(skein_lib::Duration::from_secs(2));
    world.send(root::Event::ConnectorTimer { number: 1 });
    assert_eq!(world.systems[0].observed().iter().filter(|row| row.applied).count(), 1);
    assert!(!world.store.rows.values().any(|row| matches!(row,
        root::Record::Core(core::Record::Tasks(tasks::Stored::Writer(slot)))
            if matches!(slot.writer, tasks::Writer::Effect { .. }))));
    assert!(world.store.rows.contains_key(&root::Key::Core(core::Key::Tasks(tasks::Key::Ended(procedure)))));
}

#[test]
fn an_exclusive_resource_that_refuses_waiting_refuses_the_second_batch() {
    let seed = 151;
    let (mut configuration, limits) = jig_core_world::faults::plan_fixture(seed);
    configuration.first.resources[0].hold = connector::Hold::Exclusive { mode: connector::HoldMode::Refuse };
    let mut world = World::configured(seed, false, configuration, limits);
    // The kind default waits: the individual connector report must override it.
    configure_resources(&mut world);
    let parent = world.assignments[0];
    let child = resource_member(&world, 1, b"first holder");
    let first = delegate(&mut world, parent, 1, child);
    let before = world.assignments.len();
    let child = resource_member(&world, 1, b"second holder");
    world.send(root::Event::Core(core::Event::DelegateValidated {
        to: ReplyTo::new(Token::new(9152)),
        key: core::CallKey { task: parent.0, attempt: parent.1, completion: 2, position: 0 },
        batch: Box::new([child]),
        stubs: Box::new([]),
    }));
    assert_eq!(world.assignments.len(), before);
    assert!(matches!(
        world.answers.last().expect("second batch answer").2,
        core::CallPart::DelegationRefused(ref problem) if problem.why == tasks::Refusal::HoldTaken && problem.blocked_by.as_deref() == Some(&[first])
    ));
    let run = running(&world, first);
    finish(&mut world, run, b"first done");
    finish(&mut world, parent, b"exclusive work done");
}

#[test]
fn a_batch_waits_without_creating_a_task_or_reserving_funding_until_its_resource_is_reported() {
    let mut world = plan_world(152);
    let parent = world.assignments[0];
    let resource = jig_test_connector_world::path(1, 1);
    world.send(root::Event::Connector {
        number: 1,
        event: connector::Event::Adopt { project: 1, resource: resource.clone(), role: connector::ResourceRole::Owned },
    });
    let before = match world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(parent.0)))) {
        Some(root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row)))) => row.numbers,
        row => panic!("parent row: {row:?}"),
    };
    let member = resource_member(&world, 1, b"wait for its report");
    world.send(root::Event::Core(core::Event::Tasks(tasks::Event::Make {
        reply_to: ReplyTo::new(Token::new(u64::MAX - 3)),
        creator: tasks::Party::Task(parent.0),
        batch: Box::new([tasks::New {
            number: 100,
            project: 1,
            executor: member.executor,
            spec: member.spec,
            contract: member.contract,
            numbers: tasks::Numbers { budget: 10, spent: 0, spent_below: 0, reserved: 0 },
            authority: member.authority,
            funder: tasks::Funder::Task(parent.0),
            dependencies: Box::new([]),
            holdings: Box::new([tasks::Holding::Write {
                resource: tasks::Name { connector: 1, path: resource.segments().into() },
                kind: 1,
            }]),
            wake: tasks::WakePolicy::DEFAULT,
            recurring: None,
            tracked: None,
        }]),
    })));
    let key = root::Key::Core(core::Key::Tasks(tasks::Key::Live(100)));
    assert!(!world.store.rows.contains_key(&key));
    assert_eq!(world.assignments.len(), 1);
    let row = world.store.rows.get(&root::Key::Core(core::Key::Tasks(tasks::Key::Live(parent.0)))).expect("parent row");
    assert!(matches!(row, root::Record::Core(core::Record::Tasks(tasks::Stored::Live(row))) if row.numbers == before));
    world.send(root::Event::Connector {
        number: 1,
        event: connector::Event::Names { task: 100, project: 1, resources: Box::new([resource]) },
    });
    assert!(world.store.rows.contains_key(&key), "a connector report resumes the whole batch");
    assert_eq!(world.assignments.len(), 2);
    let run = running(&world, 100);
    finish(&mut world, run, b"reported work done");
    finish(&mut world, parent, b"done");
}
