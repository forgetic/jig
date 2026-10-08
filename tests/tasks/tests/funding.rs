use jig_core_tasks::{
    self as tasks, Accepted, Cause, End, Event, Funder, Key, Numbers, Party, Refusal, Stored, TaskResult,
};
use jig_tasks_world::{LIMITS, Reply, World, task};
use skein_lib::ReplyTo;

fn send(world: &mut World, build: impl FnOnce(ReplyTo) -> Event) -> Reply {
    let reply_to = world.to();
    let key = reply_to.into_token().raw();
    world.send(build(ReplyTo::new(skein_lib::Token::new(key))));
    world.replies[&key].clone()
}

fn turn(world: &mut World, task: u64, attempt: u64, turn: u32, read: Option<u64>, cumulative: u64) -> Reply {
    send(world, |reply_to| Event::Turn { reply_to, task, attempt, turn, read, offered: None, cumulative })
}

fn end(world: &mut World, task: u64, attempt: u64, end: End, cumulative: u64) -> Reply {
    send(world, |reply_to| Event::Activation {
        reply_to,
        task,
        attempt,
        end,
        saved: None,
        cause: Cause::Priced { cumulative },
    })
}

fn ledger(world: &World, funder: Funder) -> tasks::FundingRecord {
    match world.records[&Key::Ledger(funder)] {
        Stored::Ledger(record) => record,
        Stored::Live(_)
        | Stored::Ended(_)
        | Stored::Writer(_)
        | Stored::Pool(_)
        | Stored::Stub(_)
        | Stored::Milestone(_)
        | Stored::History(_)
        | Stored::PersonProposal(_) => {
            unreachable!()
        }
    }
}

#[test]
fn finite_pool_carves_reserves_and_posts_to_its_original_period_atomically() {
    let mut w = World::new(71, LIMITS);
    w.open_period(1, 300);
    w.carve_pool(1, 200);
    let period = Funder::Period { project: 1, period: 1 };
    let pool = Funder::Pool { project: 1, person: 9, period: 1 };
    assert_eq!(ledger(&w, pool).parent, Some(period));
    let mut first = task(1, &[]);
    first.funder = pool;
    let mut second = task(2, &[]);
    second.funder = pool;
    w.make(Party::Person(9), vec![first]);
    assert_eq!(ledger(&w, pool).numbers.reserved, 100);
    let before = w.records.clone();
    let mut third = second.clone();
    third.number = 3;
    let reply = w.make(Party::Person(9), vec![second, third]);
    assert!(matches!(reply,Reply::Refused(problem) if problem.why==Refusal::Funding));
    assert_eq!(w.records, before);
    w.claim(1, 1);
    assert_eq!(turn(&mut w, 1, 1, 1, None, 20), Reply::Turn(Accepted::New));
    w.open_period(2, 300);
    w.restart();
    assert_eq!(
        end(
            &mut w,
            1,
            1,
            End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
            30
        ),
        Reply::Acknowledged(Accepted::New)
    );
    w.settle(1);
    assert_eq!(ledger(&w, pool).numbers, Numbers { budget: 200, spent: 0, spent_below: 30, reserved: 0 });
    assert_eq!(ledger(&w, period).numbers.reserved, 0);
    assert_eq!(ledger(&w, period).numbers.spent_below, 30);
    assert!(ledger(&w, period).closed);
    assert_eq!(ledger(&w, Funder::Period { project: 1, period: 2 }).numbers.reserved, 0);
}

#[test]
fn a_period_retires_once_its_last_reservation_closes() {
    let mut w = World::new(91, LIMITS);
    w.open_period(1, 300);
    w.carve_pool(1, 200);
    let period = Funder::Period { project: 1, period: 1 };
    let pool = Funder::Pool { project: 1, person: 9, period: 1 };
    let mut first = task(1, &[]);
    first.funder = pool;
    w.make(Party::Person(9), vec![first]);
    w.open_period(2, 300);
    assert!(!ledger(&w, period).closed);
    assert!(!ledger(&w, pool).closed);
    w.claim(1, 1);
    w.terminal_cause(
        1,
        End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 12 },
    );
    w.settle(1);
    assert_eq!(ledger(&w, pool).numbers.spent_below, 12);
    assert!(ledger(&w, pool).closed);
    assert!(ledger(&w, period).closed);
    assert_eq!(ledger(&w, period).numbers.spent_below, 12);
    assert_eq!(ledger(&w, period).numbers.reserved, 0);
    let before = w.records.clone();
    w.restart();
    assert_eq!(w.records, before);
}

