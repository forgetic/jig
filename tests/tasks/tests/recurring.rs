use jig_core_tasks::{
    self as tasks, Contract, Event, Executor, Funder, New, Numbers, Party, RecurringOverlap, RecurringTemplate,
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
