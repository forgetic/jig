use jig_core as core;
use jig_core_authority as authority;
use jig_core_tasks as tasks;
use jig_core_world::effects::{World, fixture};
use jig_test_domain as root;
use skein_lib::{Env, Queue, ReplyTo, Time, Token};

fn recovered(world: &World, seed: u64, requirement: bool) -> (core::Core, Env<core::Limits>) {
    let (configuration, limits) = fixture(seed, requirement);
    let env = Env { now: Time::from_nanos(world.wall_time().as_nanos()), wall: world.wall_time(), limits: limits.core };
    let mut core = core::Core::new(configuration.core, &env.limits);
    for row in world.store.rows.values() {
        if let root::Record::Core(core::Record::Core(row @ core::CoreRecord::Deployment(_))) = row {
            assert!(matches!(core.restore_core(row.clone(), &env.limits), core::Restored::Deployment { .. }));
        }
    }
    for row in world.store.rows.values() {
        if let root::Record::Core(core::Record::Tasks(row)) = row {
            if matches!(row, tasks::Stored::Ended(_) | tasks::Stored::Milestone(_) | tasks::Stored::History(_)) {
                continue;
            }
            assert!(core.restore_task_row(row));
            let _requests =
                core::step(&mut core, &env, core::Event::Tasks(tasks::Event::Restore { record: row.clone() }));
        }
    }
    for row in world.store.rows.values() {
        if let root::Record::Core(core::Record::Core(
            row @ (core::CoreRecord::RunProof(_) | core::CoreRecord::Call(_)),
        )) = row
        {
            assert_eq!(core.restore_core(row.clone(), &env.limits), core::Restored::Live);
        }
    }
    let _requests = core::step(&mut core, &env, core::Event::Tasks(tasks::Event::Restored));
    (core, env)
}

fn description() -> core::connector::EffectDescription {
    let mut state = [0; 32];
    state[31] = 13;
    core::connector::EffectDescription {
        connector: 1,
        purpose: 19,
        form: core::connector::EffectForm::Set,
        recovery: core::connector::Recovery::Keyed,
        effect: authority::Effect {
            connector: 1,
            kind: 5,
            name: authority::Name { segments: Box::new([Box::from([1]), Box::from([1])]) },
            state,
            price: Some(3),
            access: authority::EffectAccess::Owned,
            additional: Box::new([]),
            guards: Box::new([]),
        },
    }
}

fn begin(core: &mut core::Core, env: &Env<core::Limits>, origin: core::EffectOrigin) -> authority::Judge {
    let owner = Token::new(9900);
    let _requests = core::step(core, env, core::Event::EffectStart { owner, connector: 1, origin });
    let core::Requests::Out(mut requests) = core::step(
        core,
        env,
        core::Event::EffectConnector(core::connector::Event::Described { owner, description: Box::new(description()) }),
    );
    for _ in 0..requests.len() {
        if let core::Request::Ask { ask: core::Ask::Effect(core::connector::Ask::Judge { judge, .. }), .. } =
            requests.pop().expect("request count")
        {
            return judge;
        }
    }
    panic!("the described effect asks its external judge");
}

fn verdict(core: &mut core::Core, env: &Env<core::Limits>, judge: authority::Judge) -> Vec<core::Request> {
    let core::Requests::Out(mut requests) = core::step(
        core,
        env,
        core::Event::EffectConnector(core::connector::Event::Verdict {
            owner: Token::new(9900),
            judge,
            verdict: authority::Verdict::Met,
            at: env.wall,
            guarded: false,
            state: description().effect.state,
        }),
    );
    let mut outputs = Vec::new();
    for _ in 0..requests.len() {
        outputs.push(requests.pop().expect("request count"));
    }
    outputs
}

#[test]
fn a_delayed_judge_cannot_keep_or_charge_an_effect_after_its_calling_run_is_cancelled() {
    let world = World::new(129, true);
    let key = world.call_key(1);
    let (mut core, env) = recovered(&world, 129, true);
    let judge = begin(
        &mut core,
        &env,
        core::EffectOrigin::Call { to: ReplyTo::new(Token::new(9901)), key, deadline: env.wall },
    );
    let _requests = core::step(
        &mut core,
        &env,
        core::Event::Tasks(tasks::Event::Control {
            reply_to: ReplyTo::new(Token::new(9902)),
            by: tasks::Party::Person(1),
            task: key.task,
            control: tasks::Control::Cancel { reason: b"cancel while the judge reads".as_slice().into() },
        }),
    );
    let before = core.tasks.task(key.task).expect("closing task").numbers;
    let outputs = verdict(&mut core, &env, judge);
    assert!(outputs.iter().any(|request| matches!(
        request,
        core::Request::Ask { ask: core::Ask::Effect(core::connector::Ask::Drop { .. }), .. }
    )));
    assert!(!outputs.iter().any(|request| matches!(
        request,
        core::Request::Write(_)
            | core::Request::Held(_)
            | core::Request::Ask { ask: core::Ask::Effect(core::connector::Ask::Keep { .. }), .. }
    )));
    assert_eq!(core.tasks.task(key.task).expect("closing task").numbers, before);
    assert!(!core.pending_calls.contains_key(&key));
}