#[test]
fn a_new_period_funds_new_work_while_the_old_one_settles() {
    let mut w = World::new(92, LIMITS);
    w.open_period(1, 100);
    let old = Funder::Period { project: 1, period: 1 };
    let fresh = Funder::Period { project: 1, period: 2 };
    let mut first = task(1, &[]);
    first.funder = old;
    w.make(Party::Person(9), vec![first]);
    w.open_period(2, 100);
    let mut second = task(2, &[]);
    second.funder = fresh;
    w.make(Party::Person(9), vec![second]);
    assert!(!ledger(&w, old).closed);
    assert_eq!(ledger(&w, fresh).numbers.reserved, 100);
    w.claim(1, 1);
    w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false });
    w.settle(1);
    assert!(ledger(&w, old).closed);
    assert!(!ledger(&w, fresh).closed);
    w.claim(2, 2);
    w.terminal(2, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false });
    w.settle(2);
    assert!(!ledger(&w, fresh).closed);
}

#[test]
fn priced_admissions_refuse_before_spending_and_adoption_carries_kept_turns() {
    let mut w = World::new(72, LIMITS);
    w.make(Party::Person(9), vec![task(1, &[])]);
    w.claim(1, 1);
    for (attempt, read, why) in [(2, None, Refusal::Attempt), (1, Some(999), Refusal::Read)] {
        let before = w.records.clone();
        assert!(matches!(turn(&mut w,1,attempt,1,read,20),Reply::Refused(problem) if problem.why==why));
        assert_eq!(w.records, before);
    }
    assert_eq!(turn(&mut w, 1, 1, 1, None, 20), Reply::Turn(Accepted::New));
    w.restart();
    assert_eq!(w.record(1).turn, 1);
    assert_eq!(w.runs.get(&1), Some(&1));
    let before = w.records.clone();
    assert!(matches!(turn(&mut w, 1, 1, 1, None, 20), Reply::Refused(problem) if problem.why == Refusal::Turn));
    assert_eq!(w.records, before);
    let finish = End::Finished { result: TaskResult::Report { words: Box::new([2]) }, cancel_delegates: false };
    assert!(matches!(end(&mut w,1,1,finish.clone(),19),Reply::Refused(problem) if problem.why==Refusal::Turn));
    assert_eq!(w.records, before);
    assert_eq!(end(&mut w, 1, 1, finish, 30), Reply::Acknowledged(Accepted::New));
    w.settle(1);
}

#[test]
fn priced_invalid_terminals_charge_but_live_delegate_refusals_do_not() {
    let mut w = World::new(73, LIMITS);
    w.make(Party::Person(9), vec![task(1, &[])]);
    w.make(Party::Task(1), vec![task(2, &[])]);
    w.claim(1, 1);
    let before = w.records.clone();
    assert!(
        matches!(end(&mut w,1,1,End::Finished { result:TaskResult::Report { words:Box::new([1]) },cancel_delegates:false },20),Reply::Refused(problem) if problem.why==Refusal::LiveDelegates && problem.blocked_by.as_deref() == Some(&[2][..]))
    );
    assert_eq!(w.records, before);
    assert_eq!(
        end(
            &mut w,
            1,
            1,
            End::Finished { result: TaskResult::Verdict { code: 99, words: Box::new([1]) }, cancel_delegates: false },
            20
        ),
        Reply::Acknowledged(Accepted::New)
    );
    assert_eq!(w.record(1).numbers.spent, 20);
    assert_eq!(w.record(1).tries.invalid, 1);
    w.advance();
    w.claim(1, 2);
    w.terminal(1, End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true });
    w.complete_cancel();
    assert_eq!(ledger(&w, Funder::Period { project: 1, period: 0 }).numbers.spent_below, 20);
}

#[test]
fn priced_turns_cannot_spend_beyond_the_claimed_allowance() {
    let mut w = World::new(75, LIMITS);
    w.make(Party::Person(9), vec![task(1, &[])]);
    w.claim(1, 1);
    assert_eq!(w.record(1).run_reserved, 100);
    let before = w.records.clone();
    assert!(matches!(turn(&mut w,1,1,1,None,101),Reply::Refused(problem) if problem.why==Refusal::Funding));
    assert_eq!(w.records, before);
    assert_eq!(turn(&mut w, 1, 1, 1, None, 100), Reply::Turn(Accepted::New));
    assert_eq!(w.record(1).numbers.spent, 100);
    assert_eq!(w.record(1).run_reserved, 0);
    let before = w.records.clone();
    assert!(matches!(turn(&mut w,1,1,2,None,101),Reply::Refused(problem) if problem.why==Refusal::Funding));
    assert_eq!(w.records, before);
}

