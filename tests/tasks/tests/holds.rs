use jig_core_tasks::{
    Class, End, Event, Funder, Hold, HoldKind, Holding, Kind, MessageKind, Name, NoticeState, Party, Phase, Refusal,
    Stored, Subscription, SubscriptionKind, Taken, Writer,
};
use jig_tasks_world::{LIMITS, Reply, World, task};
use skein_lib::{Duration, ReplyTo, Token};

fn resource(value: u8) -> Holding {
    Holding::Write { resource: Name { connector: 1, path: Box::new([Box::new([value])]) }, kind: 1 }
}

fn pool(value: u8) -> Holding {
    Holding::Slot { pool: Name { connector: 1, path: Box::new([Box::new([value])]) }, kind: 2 }
}

fn configured_pool(seed: u64) -> World {
    let mut world = World::new(seed, LIMITS);
    world.configure_holds(1, vec![Kind { connector: 1, kind: 2, hold: HoldKind::Pooled { taken: Taken::Waits } }]);
    world
}

#[test]
fn two_tasks_want_the_last_slot_of_a_pool_and_the_second_takes_it_as_the_first_closes() {
    let mut world = configured_pool(308);
    let Holding::Slot { pool: name, .. } = pool(1) else { unreachable!() };
    world.send(Event::Slots { pool: name.clone(), slots: 1 });
    let mut first = task(1, &[]);
    first.holdings = Box::new([pool(1)]);
    let mut second = task(2, &[]);
    second.holdings = Box::new([pool(1)]);
    assert_eq!(world.make(Party::Person(1), vec![first, second]), Reply::Made(vec![1, 2]));
    assert!(world.record(1).holds_taken);
    assert!(!world.record(2).holds_taken);
    world.restart();
    assert!(!world.record(2).holds_taken);
    world.claim(1, 1);
    world.finish(1);
    world.settle(1);
    assert!(world.record(2).holds_taken);
}

#[test]
fn a_pool_shrinking_while_its_slots_are_held_admits_nobody_until_it_drains() {
    let mut world = configured_pool(309);
    let Holding::Slot { pool: name, .. } = pool(2) else { unreachable!() };
    world.send(Event::Slots { pool: name.clone(), slots: 2 });
    let mut first = task(1, &[]);
    first.holdings = Box::new([pool(2)]);
    let mut second = task(2, &[]);
    second.holdings = Box::new([pool(2)]);
    let mut third = task(3, &[]);
    third.holdings = Box::new([pool(2)]);
    assert_eq!(world.make(Party::Person(1), vec![first, second, third]), Reply::Made(vec![1, 2, 3]));
    assert!(world.record(1).holds_taken && world.record(2).holds_taken);
    assert!(!world.record(3).holds_taken);
    world.send(Event::Slots { pool: name.clone(), slots: 1 });
    world.restart();
    world.claim(1, 1);
    world.finish(1);
    world.settle(1);
    assert!(!world.record(3).holds_taken);
    world.claim(2, 2);
    world.finish(2);
    world.settle(2);
    assert!(world.record(3).holds_taken);
}

#[test]
fn a_lost_pool_allocation_holds_its_task_without_releasing_its_slot() {
    let mut world = configured_pool(310);
    let Holding::Slot { pool: name, .. } = pool(3) else { unreachable!() };
    world.send(Event::Slots { pool: name.clone(), slots: 1 });
    let mut holder = task(1, &[]);
    holder.holdings = Box::new([pool(3)]);
    let mut waiter = task(2, &[]);
    waiter.holdings = Box::new([pool(3)]);
    assert_eq!(world.make(Party::Person(1), vec![holder, waiter]), Reply::Made(vec![1, 2]));
    world.send(Event::AllocationGone { pool: name, task: 1 });
    assert!(matches!(world.record(1).phase, Phase::Held { why: Hold::Drift, .. }));
    assert!(world.record(1).holds_taken);
    assert!(!world.record(2).holds_taken);
}

fn configured(seed: u64) -> World {
    let mut world = World::new(seed, LIMITS);
    world.configure_holds(1, vec![Kind { connector: 1, kind: 1, hold: HoldKind::Exclusive { taken: Taken::Waits } }]);
    world
}

