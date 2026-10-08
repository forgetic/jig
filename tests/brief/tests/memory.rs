//! A full inventory fits the child's declared worst case.

use jig_brief_world::LIMITS;
use jig_core_brief::{Core, GatherDomain, GatherEvent, Planned, gather_max_out, gather_step, gather_worst_case};
use skein_lib::{Env, Queue, Time, Token, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

#[test]
fn full_sections_fit_the_declared_worst_case() {
    let limits = LIMITS;
    let bound = gather_worst_case(&limits).expect("positive limits fit");
    let mut domain = GatherDomain::new(&limits);
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
    let mut out = Queue::with_capacity(gather_max_out(&limits));
    let meter = Meter::new();
    let sections: Box<[Planned]> = (0..limits.sections)
        .map(|index| Planned::Core {
            kind: Core::Results,
            text: vec![b'x'; usize::try_from(limits.brief_bytes).expect("small bytes")].into_boxed_slice(),
            limit: limits.brief_bytes,
            priority: u16::try_from(index).expect("small index"),
            required: index == 0,
        })
        .collect();
    meter.start();
    gather_step(
        &mut domain,
        &env,
        GatherEvent::Plan {
            brief: Token::new(1),
            budget: limits.brief_bytes,
            deadline: Time::ZERO.saturating_add(skein_lib::Duration::from_secs(1)),
            sections,
        },
        &mut out,
    );
    let measured = meter.end();
    while out.pop().is_some() {}
    meter.check(measured, bound, &"full sections");
    assert!(meter.held() <= bound);
}
