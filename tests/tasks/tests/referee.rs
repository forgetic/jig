use jig_core_tasks::{Party, Status};
use jig_tasks_world::referee::{Seen, Tasks};
use skein_lib::Time;
use skein_world::domain::{Referee, Verdict};

fn rejects(seen: Vec<Seen>) {
    let mut referee = Referee::new(Tasks::default());
    let mut out = Vec::new();
    for observation in seen {
        referee.observe(Time::ZERO, observation, &mut out);
    }
    assert!(matches!(referee.verdict(), Verdict::Failed(_)), "referee catches violated invariant");
}

fn made(task: u64) -> Seen {
    Seen::Made { task, parent: Party::Person(1), dependencies: vec![], depth: 0 }
}

fn assigned(task: u64, attempt: u64) -> Seen {
    Seen::Assigned { task, attempt, after: 0, adopted: false }
}

fn partial_batch() {
    rejects(vec![Seen::Batch { members: vec![1, 2], accepted: true, made: vec![1] }]);
}

fn refused_batch_created_task() {
    rejects(vec![Seen::Batch { members: vec![1], accepted: false, made: vec![1] }]);
}

fn reused_number() {
    rejects(vec![made(1), made(1)]);
}

fn dependency_on_self() {
    rejects(vec![Seen::Made { task: 1, parent: Party::Person(1), dependencies: vec![1], depth: 0 }]);
}

fn delegate_without_parent() {
    rejects(vec![Seen::Made { task: 2, parent: Party::Task(1), dependencies: vec![], depth: 1 }]);
}

fn assignment_before_dependency_done() {
    rejects(vec![
        made(1),
        Seen::Made { task: 2, parent: Party::Person(1), dependencies: vec![1], depth: 0 },
        assigned(2, 1),
    ]);
}

fn overlapping_runs() {
    rejects(vec![made(1), assigned(1, 1), assigned(1, 2)]);
}

fn nonmonotonic_attempt() {
    rejects(vec![made(1), assigned(1, 2), Seen::Terminal { task: 1, attempt: 2 }, assigned(1, 1)]);
}

fn terminal_of_wrong_attempt() {
    rejects(vec![made(1), assigned(1, 1), Seen::Terminal { task: 1, attempt: 2 }]);
}

fn closing_before_run_ended() {
    rejects(vec![made(1), assigned(1, 1), Seen::Closing { task: 1 }]);
}

fn closing_before_delegate_ended() {
    rejects(vec![
        made(1),
        Seen::Made { task: 2, parent: Party::Task(1), dependencies: vec![], depth: 1 },
        Seen::Closing { task: 1 },
    ]);
}

fn settlement_before_close() {
    rejects(vec![Seen::Settled { task: 1 }]);
}

fn result_before_settlement() {
    rejects(vec![made(1), Seen::Ended { task: 1, status: Status::Done, after: 0 }]);
}

fn duplicate_result() {
    rejects(vec![
        made(1),
        Seen::Closing { task: 1 },
        Seen::Settled { task: 1 },
        Seen::Ended { task: 1, status: Status::Done, after: 0 },
        Seen::Ended { task: 1, status: Status::Done, after: 0 },
    ]);
}

fn parent_result_before_child() {
    rejects(vec![
        made(1),
        Seen::Closing { task: 1 },
        Seen::Settled { task: 1 },
        Seen::Made { task: 2, parent: Party::Task(1), dependencies: vec![], depth: 1 },
        Seen::Ended { task: 1, status: Status::Done, after: 0 },
    ]);
}

fn wrong_cancelled_ending() {
    rejects(vec![
        made(1),
        Seen::Cancelled { task: 1 },
        Seen::Closing { task: 1 },
        Seen::Settled { task: 1 },
        Seen::Ended { task: 1, status: Status::Done, after: 0 },
    ]);
}

fn cancelled_descendant_missing() {
    rejects(vec![made(1), Seen::Cancelled { task: 1 }, Seen::Finished]);
}

fn live_limit() {
    rejects(vec![Seen::Limit { live: 2, cap: 1 }]);
}

fn reply_before_durable() {
    rejects(vec![Seen::Replied { call: 1, after: 1 }]);
}

fn duplicate_reply() {
    rejects(vec![Seen::Replied { call: 1, after: 0 }, Seen::Replied { call: 1, after: 0 }]);
}

fn assignment_before_durable() {
    rejects(vec![made(1), Seen::Assigned { task: 1, attempt: 1, after: 1, adopted: false }]);
}

fn result_before_durable() {
    rejects(vec![
        made(1),
        Seen::Closing { task: 1 },
        Seen::Settled { task: 1 },
        Seen::Ended { task: 1, status: Status::Done, after: 1 },
    ]);
}

#[test]
fn each_initial_invariant_catches_an_independent_violation() {
    dependency_cycle();
    cycle_through_delegate();
    missing_end_deadline();
    stored_limits();
    partial_batch();
    refused_batch_created_task();
    reused_number();
    dependency_on_self();
    delegate_without_parent();
    assignment_before_dependency_done();
    overlapping_runs();
    nonmonotonic_attempt();
    terminal_of_wrong_attempt();
    closing_before_run_ended();
    closing_before_delegate_ended();
    settlement_before_close();
    result_before_settlement();
    duplicate_result();
    parent_result_before_child();
    wrong_cancelled_ending();
    cancelled_descendant_missing();
    live_limit();
    reply_before_durable();
    duplicate_reply();
    assignment_before_durable();
    result_before_durable();
}

