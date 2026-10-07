use jig_core_tasks::{
    Active, Class, Contract, End, Ending, Event, Hold, Key, Party, Phase, Refusal, Stage, TaskResult, Was,
};
use jig_tasks_world::{LIMITS, Reply, World, task};

fn refused(reply: &Reply, why: Refusal) {
    assert!(matches!(reply, Reply::Refused(problem) if problem.why == why), "expected {why:?}, got {reply:?}");
}

#[test]
fn batch_is_atomic_and_cycles_and_limits_refuse_at_entrance() {
    for (limits, why) in [
        (jig_core_tasks::Limits { batch: 1, ..LIMITS }, Refusal::Batch),
        (jig_core_tasks::Limits { tasks: 1, ..LIMITS }, Refusal::Live),
        (jig_core_tasks::Limits { project_tasks: 1, ..LIMITS }, Refusal::Project),
    ] {
        let mut w = World::new(1, limits);
        let before = w.records.clone();
        refused(&w.make(Party::Person(1), vec![task(1, &[]), task(2, &[])]), why);
        assert_eq!(w.records, before);
    }
    let mut w = World::new(1, LIMITS);
    refused(&w.make(Party::Person(1), vec![task(1, &[2]), task(2, &[1])]), Refusal::Cycle);
    refused(&w.make(Party::Person(1), vec![task(1, &[]), task(1, &[])]), Refusal::Duplicate);
    refused(&w.make(Party::Person(1), vec![]), Refusal::Empty);
    for (new, why) in [
        (
            {
                let mut n = task(2, &[]);
                n.spec.words = vec![1; 65].into_boxed_slice();
                n
            },
            Refusal::Spec,
        ),
        (
            {
                let mut n = task(2, &[]);
                n.contract = Contract::Report { words: 33 };
                n
            },
            Refusal::Contract,
        ),
        (
            {
                let mut n = task(2, &[]);
                n.executor = jig_core_tasks::Executor::Agent { charter: 99 };
                n
            },
            Refusal::Executor,
        ),
        (task(2, &[99]), Refusal::Dependencies),
        (
            {
                let mut n = task(2, &[]);
                n.authority.delegation.kinds = vec![jig_core_tasks::AuthorityExecutor::Role(1); 4].into_boxed_slice();
                n
            },
            Refusal::AuthorityShape,
        ),
    ] {
        let before = w.records.clone();
        refused(&w.make(Party::Person(1), vec![task(1, &[]), new]), why);
        assert_eq!(w.records, before);
    }
}

#[test]
fn bounded_historical_inputs_are_left_for_the_roots_archive_check() {
    let mut w = World::new(101, LIMITS);
    let mut next = task(2, &[]);
    next.spec.inputs = Box::new([99]);
    assert_eq!(w.make(Party::Person(1), vec![task(1, &[]), next]), Reply::Made(vec![1, 2]));
}

#[test]
fn a_dependency_on_an_introduced_sibling_is_accepted_and_a_cross_subtree_wait_cycle_refused() {
    for cycle in [false, true] {
        let mut w = World::new(102 + u64::from(cycle), LIMITS);
        w.make(Party::Person(1), vec![task(1, &[])]);
        let right = if cycle { task(3, &[2]) } else { task(3, &[]) };
        w.make(Party::Task(1), vec![task(2, &[]), right]);
        refused(&w.make(Party::Task(2), vec![task(4, &[3])]), Refusal::Dependencies);
        let reply_to = w.to();
        let key = reply_to.into_token().raw();
        w.send(Event::Introduce {
            reply_to: skein_lib::ReplyTo::new(skein_lib::Token::new(key)),
            by: 1,
            left: 2,
            right: 3,
        });
        assert_eq!(w.replies[&key], Reply::Done);
        assert_eq!(w.record(2).references.as_ref(), [3]);
        assert_eq!(w.record(3).references.as_ref(), [2]);
        let answer = w.make(Party::Task(2), vec![task(4, &[3])]);
        if cycle {
            refused(&answer, Refusal::Cycle);
        } else {
            assert_eq!(answer, Reply::Made(vec![4]));
        }
        w.restart();
    }
}

#[test]
fn tree_depth_and_delegate_limits_are_atomic() {
    for (limits, why) in [
        (jig_core_tasks::Limits { tree_tasks: 1, ..LIMITS }, Refusal::Tree),
        (jig_core_tasks::Limits { depth: 0, ..LIMITS }, Refusal::Depth),
        (jig_core_tasks::Limits { delegates: 0, ..LIMITS }, Refusal::Delegates),
    ] {
        let mut w = World::new(2, limits);
        w.make(Party::Person(1), vec![task(1, &[])]);
        let before = w.records.clone();
        refused(&w.make(Party::Task(1), vec![task(2, &[])]), why);
        assert_eq!(w.records, before);
    }
}

