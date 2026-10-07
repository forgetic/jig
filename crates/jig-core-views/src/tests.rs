use alloc::boxed::Box;

use skein_lib::{Env, List, Queue, Time, Token, Wall};

use crate::{Chunk, Domain, End, Event, Kind, Limits, Refusal, Request, Subject, max_out, step};

const LIMITS: Limits = Limits { runs: 2, watchers: 4, backlog: 2, report_bytes: 16, snapshot_bytes: 16, facts: 32 };

fn drive(domain: &mut Domain, event: Event) -> Box<[Request]> {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let mut out = Queue::with_capacity(max_out(&LIMITS));
    step(domain, &env, event, &mut out);
    let mut requests = List::with_capacity(max_out(&LIMITS));
    while let Some(request) = out.pop() {
        requests.push(request).expect("room for each request");
    }
    requests.into_boxed()
}

fn watch(domain: &mut Domain, watcher: u64, subject: Subject) -> Box<[Request]> {
    drive(domain, Event::Watch { watcher: Token::new(watcher), subject, snapshot: Box::from(&b"now"[..]) })
}

#[test]
fn a_watch_begins_with_its_snapshot() {
    let mut domain = Domain::new(&LIMITS);
    assert_eq!(
        watch(&mut domain, 1, Subject::Tree { task: Token::new(7) }).as_ref(),
        &[
            Request::Watching { watcher: Token::new(1) },
            Request::Deliver {
                watcher: Token::new(1),
                missed: 0,
                chunks: Box::new([Chunk::Snapshot { at: Time::ZERO, content: Box::from(&b"now"[..]) }])
            }
        ]
    );
}

#[test]
fn a_slow_watcher_is_told_what_it_missed() {
    let mut domain = Domain::new(&LIMITS);
    watch(&mut domain, 1, Subject::Tree { task: Token::new(7) });
    drive(&mut domain, Event::Started { task: Token::new(7), attempt: Token::new(1) });
    for number in 1..=3_u32 {
        assert!(drive(&mut domain, Event::Turn { task: Token::new(7), attempt: Token::new(1), number }).is_empty());
    }
    assert_eq!(
        drive(&mut domain, Event::Delivered { watcher: Token::new(1), done: true }).as_ref(),
        &[Request::Deliver {
            watcher: Token::new(1),
            missed: 2,
            chunks: Box::new([Chunk::Report {
                task: Token::new(7),
                attempt: Token::new(1),
                kind: Kind::Progress,
                at: Time::ZERO,
                content: Box::new(3_u32.to_be_bytes())
            }])
        }]
    );
}

#[test]
fn a_finished_run_ends_its_attempt_watch_after_delivery() {
    let mut domain = Domain::new(&LIMITS);
    drive(&mut domain, Event::Started { task: Token::new(7), attempt: Token::new(1) });
    watch(&mut domain, 1, Subject::Run { task: Token::new(7), attempt: Token::new(1) });
    assert!(drive(&mut domain, Event::Finished { task: Token::new(7) }).is_empty());
    assert_eq!(
        drive(&mut domain, Event::Delivered { watcher: Token::new(1), done: true }).as_ref(),
        &[Request::Ended { watcher: Token::new(1), end: End::Finished }]
    );
}

#[test]
fn a_new_attempt_fences_the_old_attempt() {
    let mut domain = Domain::new(&LIMITS);
    drive(&mut domain, Event::Started { task: Token::new(7), attempt: Token::new(1) });
    drive(&mut domain, Event::Started { task: Token::new(7), attempt: Token::new(2) });
    assert_eq!(
        watch(&mut domain, 1, Subject::Run { task: Token::new(7), attempt: Token::new(1) }).as_ref(),
        &[Request::Refused { watcher: Token::new(1), refusal: Refusal::Unknown }]
    );
    assert_eq!(watch(&mut domain, 2, Subject::Run { task: Token::new(7), attempt: Token::new(2) }).len(), 2);
    assert!(
        drive(
            &mut domain,
            Event::Reported {
                task: Token::new(7),
                attempt: Token::new(1),
                kind: Kind::Text,
                content: Box::from(&b"stale"[..])
            }
        )
        .is_empty()
    );
}

#[test]
fn goals_and_inbox_have_distinct_subjects() {
    let mut domain = Domain::new(&LIMITS);
    watch(&mut domain, 1, Subject::Goals { project: 2 });
    watch(&mut domain, 2, Subject::Inbox { party: 9 });
    drive(&mut domain, Event::Delivered { watcher: Token::new(1), done: true });
    drive(&mut domain, Event::Delivered { watcher: Token::new(2), done: true });
    assert_eq!(
        drive(
            &mut domain,
            Event::TaskPhase { task: Token::new(7), trees: Box::new([]), project: 2, phase: 1, priority: Some(3) }
        )
        .len(),
        1
    );
    assert_eq!(
        drive(&mut domain, Event::Inbox { party: 9, content: Box::from(&b"letter"[..]) }).as_ref(),
        &[Request::Deliver {
            watcher: Token::new(2),
            missed: 0,
            chunks: Box::new([Chunk::Inbox { party: 9, at: Time::ZERO, content: Box::from(&b"letter"[..]) }])
        }]
    );
}