#[test]
fn two_tasks_each_wanting_two_holds_the_other_wants_never_wait_in_a_circle() {
    let mut world = configured(301);
    let mut first = task(1, &[]);
    first.holdings = Box::new([resource(1), resource(2)]);
    let mut second = task(2, &[]);
    second.holdings = Box::new([resource(2), resource(1)]);
    assert_eq!(world.make(Party::Person(1), vec![first, second]), Reply::Made(vec![1, 2]));
    assert!(world.record(1).holds_taken);
    assert!(!world.record(2).holds_taken);
    assert!(world.activations.contains(&1));
    assert!(!world.activations.contains(&2));
    world.restart();
    assert!(!world.record(2).holds_taken);
    world.claim(1, 1);
    world.finish(1);
    world.settle(1);
    assert!(world.record(2).holds_taken);
    assert!(world.activations.contains(&2));
}

#[test]
fn a_task_waiting_past_its_bound_for_a_hold_is_held_and_its_requester_told() {
    let mut world = configured(302);
    assert_eq!(world.make(Party::Person(1), vec![task(1, &[])]), Reply::Made(vec![1]));
    let mut holder = task(2, &[]);
    holder.holdings = Box::new([resource(1)]);
    let mut waiter = task(3, &[]);
    waiter.holdings = Box::new([resource(1)]);
    assert_eq!(world.make(Party::Task(1), vec![holder, waiter]), Reply::Made(vec![2, 3]));
    world.send(jig_core_tasks::Event::Subscribe {
        reply_to: ReplyTo::new(Token::new(500)),
        task: 1,
        subscription: Subscription {
            number: 10,
            kind: SubscriptionKind::Task { target: 3, held: true, result: false },
        },
    });
    world.restart();
    world.elapse(Duration::from_secs(11));
    assert!(matches!(world.record(3).phase, Phase::Held { why: Hold::HoldsWaited, .. }));
    assert!(!world.record(3).holds_taken);
    assert!(
        world
            .record(1)
            .inbox
            .iter()
            .any(|word| matches!(word.kind, MessageKind::Notice { target: 3, state: NoticeState::Held, .. }))
    );
}

#[test]
fn a_refusing_kind_rejects_the_whole_batch_and_names_the_holder() {
    let mut world = World::new(303, LIMITS);
    world.configure_holds(1, vec![Kind { connector: 1, kind: 1, hold: HoldKind::Exclusive { taken: Taken::Refuses } }]);
    let mut first = task(1, &[]);
    first.holdings = Box::new([resource(1)]);
    assert_eq!(world.make(Party::Person(1), vec![first]), Reply::Made(vec![1]));
    let mut second = task(2, &[]);
    second.holdings = Box::new([resource(1)]);
    assert!(matches!(world.make(Party::Person(1), vec![second]), Reply::Refused(problem)
        if problem.why == Refusal::HoldTaken && problem.blocked_by.as_deref() == Some(&[1][..])));
}

#[test]
fn the_higher_priority_goal_gets_a_freed_hold_first() {
    let mut world = configured(304);
    let mut holder = task(1, &[]);
    holder.holdings = Box::new([resource(1)]);
    let mut low = task(2, &[]);
    low.holdings = Box::new([resource(1)]);
    low.tracked = Some(1);
    let mut high = task(3, &[]);
    high.holdings = Box::new([resource(1)]);
    high.tracked = Some(9);
    assert_eq!(world.make(Party::Person(1), vec![holder]), Reply::Made(vec![1]));
    assert_eq!(world.make(Party::Person(1), vec![low, high]), Reply::Made(vec![2, 3]));
    world.claim(1, 1);
    world.finish(1);
    world.settle(1);
    assert!(world.record(3).holds_taken);
    assert!(!world.record(2).holds_taken);
}

