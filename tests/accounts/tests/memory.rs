use jig_accounts_world::LIMITS;
use jig_core_accounts::{Domain, Event, MAX_OUT, step, worst_case};
use skein_lib::{Duration, Env, Queue, Time, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

#[test]
fn full_account_and_deadline_tables_stay_within_the_worst_case() {
    let mut out = Queue::with_capacity(MAX_OUT);
    let meter = Meter::new();
    let mut domain = Domain::new(&LIMITS);
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let bound = worst_case(&LIMITS).expect("world limits fit");
    for account in 0..LIMITS.accounts {
        meter.start();
        step(&mut domain, &env, Event::Add { account, generation: 9, valid: Some(Duration::from_secs(100)) }, &mut out);
        let measured = meter.end();
        while out.pop().is_some() {}
        meter.check(measured, bound, &account);
    }
    assert_eq!(domain.accounts(), LIMITS.accounts);
}
