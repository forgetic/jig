use jig_core_views::{self as views, Domain, Event, Subject};
use jig_views_world::LIMITS;
use skein_lib::{Env, Queue, Time, Token, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

#[test]
fn full_watches_and_backlogs_fit_the_declared_worst_case() {
    let meter = Meter::new();
    meter.start();
    let mut domain = Domain::new(&LIMITS);
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let mut out = Queue::with_capacity(views::max_out(&LIMITS));
    views::step(&mut domain, &env, Event::Started { task: Token::new(7), attempt: Token::new(1) }, &mut out);
    for watcher in 0..LIMITS.watchers {
        views::step(
            &mut domain,
            &env,
            Event::Watch {
                watcher: Token::new(u64::from(watcher)),
                subject: Subject::Tree { task: Token::new(7) },
                snapshot: vec![1; 32].into_boxed_slice(),
            },
            &mut out,
        );
        while out.pop().is_some() {}
    }
    for number in 1..=LIMITS.backlog + 1 {
        views::step(&mut domain, &env, Event::Turn { task: Token::new(7), attempt: Token::new(1), number }, &mut out);
        while out.pop().is_some() {}
    }
    let measured = meter.end();
    let bound = views::worst_case(&LIMITS).expect("valid limits");
    meter.check(measured, bound, &"full live views");
}