fn dependency_cycle() {
    rejects(vec![
        Seen::Made { task: 1, parent: Party::Person(1), dependencies: vec![2], depth: 0 },
        Seen::Made { task: 2, parent: Party::Person(1), dependencies: vec![1], depth: 0 },
    ]);
}

fn cycle_through_delegate() {
    rejects(vec![made(1), Seen::Made { task: 2, parent: Party::Task(1), dependencies: vec![1], depth: 1 }]);
}

fn missing_end_deadline() {
    let mut referee = Referee::new(Tasks::default());
    let mut out = Vec::new();
    referee.observe(Time::ZERO, made(1), &mut out);
    referee.observe(Time::ZERO, Seen::Cancelled { task: 1 }, &mut out);
    referee.fire(Time::ZERO.saturating_add(skein_lib::Duration::from_secs(10)), &mut out);
    assert!(matches!(referee.verdict(), Verdict::Failed(_)), "missing cancelled result expires");
}

fn stored_limits() {
    use jig_tasks_world::{LIMITS, World, task};
    let mut w = World::new(0, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    for field in 0..10 {
        let mut record = w.record(1).clone();
        match field {
            0 => record.depth = LIMITS.depth + 1,
            1 => record.made = LIMITS.tree_tasks + 1,
            2 => record.delegates = vec![2; 9].into_boxed_slice(),
            3 => record.dependencies = vec![2; 9].into_boxed_slice(),
            4 => record.spec.words = vec![1; 65].into_boxed_slice(),
            5 => record.spec.inputs = vec![2; 5].into_boxed_slice(),
            6 => {
                record.spec.parameters =
                    vec![jig_core_tasks::Parameter::Number { name: 1, value: 1 }; 5].into_boxed_slice();
            }
            7 => record.contract = jig_core_tasks::Contract::Report { words: 33 },
            8 => {
                record.authority.delegation.kinds =
                    vec![jig_core_tasks::AuthorityExecutor::Role(1); 4].into_boxed_slice();
            }
            9 => {
                record.authority.grants = vec![
                    jig_core_tasks::Grant {
                        connector: 0,
                        kind: 0,
                        pattern: jig_core_tasks::Pattern {
                            segments: Box::new([]),
                            last: jig_core_tasks::Last::Exact(Box::from(b"resource".as_slice()))
                        }
                    };
                    5
                ]
                .into_boxed_slice();
            }
            _ => unreachable!(),
        }
        rejects(vec![Seen::Stored { live: vec![record], limits: Box::new(LIMITS) }]);
    }
    let record = w.record(1).clone();
    rejects(vec![Seen::Stored {
        live: vec![record.clone(), record],
        limits: Box::new(jig_core_tasks::Limits { project_tasks: 1, ..LIMITS }),
    }]);
}

#[test]
fn accounting_referee_rejects_expense_reservation_and_settlement_corruption() {
    use jig_core_tasks::{Cause, End, Funder, Key, Stored};
    use jig_tasks_world::{LIMITS, World, accounting_referee::Accounting, task};
    let mut w = World::new(80, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    let before = w.records.clone();
    w.terminal_cause(
        1,
        End::Finished { result: jig_core_tasks::TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 7 },
    );
    for fault in 0..3 {
        let mut bad = w.records.clone();
        if let Some(Stored::Live(task)) = bad.get_mut(&Key::Live(1)) {
            match fault {
                0 => task.numbers.spent = 0,
                1 => task.run_spent = 0,
                2 => task.numbers.reserved = 1,
                _ => unreachable!(),
            }
        }
        let mut judge = Accounting::default();
        judge.reset(&before);
        judge.charged(1, 7);
        assert!(judge.committed(&bad).is_err(), "expense fault {fault}");
    }
    let before = w.records.clone();
    w.settle(1);
    for fault in 0..6 {
        let mut bad = w.records.clone();
        match fault {
            0 => {
                bad.remove(&Key::Ended(1));
            }
            1 => {
                if let Some(Stored::Ended(row)) = bad.get_mut(&Key::Ended(1)) {
                    row.numbers.spent = 6;
                }
            }
            2 => {
                if let Some(Stored::Ended(row)) = bad.get_mut(&Key::Ended(1)) {
                    row.funder = Funder::Task(99);
                }
            }
            3 => {
                if let Some(Stored::Ledger(row)) = bad.get_mut(&Key::Ledger(Funder::Period { project: 1, period: 0 })) {
                    row.numbers.reserved = 100;
                }
            }
            4 => {
                if let Some(Stored::Ledger(row)) = bad.get_mut(&Key::Ledger(Funder::Period { project: 1, period: 0 })) {
                    row.numbers.spent_below = 0;
                }
            }
            5 => {
                if let Some(Stored::Ended(row)) = bad.get_mut(&Key::Ended(1)) {
                    row.numbers.budget = 99;
                }
            }
            _ => unreachable!(),
        }
        let mut judge = Accounting::default();
        judge.reset(&before);
        assert!(judge.committed(&bad).is_err(), "settlement fault {fault}");
    }
    let mut bad = w.records.clone();
    bad.remove(&Key::Ended(1));
    let mut judge = Accounting::default();
    judge.reset(&w.records);
    assert_eq!(judge.committed(&bad), Err("immutable accounting row changed"));
}