#[test]
fn dependency_order_negative_verdict_starts_but_failure_holds() {
    for fail in [false, true] {
        let mut w = World::new(3, LIMITS);
        let mut first = task(1, &[]);
        first.contract = Contract::Verdict { choices: Box::new([jig_core_tasks::Verdict { code: 0, words: 32 }]) };
        w.make(Party::Person(1), vec![first, task(2, &[1])]);
        assert!(!w.activations.contains(&2));
        w.claim(1, 1);
        let result = if fail {
            TaskResult::Failure { reason: Box::new([1]) }
        } else {
            TaskResult::Verdict { code: 0, words: Box::new([]) }
        };
        w.terminal(1, End::Finished { result, cancel_delegates: false });
        assert!(!w.activations.contains(&2));
        w.settle(1);
        if fail {
            assert_eq!(w.record(2).phase, Phase::Held { was: Was::Waiting, why: Hold::Dependency(1) });
            w.restart();
            assert_eq!(w.record(2).phase, Phase::Held { was: Was::Waiting, why: Hold::Dependency(1) });
        } else {
            w.restart();
            w.claim(2, 2);
            w.finish(2);
            w.settle(2);
        }
    }
}

#[test]
fn ended_dependency_keeps_a_bounded_stub_until_its_last_live_reader_ends() {
    let mut w = World::new(103, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[]), task(2, &[1])]);
    w.claim(1, 1);
    w.finish(1);
    w.settle(1);
    assert!(!w.records.contains_key(&Key::Live(1)));
    assert!(
        matches!(w.records.get(&Key::Stub(1)), Some(jig_core_tasks::Stored::Stub(stub)) if stub.task == 1 && stub.result.raw() == 1)
    );
    w.restart();
    assert!(w.records.contains_key(&Key::Stub(1)));
    w.claim(2, 2);
    w.finish(2);
    w.settle(2);
    assert!(!w.records.contains_key(&Key::Stub(1)));
    assert!(w.records.contains_key(&Key::Ended(1)));
}

