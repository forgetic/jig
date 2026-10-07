use jig_core_tasks::{
    self as tasks, Active, End, Event, Executor, Funder, Party, Phase, ProcedureDecision, TaskResult,
};
use jig_tasks_world::{LIMITS, Reply, World, task};
use skein_lib::ReplyTo;

fn step(world: &mut World, task: u64, step: u64, decision: ProcedureDecision) -> Reply {
    let reply_to = world.to();
    let key = reply_to.into_token().raw();
    world.send(Event::Procedure { reply_to: ReplyTo::new(skein_lib::Token::new(key)), task, step, decision });
    world.replies[&key].clone()
}

#[test]
fn a_procedure_steps_through_delegates_to_its_result() {
    let mut world = World::new(201, LIMITS);
    world.make(Party::Person(9), vec![task(1, &[])]);
    let mut procedure = task(2, &[]);
    procedure.executor = Executor::Procedure { connector: 1, code: 1 };
    procedure.funder = Funder::Task(1);
    world.make(Party::Task(1), vec![procedure]);
    assert_eq!(world.record(2).phase, Phase::Active(Active::Due));
    let mut child = task(3, &[]);
    child.funder = Funder::Task(2);
    assert_eq!(step(&mut world, 2, 1, ProcedureDecision::Delegate(Box::new([child]))), Reply::Made(vec![3]));
    assert_eq!(world.record(2).phase, Phase::Active(Active::Idle));
    world.claim(3, 3);
    world.terminal(3, End::Finished { result: TaskResult::Report { words: Box::new([7]) }, cancel_delegates: false });
    world.settle(3);
    assert_eq!(world.record(2).phase, Phase::Active(Active::Due));
    assert_eq!(
        step(&mut world, 2, 2, ProcedureDecision::Result(TaskResult::Report { words: Box::new([8]) })),
        Reply::Done
    );
    world.settle(2);
    assert!(
        matches!(world.results.get(&2), Some(tasks::Ending::Done(TaskResult::Report { words })) if words.as_ref() == [8])
    );
    let before = world.records.clone();
    world.restart();
    assert_eq!(world.records, before);
}