#[test]
fn a_claim_holds_only_its_capped_allowance_and_returns_the_unused_part() {
    let mut w = World::new(76, LIMITS);
    w.make(Party::Person(9), vec![task(1, &[])]);
    w.claim_budget(1, 1, 40);
    assert_eq!(w.record(1).numbers, Numbers { budget: 100, spent: 0, spent_below: 0, reserved: 40 });
    assert_eq!(turn(&mut w, 1, 1, 1, None, 10), Reply::Turn(Accepted::New));
    assert_eq!(w.record(1).run_reserved, 30);
    assert_eq!(w.record(1).numbers.reserved, 30);
    assert!(matches!(turn(&mut w,1,1,2,None,41),Reply::Refused(problem) if problem.why==Refusal::Funding));
    assert_eq!(
        end(
            &mut w,
            1,
            1,
            End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
            15
        ),
        Reply::Acknowledged(Accepted::New)
    );
    assert_eq!(w.record(1).run_reserved, 0);
    assert_eq!(w.record(1).numbers, Numbers { budget: 100, spent: 15, spent_below: 0, reserved: 0 });
    w.restart();
    assert_eq!(w.record(1).numbers.reserved, 0);
}

#[test]
fn oversized_terminal_does_not_charge_or_copy_and_sources_refuse_at_capacity() {
    let mut w = World::new(77, LIMITS);
    w.make(Party::Person(9), vec![task(1, &[])]);
    w.claim(1, 1);
    let before = w.records.clone();
    assert!(
        matches!(end(&mut w,1,1,End::Finished { result:TaskResult::Report { words:vec![0;1024].into_boxed_slice() },cancel_delegates:false },2),Reply::Refused(problem) if problem.why==Refusal::Contract)
    );
    assert_eq!(w.records, before);
    let mut w = World::new(78, tasks::Limits { funders: 1, ..LIMITS });
    let before = w.records.clone();
    w.open_period(1, 100);
    assert!(
        matches!(w.replies.last_key_value().expect("reply").1,Reply::Refused(problem) if problem.why==Refusal::Busy)
    );
    assert_eq!(w.records, before);
}

#[test]
fn independent_referee_detects_omitted_ledger_save_and_settlement_posting() {
    use jig_tasks_world::accounting_referee::Accounting;
    let mut w = World::new(79, LIMITS);
    let before = w.records.clone();
    w.make(Party::Person(9), vec![task(1, &[])]);
    let source = Funder::Period { project: 1, period: 0 };
    let mut good = Accounting::default();
    good.reset(&before);
    assert_eq!(good.committed(&w.records, &[]), Ok(()));
    let mut bad = w.records.clone();
    bad.insert(Key::Ledger(source), before[&Key::Ledger(source)].clone());
    let mut judge = Accounting::default();
    judge.reset(&before);
    assert!(judge.committed(&bad, &[]).is_err());
    w.claim(1, 1);
    w.terminal_cause(
        1,
        End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 7 },
    );
    let before = w.records.clone();
    w.settle(1);
    let mut bad = w.records.clone();
    if let Some(Stored::Ledger(ledger)) = bad.get_mut(&Key::Ledger(source)) {
        ledger.numbers.spent_below = 0;
    }
    let mut judge = Accounting::default();
    judge.reset(&before);
    assert!(judge.committed(&bad, &[]).is_err());
}

#[test]
fn delegate_expense_follows_task_funding_chain_once() {
    let mut w = World::new(81, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    let mut child = task(2, &[]);
    child.funder = Funder::Task(1);
    child.numbers.budget = 40;
    child.authority.budget.spend = 40;
    w.make(Party::Task(1), vec![child]);
    assert_eq!(w.record(1).numbers.reserved, 40);
    w.claim(1, 1);
    assert_eq!(w.record(1).numbers.reserved, 100, "parent's run reserves its remaining 60");
    w.claim(2, 2);
    assert_eq!(turn(&mut w, 2, 2, 1, None, 8), Reply::Turn(Accepted::New));
    w.terminal_cause(
        2,
        End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 12 },
    );
    w.settle(2);
    assert_eq!(w.record(1).numbers, Numbers { budget: 100, spent: 0, spent_below: 12, reserved: 60 });
    w.restart();
    w.terminal_cause(
        1,
        End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 5 },
    );
    w.settle(1);
    assert_eq!(ledger(&w, Funder::Period { project: 1, period: 0 }).numbers.spent_below, 17);
    let before = w.records.clone();
    w.restart();
    assert_eq!(w.records, before);
}