#[test]
fn ended_reference_survives_restart_as_a_stub_until_its_peer_ends() {
    let mut w = World::new(104, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.make(Party::Task(1), vec![task(2, &[]), task(3, &[])]);
    let reply_to = w.to();
    w.send(Event::Introduce { reply_to, by: 1, left: 2, right: 3 });
    w.claim(2, 2);
    w.finish(2);
    w.settle(2);
    assert!(w.records.contains_key(&Key::Stub(2)));
    assert_eq!(w.record(3).references.as_ref(), [2]);
    w.restart();
    assert_eq!(w.record(3).references.as_ref(), [2]);
    w.claim(3, 3);
    w.finish(3);
    w.settle(3);
    assert!(!w.records.contains_key(&Key::Stub(2)));
}

#[test]
fn every_failure_class_holds_after_its_retry_budget() {
    for class in [Class::Transient, Class::Permanent, Class::Run, Class::Agent, Class::Lost, Class::Invalid] {
        let mut w = World::new(4, LIMITS);
        w.make(Party::Person(1), vec![task(1, &[])]);
        for attempt in 1..=3 {
            w.claim(1, attempt);
            w.terminal(1, End::Failed(class));
            if attempt < 3 {
                w.advance();
            }
        }
        assert_eq!(w.record(1).phase, Phase::Held { was: Was::Active(Active::Due), why: Hold::Failures(class) });
        assert_eq!(w.record(1).tries.of(class), 3);
        let before = w.records.clone();
        w.restart();
        assert_eq!(w.records, before);
        assert!(w.activations.is_empty());
    }
}

#[test]
fn a_delegate_failing_past_its_tries_is_held_and_released_finishes() {
    let mut w = World::new(105, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.make(Party::Task(1), vec![task(2, &[])]);
    for attempt in 1..=3 {
        w.claim(2, attempt);
        w.terminal(2, End::Failed(Class::Run));
        if attempt < 3 {
            w.advance();
        }
    }
    assert_eq!(w.record(2).phase, Phase::Held { was: Was::Active(Active::Due), why: Hold::Failures(Class::Run) });
    assert_eq!(w.record(2).tries.run, 3);
    w.restart();
    let reply_to = w.to();
    let call = reply_to.into_token().raw();
    w.send(Event::Control {
        reply_to: skein_lib::ReplyTo::new(skein_lib::Token::new(call)),
        by: Party::Task(1),
        task: 2,
        control: jig_core_tasks::Control::Release,
    });
    assert_eq!(w.replies[&call], Reply::Done);
    assert_eq!(w.record(2).tries, jig_core_tasks::Tries::NONE);
    w.claim(2, 4);
    w.finish(2);
    w.settle(2);
    assert!(matches!(w.results.get(&2), Some(Ending::Done(_))));
    w.claim(1, 5);
    w.finish(1);
    w.settle(1);
}

#[test]
fn typed_hold_reasons_survive_restart_and_release_without_losing_task_state() {
    let reasons = [
        Hold::Stalled,
        Hold::EffectFailed,
        Hold::Uncertain { entry: 7 },
        Hold::HoldsWaited,
        Hold::PoolLost { pool: 8 },
        Hold::Procedure,
        Hold::StoppedBy { party: 1 },
    ];
    for (index, reason) in reasons.into_iter().enumerate() {
        let mut w = World::new(106 + u64::try_from(index).expect("bounded reason index"), LIMITS);
        w.make(Party::Person(1), vec![task(1, &[])]);
        w.send(Event::Hold { task: 1, why: reason });
        assert_eq!(w.record(1).phase, Phase::Held { was: Was::Active(Active::Due), why: reason });
        w.restart();
        assert_eq!(w.record(1).phase, Phase::Held { was: Was::Active(Active::Due), why: reason });
        let reply_to = w.to();
        let call = reply_to.into_token().raw();
        w.send(Event::Control {
            reply_to: skein_lib::ReplyTo::new(skein_lib::Token::new(call)),
            by: Party::Person(1),
            task: 1,
            control: jig_core_tasks::Control::Release,
        });
        assert_eq!(w.replies[&call], Reply::Done);
        assert_eq!(w.record(1).phase, Phase::Active(Active::Due));
    }
}

#[test]
fn releasing_a_held_closing_task_retries_its_unsettled_effects() {
    let mut w = World::new(114, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.finish(1);
    w.send(Event::Hold { task: 1, why: Hold::EffectFailed });
    assert!(matches!(w.record(1).phase, Phase::Held { was: Was::Closing(_), why: Hold::EffectFailed }));
    w.restart();
    let reply_to = w.to();
    let call = reply_to.into_token().raw();
    w.send(Event::Control {
        reply_to: skein_lib::ReplyTo::new(skein_lib::Token::new(call)),
        by: Party::Person(1),
        task: 1,
        control: jig_core_tasks::Control::Release,
    });
    assert_eq!(w.replies[&call], Reply::Done);
    assert!(matches!(w.record(1).phase, Phase::Closing(jig_core_tasks::Closing { stage: Stage::Effects, .. })));
    assert!(w.closing.contains(&1), "the close obligation is reissued after release");
    w.settle(1);
    assert!(matches!(w.results.get(&1), Some(Ending::Done(_))));
}

#[test]
fn refusals_and_preparation_failure_do_not_spend_tries() {
    let mut w = World::new(5, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    let reply_to = w.to();
    w.send(Event::Prepare { reply_to, task: 1 });
    w.send(Event::PreparationFailed { task: 1 });
    assert_eq!(w.record(1).attempt, 0);
    w.advance();
    for attempt in 1..=5 {
        w.claim(1, attempt);
        w.terminal(1, End::Refused);
        w.advance();
        assert_eq!(w.record(1).tries, jig_core_tasks::Tries::NONE);
    }
    w.claim(1, 6);
    w.finish(1);
    w.settle(1);
}

#[test]
fn held_running_task_waits_for_terminal_and_retains_its_hold() {
    let mut w = World::new(6, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.send(Event::Hold { task: 1, why: Hold::Stopped });
    assert!(w.stops.contains(&(1, 1)));
    w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false });
    assert!(w.closing.is_empty());
    assert!(matches!(w.record(1).phase, Phase::Held { was: Was::Closing(_), .. }));
    let before = w.records.clone();
    w.restart();
    assert_eq!(w.records, before);
    assert!(w.closing.is_empty());
    assert!(w.results.is_empty());
    assert!(matches!(
        w.record(1).phase,
        Phase::Held { was: Was::Closing(jig_core_tasks::Closing { stage: Stage::Delegates, .. }), why: Hold::Stopped }
    ));
}

#[test]
fn corrected_finish_cancels_delegates_and_closes_deepest_first() {
    let mut w = World::new(7, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.make(Party::Task(1), vec![task(2, &[])]);
    w.claim(2, 2);
    w.make(Party::Task(2), vec![task(3, &[])]);
    w.claim(3, 3);
    let finish = End::Finished { result: TaskResult::Report { words: Box::new([9]) }, cancel_delegates: false };
    let before = w.records.clone();
    refused(&w.terminal(1, finish), Refusal::LiveDelegates);
    assert_eq!(w.records, before);
    w.observe(jig_tasks_world::referee::Seen::Cancelled { task: 2 });
    w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true });
    assert!(w.closing.is_empty());
    w.terminal(3, End::Finished { result: TaskResult::Report { words: Box::new([9]) }, cancel_delegates: false });
    w.terminal(2, End::Parked);
    assert_eq!(w.closing.iter().copied().collect::<Vec<_>>(), [3]);
    w.settle(3);
    assert!(w.closing.contains(&2));
    w.settle(2);
    assert!(w.closing.contains(&1));
    w.settle(1);
    assert!(
        matches!(w.results.get(&3),Some(Ending::Cancelled { result:Some(TaskResult::Report { words }),.. }) if words.as_ref()==[9])
    );
    assert!(matches!(w.results.get(&1), Some(Ending::Done(_))));
    w.referee.assert_passed(7);
}

#[test]
fn a_cancel_closes_a_tree_of_three_levels_deepest_first_with_runs_live_and_effects_in_flight() {
    let mut w = World::new(71, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.make(Party::Task(1), vec![task(2, &[])]);
    w.claim(2, 2);
    w.make(Party::Task(2), vec![task(3, &[])]);
    w.claim(3, 3);

    w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true });
    assert!(w.stops.contains(&(2, 2)) && w.stops.contains(&(3, 3)));
    w.terminal(3, End::Parked);
    w.terminal(2, End::Parked);
    assert_eq!(w.closing.iter().copied().collect::<Vec<_>>(), [3]);
    w.settle_effects(3);
    assert!(w.results.is_empty(), "effect settlement alone does not end the deepest task");
    w.release(3);
    assert_eq!(w.closing.iter().copied().collect::<Vec<_>>(), [2]);
    w.settle_effects(2);
    assert!(w.results.contains_key(&3) && !w.results.contains_key(&2));
    w.release(2);
    assert_eq!(w.closing.iter().copied().collect::<Vec<_>>(), [1]);
    w.settle(1);
    assert_eq!(w.results.len(), 3);
    assert!(matches!(w.results.get(&1), Some(Ending::Done(_))));
    assert!(matches!(w.results.get(&2), Some(Ending::Cancelled { .. })));
    assert!(matches!(w.results.get(&3), Some(Ending::Cancelled { .. })));
}

#[test]
fn a_dependent_starts_only_after_its_dependency_has_closed_and_its_effects_settled() {
    let mut w = World::new(72, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[]), task(2, &[1])]);
    assert!(w.activations.contains(&1) && !w.activations.contains(&2));
    w.claim(1, 1);
    w.finish(1);
    assert!(w.closing.contains(&1));
    assert!(!w.activations.contains(&2));
    w.settle_effects(1);
    assert!(!w.activations.contains(&2), "resource cleanup still follows effect settlement");
    w.release(1);
    assert!(w.activations.contains(&2));
}

