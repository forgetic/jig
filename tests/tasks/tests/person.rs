use jig_core_tasks::{self as tasks, Contract, Event, Executor, Party, PersonAddress, Phase, TaskResult, Verdict};
use jig_tasks_world::{LIMITS, Reply, World, task};

fn response(world: &mut World, event: impl FnOnce(skein_lib::ReplyTo) -> Event) -> Reply {
    let reply_to = world.to();
    world.send(event(reply_to));
    world.replies.last_key_value().expect("terminal reply").1.clone()
}

#[test]
fn a_person_task_is_taken_handed_back_and_answered() {
    let mut world = World::new(220, LIMITS);
    let mut new = task(1, &[]);
    new.executor = Executor::Person(PersonAddress::Role(7));
    new.contract = Contract::Verdict {
        choices: Box::new([Verdict { code: 1, words: 8, followups: 0 }, Verdict { code: 2, words: 8, followups: 0 }]),
    };
    assert_eq!(world.make(Party::Person(9), vec![new]), Reply::Made(vec![1]));
    assert_eq!(world.record(1).phase, Phase::Active(tasks::Active::Due));
    assert!(!world.activations.contains(&1), "a person task does not claim a worker");

    assert_eq!(response(&mut world, |reply_to| Event::TakePerson { reply_to, task: 1, person: 10 }), Reply::Done);
    assert_eq!(world.record(1).taken_by, Some(10));
    assert!(matches!(
        response(&mut world, |reply_to| Event::TakePerson { reply_to, task: 1, person: 11 }),
        Reply::Refused(_)
    ));
    world.restart();
    assert_eq!(world.record(1).taken_by, Some(10));
    assert_eq!(response(&mut world, |reply_to| Event::HandBackPerson { reply_to, task: 1, person: 10 }), Reply::Done);
    assert_eq!(world.record(1).taken_by, None);
    assert_eq!(response(&mut world, |reply_to| Event::TakePerson { reply_to, task: 1, person: 11 }), Reply::Done);
    assert!(matches!(
        response(&mut world, |reply_to| Event::AnswerPerson {
            reply_to,
            task: 1,
            person: 11,
            result: TaskResult::Verdict { code: 3, words: Box::new([]) },
        }),
        Reply::Refused(_)
    ));
    assert_eq!(
        response(&mut world, |reply_to| Event::AnswerPerson {
            reply_to,
            task: 1,
            person: 11,
            result: TaskResult::Verdict { code: 2, words: Box::new([1]) },
        }),
        Reply::Done
    );
    world.settle(1);
    assert!(matches!(world.results[&1], tasks::Ending::Done(TaskResult::Verdict { code: 2, .. })));
}

#[test]
fn a_direct_person_task_accepts_only_its_addressees_answer() {
    let mut world = World::new(221, LIMITS);
    let mut new = task(1, &[]);
    new.executor = Executor::Person(PersonAddress::Person(10));
    assert_eq!(world.make(Party::Person(9), vec![new]), Reply::Made(vec![1]));
    assert!(matches!(
        response(&mut world, |reply_to| Event::AnswerPerson {
            reply_to,
            task: 1,
            person: 11,
            result: TaskResult::Report { words: Box::new([1]) },
        }),
        Reply::Refused(_)
    ));
    assert_eq!(
        response(&mut world, |reply_to| Event::AnswerPerson {
            reply_to,
            task: 1,
            person: 10,
            result: TaskResult::Report { words: Box::new([1]) },
        }),
        Reply::Done
    );
    world.settle(1);
    assert!(matches!(world.results[&1], tasks::Ending::Done(TaskResult::Report { .. })));
}
