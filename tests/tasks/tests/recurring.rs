use jig_core_tasks::{
    self as tasks, Cause, Contract, Event, Executor, Funder, Key, New, Numbers, Party, RecurringOverlap,
    RecurringTemplate, Stored, Subscription, SubscriptionKind,
};
use jig_tasks_world::{LIMITS, Reply, World, task};

fn recurring(number: u64, overlap: RecurringOverlap) -> New {
    let mut member = task(1, &[]);
    member.numbers.budget = 30;
    member.authority.budget.spend = 30;
    let mut master = task(number, &[]);
    master.executor = Executor::Procedure { connector: 0, code: 1 };
    master.numbers = Numbers { budget: 0, spent: 0, spent_below: 0, reserved: 0 };
    master.contract = Contract::Report { words: 0 };
    master.recurring = Some(Box::new(RecurringTemplate { key: 1, batch: Box::new([member]), overlap }));
    master
}

fn batch(world: &mut World, task: u64, period: u64, child: u64) {
    world.send(Event::TickRecurring { task, period });
    world.send(Event::RecurringBatch { task, period, numbers: Box::new([child]) });
    assert_eq!(world.record(child).funder, Funder::Recurring { project: 1, task, period });
}

#[test]
fn a_recurring_task_continues_across_a_period_reset() {
    let mut world = World::new(210, LIMITS);
    assert_eq!(
        world.make(Party::Deployment { project: 1 }, vec![recurring(9, RecurringOverlap::Skip)]),
        Reply::Made(vec![9])
    );
    world.open_period(1, 100_000);
    batch(&mut world, 9, 1, 10);
    world.claim(10, 10);
    world.terminal(
        10,
        tasks::End::Finished { result: tasks::TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
    );
    world.settle(10);
    world.open_period(2, 100_000);
    batch(&mut world, 9, 2, 11);
    assert_eq!(world.record(9).recurring.as_ref().expect("template").last_period, 2);
    assert!(world.records.contains_key(&tasks::Key::Ledger(Funder::Recurring { project: 1, task: 9, period: 1 })));
    world.restart();
    assert_eq!(world.record(11).funder, Funder::Recurring { project: 1, task: 9, period: 2 });
}

#[test]
fn a_recurring_task_with_the_engine_down_for_several_periods_makes_one_batch() {
    let mut world = World::new(211, LIMITS);
    assert_eq!(
        world.make(Party::Deployment { project: 1 }, vec![recurring(9, RecurringOverlap::Skip)]),
        Reply::Made(vec![9])
    );
    world.restart();
    world.open_period(4, 100_000);
    batch(&mut world, 9, 4, 10);
    assert_eq!(world.record(9).recurring.as_ref().expect("template").last_period, 4);
    assert_eq!(world.live(), 2, "only the master and newest batch member exist");
    world.restart();
    assert_eq!(world.record(10).funder, Funder::Recurring { project: 1, task: 9, period: 4 });
}

#[test]
fn a_recurring_template_waits_for_its_old_batch_before_the_new_period() {
    let mut world = World::new(212, LIMITS);
    assert_eq!(
        world.make(Party::Deployment { project: 1 }, vec![recurring(9, RecurringOverlap::Wait)]),
        Reply::Made(vec![9])
    );
    world.open_period(1, 100_000);
    batch(&mut world, 9, 1, 10);
    world.open_period(2, 100_000);
    world.send(Event::TickRecurring { task: 9, period: 2 });
    assert_eq!(world.record(9).recurring.as_ref().expect("template").pending_period, Some(2));
    world.claim(10, 10);
    world.terminal(
        10,
        tasks::End::Finished { result: tasks::TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
    );
    world.settle(10);
    world.send(Event::RecurringBatch { task: 9, period: 2, numbers: Box::new([11]) });
    assert_eq!(world.record(9).recurring.as_ref().expect("template").pending_period, None);
    assert_eq!(world.record(11).funder, Funder::Recurring { project: 1, task: 9, period: 2 });
}

#[test]
fn a_standing_task_makes_delegates_across_many_periods_its_allotment_renewed_each_time() {
    let mut world = World::new(213, LIMITS);
    let mut master = recurring(9, RecurringOverlap::Skip);
    master.authority.budget.spend = 30;
    assert_eq!(world.make(Party::Deployment { project: 1 }, vec![master]), Reply::Made(vec![9]));
    for period in 1..=5 {
        world.open_period(period, 30);
        let child = period + 9;
        batch(&mut world, 9, period, child);
        let funder = Funder::Recurring { project: 1, task: 9, period };
        assert_eq!(world.record(child).funder, funder);
        assert!(matches!(world.records.get(&Key::Ledger(funder)),
            Some(Stored::Ledger(row)) if row.numbers.budget == 30 && row.numbers.reserved == 30));
        world.claim(child, child);
        world.terminal_cause(
            child,
            tasks::End::Finished {
                result: tasks::TaskResult::Report { words: Box::new([1]) },
                cancel_delegates: false,
            },
            Cause::Priced { cumulative: 10 },
        );
        world.settle(child);
        assert!(matches!(world.records.get(&Key::Ledger(funder)),
            Some(Stored::Ledger(row)) if row.numbers.spent_below == 10 && row.numbers.reserved == 0));
        world.restart();
    }
    for period in 1..5 {
        let old = Funder::Period { project: 1, period };
        assert!(matches!(world.records.get(&Key::Ledger(old)),
            Some(Stored::Ledger(row)) if row.closed && row.numbers.spent_below == 10));
    }
    assert_eq!(world.record(9).recurring.as_ref().expect("standing template").last_period, 5);
}

#[test]
fn a_subscribed_procedure_renews_while_its_old_delegate_remains_live() {
    let mut world = World::new(214, LIMITS);
    let mut watch = task(20, &[]);
    watch.executor = Executor::Procedure { connector: 1, code: 1 };
    watch.authority.budget.spend = 30;
    watch.numbers.budget = 30;
    assert_eq!(world.make(Party::Deployment { project: 1 }, vec![watch]), Reply::Made(vec![20]));
    let reply_to = world.to();
    world.send(Event::SubscribeTopic {
        reply_to,
        task: 20,
        subscription: Subscription { number: 100, kind: SubscriptionKind::Topic { connector: 1, topic: 1 } },
    });
    let mut old = task(21, &[]);
    old.authority.budget.spend = 30;
    old.numbers.budget = 30;
    old.funder = Funder::Task(20);
    assert_eq!(world.make(Party::Task(20), vec![old]), Reply::Made(vec![21]));
    world.open_period(1, 30);
    world.send(Event::RenewStanding { task: 20, period: 1 });
    let old_source = Funder::Recurring { project: 1, task: 20, period: 0 };
    assert_eq!(world.record(21).funder, old_source);
    assert_eq!(world.record(20).funder, Funder::Period { project: 1, period: 1 });
    assert_eq!(world.record(20).numbers, Numbers { budget: 30, spent: 0, spent_below: 0, reserved: 0 });
    let mut fresh = task(22, &[]);
    fresh.authority.budget.spend = 30;
    fresh.numbers.budget = 30;
    fresh.funder = Funder::Task(20);
    assert_eq!(world.make(Party::Task(20), vec![fresh]), Reply::Made(vec![22]));
    let mut old_grandchild = task(23, &[]);
    old_grandchild.authority.budget.spend = 5;
    old_grandchild.numbers.budget = 5;
    old_grandchild.funder = Funder::Task(21);
    assert_eq!(world.make(Party::Task(21), vec![old_grandchild]), Reply::Made(vec![23]));
    assert_eq!(world.record(20).made, 2, "old work leaves the fresh period's task allotment alone");
    assert!(matches!(world.records.get(&Key::Ledger(old_source)), Some(Stored::Ledger(row)) if row.made == 3));
    world.claim(23, 23);
    world.terminal(
        23,
        tasks::End::Finished { result: tasks::TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
    );
    world.settle(23);
    world.claim(21, 21);
    world.terminal_cause(
        21,
        tasks::End::Finished { result: tasks::TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 10 },
    );
    world.settle(21);
    assert!(matches!(world.records.get(&Key::Ledger(Funder::Period { project: 1, period: 0 })),
        Some(Stored::Ledger(row)) if row.closed && row.numbers.spent_below == 10));
    world.open_period(2, 30);
    world.send(Event::RenewStanding { task: 20, period: 2 });
    assert_eq!(world.record(22).funder, Funder::Recurring { project: 1, task: 20, period: 1 });
    world.restart();
    assert_eq!(world.record(20).funder, Funder::Period { project: 1, period: 2 });
    assert_eq!(world.record(22).funder, Funder::Recurring { project: 1, task: 20, period: 1 });
}

#[test]
fn a_persons_standing_procedure_renews_from_the_project_after_its_first_period() {
    let mut world = World::new(215, LIMITS);
    world.carve_pool(0, 30);
    let mut watch = task(30, &[]);
    watch.executor = Executor::Procedure { connector: 1, code: 1 };
    watch.authority.budget.spend = 30;
    watch.numbers.budget = 30;
    watch.funder = Funder::Pool { project: 1, person: 9, period: 0 };
    assert_eq!(world.make(Party::Person(9), vec![watch]), Reply::Made(vec![30]));
    let reply_to = world.to();
    world.send(Event::SubscribeTopic {
        reply_to,
        task: 30,
        subscription: Subscription { number: 101, kind: SubscriptionKind::Topic { connector: 1, topic: 1 } },
    });
    world.open_period(1, 30);
    world.send(Event::RenewStanding { task: 30, period: 1 });
    assert_eq!(world.record(30).funder, Funder::Period { project: 1, period: 1 });
    assert!(matches!(world.records.get(&Key::Ledger(Funder::Pool { project: 1, person: 9, period: 0 })),
        Some(Stored::Ledger(row)) if row.numbers.budget == 0 && row.numbers.reserved == 0));
    world.restart();
    assert_eq!(world.record(30).funder, Funder::Period { project: 1, period: 1 });
}