#[test]
fn lifetime_tree_limit_survives_delegate_ending() {
    let mut w = World::new(8, jig_core_tasks::Limits { tree_tasks: 2, ..LIMITS });
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.make(Party::Task(1), vec![task(2, &[])]);
    w.claim(2, 1);
    w.finish(2);
    w.settle(2);
    assert_eq!(w.record(1).made, 2);
    refused(&w.make(Party::Task(1), vec![task(3, &[])]), Refusal::Tree);
}

#[test]
fn invalid_results_spend_invalid_tries_and_refused_held_closing_make_is_atomic() {
    let mut w = World::new(9, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.terminal(
        1,
        End::Finished { result: TaskResult::Verdict { code: 0, words: Box::new([]) }, cancel_delegates: false },
    );
    assert_eq!(w.record(1).tries.invalid, 1);
    w.advance();
    w.claim(1, 2);
    w.finish(1);
    w.send(Event::Hold { task: 1, why: Hold::Effects });
    let before = w.records.clone();
    refused(&w.make(Party::Task(1), vec![task(2, &[]), task(3, &[2])]), Refusal::State);
    assert_eq!(w.records, before);
    w.restart();
    w.settle(1);
    assert!(w.records.contains_key(&Key::Live(1)));
    assert!(w.results.is_empty());
    let before = w.records.clone();
    w.restart();
    assert_eq!(w.records, before);
}

#[test]
fn cancelling_unfinished_dependency_siblings_finishes_in_either_settlement_order() {
    for order in [[2, 3], [3, 2]] {
        let mut w = World::new(11, LIMITS);
        w.make(Party::Person(1), vec![task(1, &[])]);
        w.claim(1, 1);
        w.make(Party::Task(1), vec![task(2, &[]), task(3, &[2])]);
        w.claim(2, 2);
        w.observe(jig_tasks_world::referee::Seen::Cancelled { task: 2 });
        w.observe(jig_tasks_world::referee::Seen::Cancelled { task: 3 });
        w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true });
        w.terminal(2, End::Parked);
        assert!(w.closing.contains(&2) && w.closing.contains(&3));
        for task in order {
            w.settle(task);
            assert!(matches!(w.results.get(&task), Some(Ending::Cancelled { .. })));
        }
        w.settle(1);
        assert_eq!(w.live(), 0);
        w.referee.assert_passed(11);
    }
}