#[test]
fn a_delayed_judge_is_checked_against_the_policy_that_exists_when_it_completes() {
    let world = World::new(130, true);
    let key = world.call_key(1);
    let (mut core, env) = recovered(&world, 130, true);
    let judge = begin(
        &mut core,
        &env,
        core::EffectOrigin::Call { to: ReplyTo::new(Token::new(9910)), key, deadline: env.wall },
    );
    let mut policy = core.authority.policy(1).expect("policy").clone();
    policy.requirements[0].must_be_guarded = true;
    policy.requirements[0].guard = authority::Guard::Guarded;
    let mut facts = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(&mut core.authority, authority::Event::Policy { project: 1, policy }, &mut facts);
    assert_eq!(facts.pop(), Some(authority::PolicyFact::Changed { project: 1 }));
    let before = core.tasks.task(key.task).expect("live task").numbers;
    let outputs = verdict(&mut core, &env, judge);
    assert!(outputs.iter().any(|request| matches!(request, core::Request::Write(core::Write::Save(core::Record::Core(core::CoreRecord::Call(row)))) if matches!(row.part, core::CallPart::EffectDenied { answer: authority::Answer::Refuse, .. }))));
    assert!(!outputs.iter().any(|request| matches!(
        request,
        core::Request::Ask { ask: core::Ask::Effect(core::connector::Ask::Keep { .. }), .. }
    )));
    assert_eq!(core.tasks.task(key.task).expect("live task").numbers, before);
}

#[test]
fn a_delayed_judge_cannot_commit_a_step_of_a_cancelled_procedure() {
    let mut world = World::new(132, true);
    world.delegate();
    let task = world.procedure.expect("procedure");
    let parent = world.assignments[0].0;
    let (mut core, env) = recovered(&world, 132, true);
    let _requests = core::step(&mut core, &env, core::Event::Tasks(tasks::Event::WakeProcedure { task }));
    let step = core.tasks.task(task).expect("procedure").attempt + 1;
    let judge = begin(&mut core, &env, core::EffectOrigin::Procedure { task, step, entry: None });
    let _requests = core::step(
        &mut core,
        &env,
        core::Event::Tasks(tasks::Event::Control {
            reply_to: ReplyTo::new(Token::new(9930)),
            by: tasks::Party::Task(parent),
            task,
            control: tasks::Control::Cancel { reason: b"procedure withdrawn".as_slice().into() },
        }),
    );
    let before = core.tasks.task(task).expect("closing procedure").numbers;
    let outputs = verdict(&mut core, &env, judge);
    assert!(outputs.iter().any(|request| matches!(
        request,
        core::Request::Ask { ask: core::Ask::Effect(core::connector::Ask::Drop { .. }), .. }
    )));
    assert!(!outputs.iter().any(|request| matches!(
        request,
        core::Request::Write(_)
            | core::Request::Held(_)
            | core::Request::Ask { ask: core::Ask::Effect(core::connector::Ask::Keep { .. }), .. }
    )));
    assert_eq!(core.tasks.task(task).expect("closing procedure").numbers, before);
}