#[test]
fn a_run_hands_a_held_resource_to_a_procedure_while_it_still_holds_the_writer_slot() {
    let mut world = configured(305);
    let held = resource(4);
    let Holding::Write { resource: name, .. } = &held else { unreachable!() };
    let name = name.clone();
    let mut parent = task(1, &[]);
    parent.numbers.budget = 200;
    parent.authority.budget.spend = 200;
    parent.holdings = Box::new([held.clone()]);
    assert_eq!(world.make(Party::Person(1), vec![parent]), Reply::Made(vec![1]));
    assert_eq!(world.claim_budget_writing(1, 1, 50, Box::new([name.clone()])), Reply::Done);

    let mut procedure = task(2, &[]);
    procedure.funder = Funder::Task(1);
    procedure.holdings = Box::new([held]);
    assert_eq!(world.make(Party::Task(1), vec![procedure]), Reply::Made(vec![2]));
    assert!(world.record(1).holdings.is_empty());
    assert!(world.record(2).holds_taken);
    assert!(world.records.values().any(|row| matches!(row, Stored::Writer(slot)
        if slot.resource == name && slot.writer == Writer::Run { task: 1, attempt: 1 })));
    assert_eq!(world.claim_budget_writing(2, 2, 50, Box::new([name.clone()])), Reply::WriterWaiting(name.clone()));

    world.send(Event::PreparationFailed { task: 2 });
    assert_eq!(world.terminal(1, End::Parked), Reply::Acknowledged(jig_core_tasks::Accepted::New));
    assert!(!world.records.values().any(|row| matches!(row, Stored::Writer(slot) if slot.resource == name)));
    world.elapse(Duration::from_secs(2));
    assert_eq!(world.claim_budget_writing(2, 3, 50, Box::new([name.clone()])), Reply::Done);
}

#[test]
fn a_lost_writer_keeps_its_slot_across_restart_until_its_connector_reads_afresh() {
    let mut world = configured(306);
    let held = resource(5);
    let Holding::Write { resource: name, .. } = &held else { unreachable!() };
    let name = name.clone();
    let mut task = task(1, &[]);
    task.holdings = Box::new([held]);
    assert_eq!(world.make(Party::Person(1), vec![task]), Reply::Made(vec![1]));
    assert_eq!(world.claim_budget_writing(1, 1, 50, Box::new([name.clone()])), Reply::Done);
    assert_eq!(world.terminal(1, End::Failed(Class::Lost)), Reply::Acknowledged(jig_core_tasks::Accepted::New));
    world.restart();
    assert!(world.records.values().any(|row| matches!(row, Stored::Writer(slot)
        if slot.resource == name && slot.lost)));
    world.send(Event::ReadAfresh { resource: name.clone() });
    assert!(!world.records.values().any(|row| matches!(row, Stored::Writer(slot) if slot.resource == name)));
}

#[test]
fn a_connector_effect_waits_for_a_run_and_owns_the_writer_until_it_settles() {
    let mut world = configured(307);
    let held = resource(6);
    let Holding::Write { resource: name, .. } = &held else { unreachable!() };
    let name = name.clone();
    let mut task = task(1, &[]);
    task.holdings = Box::new([held]);
    assert_eq!(world.make(Party::Person(1), vec![task]), Reply::Made(vec![1]));
    assert_eq!(world.claim_budget_writing(1, 1, 50, Box::new([name.clone()])), Reply::Done);
    let reply_to = world.to();
    world.send(Event::EffectInFlight { reply_to, task: 1, resource: name.clone(), entry: 99 });
    assert_eq!(world.replies.last_key_value().expect("effect reply").1, &Reply::WriterWaiting(name.clone()));
    assert_eq!(world.terminal(1, End::Parked), Reply::Acknowledged(jig_core_tasks::Accepted::New));
    let reply_to = world.to();
    world.send(Event::EffectInFlight { reply_to, task: 1, resource: name.clone(), entry: 99 });
    assert_eq!(world.replies.last_key_value().expect("effect reply").1, &Reply::Done);
    world.restart();
    assert!(world.records.values().any(|row| matches!(row, Stored::Writer(slot)
        if slot.resource == name && slot.writer == Writer::Effect { entry: 99 })));
    world.send(Event::EffectSettled { resource: name.clone(), entry: 99 });
    assert!(!world.records.values().any(|row| matches!(row, Stored::Writer(slot) if slot.resource == name)));
}