#[test]
fn every_connector_must_acknowledge_closure_and_duplicates_cannot_replace_a_missing_acknowledgement() {
    let world = World::new(133, false);
    let key = world.call_key(1);
    let (mut core, env) = recovered(&world, 133, false);
    let _requests = core::step(
        &mut core,
        &env,
        core::Event::Tasks(tasks::Event::Control {
            reply_to: ReplyTo::new(Token::new(9940)),
            by: tasks::Party::Person(1),
            task: key.task,
            control: tasks::Control::Cancel { reason: b"close all outboxes".as_slice().into() },
        }),
    );
    let _requests = core::step(
        &mut core,
        &env,
        core::Event::AnswerPayload {
            run: Token::new(key.task),
            attempt: Token::new(key.attempt),
            payload: Token::new(9941),
            cumulative: 0,
            end: tasks::End::Parked,
            saved: None,
            invalid_saved: false,
        },
    );
    for connector in [1, 1, 99] {
        let core::Requests::Out(mut requests) = core::step(
            &mut core,
            &env,
            core::Event::EffectConnector(core::connector::Event::Closed { task: key.task, connector }),
        );
        for _ in 0..requests.len() {
            assert!(!matches!(
                requests.pop().expect("request count"),
                core::Request::Ask { ask: core::Ask::Release { .. }, .. }
            ));
        }
        assert!(matches!(
            core.tasks.task(key.task).expect("still closing").phase,
            tasks::Phase::Closing(tasks::Closing { stage: tasks::Stage::Effects, .. })
        ));
    }
    let core::Requests::Out(mut requests) = core::step(
        &mut core,
        &env,
        core::Event::EffectConnector(core::connector::Event::Closed { task: key.task, connector: 2 }),
    );
    let mut releases = 0;
    for _ in 0..requests.len() {
        if matches!(requests.pop().expect("request count"), core::Request::Ask { ask: core::Ask::Release { .. }, .. }) {
            releases += 1;
        }
    }
    assert_eq!(releases, 2);
}

#[test]
fn a_conflicting_late_terminal_cannot_replace_an_already_committed_answer() {
    let mut world = World::new(131, false);
    let key = world.call_key(1);
    world.end(tasks::End::Parked, 0);
    let (mut core, env) = recovered(&world, 131, false);
    let core::Requests::Out(mut outputs) = core::step(
        &mut core,
        &env,
        core::Event::AnswerPayload {
            run: Token::new(key.task),
            attempt: Token::new(key.attempt),
            payload: Token::new(9920),
            cumulative: 0,
            end: tasks::End::Finished {
                result: tasks::TaskResult::Report { words: b"different late answer".as_slice().into() },
                cancel_delegates: false,
            },
            saved: None,
            invalid_saved: false,
        },
    );
    for _ in 0..outputs.len() {
        if let core::Request::Write(core::Write::Save(core::Record::Core(core::CoreRecord::Terminal(row)))) =
            outputs.pop().expect("request count")
        {
            assert_eq!(row.end, tasks::End::Parked);
        }
    }
    assert_eq!(
        core.proofs.get(&key.task).expect("retained proof").terminal.as_ref().expect("canonical terminal").end,
        tasks::End::Parked
    );
    core.remember_unpriced_terminal(Token::new(key.task), Token::new(key.attempt), tasks::End::Refused);
    let core::Requests::Out(mut requests) = core::step(
        &mut core,
        &env,
        core::Event::RefusedPayload {
            request: Token::new(9921),
            problem: tasks::Problem { task: Some(key.task), why: tasks::Refusal::State, blocked_by: None },
            payload: Some(core::PayloadRefusal::Answer { task: key.task, attempt: key.attempt }),
        },
    );
    for _ in 0..requests.len() {
        assert!(!matches!(requests.pop().expect("request count"), core::Request::Write(_)));
    }
    assert_eq!(
        core.proofs.get(&key.task).expect("retained proof").terminal.as_ref().expect("canonical terminal").end,
        tasks::End::Parked
    );
}

#[test]
fn late_busy_or_refused_descriptions_cannot_answer_a_cancelled_run() {
    let world = World::new(135, true);
    let key = world.call_key(1);
    for busy in [true, false] {
        let (mut core, env) = recovered(&world, 135, true);
        let owner = Token::new(9950);
        let _requests = core::step(
            &mut core,
            &env,
            core::Event::EffectStart {
                owner,
                connector: 1,
                origin: core::EffectOrigin::Call { to: ReplyTo::new(Token::new(9951)), key, deadline: env.wall },
            },
        );
        let _requests = core::step(
            &mut core,
            &env,
            core::Event::Tasks(tasks::Event::Control {
                reply_to: ReplyTo::new(Token::new(9952)),
                by: tasks::Party::Person(1),
                task: key.task,
                control: tasks::Control::Cancel { reason: b"cancel while describing".as_slice().into() },
            }),
        );
        let event = if busy {
            core::connector::Event::DescribeBusy { owner }
        } else {
            core::connector::Event::DescribeRefused { owner }
        };
        let core::Requests::Out(mut requests) = core::step(&mut core, &env, core::Event::EffectConnector(event));
        for _ in 0..requests.len() {
            assert!(!matches!(
                requests.pop().expect("request count"),
                core::Request::Write(_) | core::Request::Held(_) | core::Request::Now(_)
            ));
        }
        assert!(!core.pending_calls.contains_key(&key));
    }
}
