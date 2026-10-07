//! Both engine slots occupied under the host's checked memory bound
//! (domain/hosts.md, section 5; programming-model.md, section 6.3).

use jig_local_host::{
    self as host, Assignment, Budget, Charter, Contract, Event, Grant, Model, Prices, Section, TextRule,
};
use skein_lib::{Duration, Env, Queue, Time, Wall};
use skein_world::domain::heap::{self, Meter};
use smith_domain as smith;

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

fn assignment(task: u64) -> Assignment {
    Assignment {
        task,
        attempt: 1,
        charter: Charter {
            instructions: b"Answer the brief.".as_slice().into(),
            tools: Box::new([]),
            wait: true,
            agents: false,
            contract: Contract {
                report: Some(TextRule { max: 128, fields: Box::new([]) }),
                failure: None,
                verdicts: Box::new([]),
            },
            budget: Budget { turns: 4, spend: 1, time: Duration::from_secs(60) },
            model: Model {
                prices: Prices { input: 0, cached: 0, output: 0, unit: 1 },
                dialect: 0,
                account: 0,
                endpoint: 0,
                name: b"fake-1".as_slice().into(),
                max_tokens: 100,
            },
            models: Box::new([]),
            waiting: Duration::from_secs(1),
            resumes: true,
        },
        brief: Box::new([Section { title: b"Task".as_slice().into(), text: b"Report the result.".as_slice().into() }]),
        transcript: None,
        calls: Box::new([]),
        grants: Box::new([Grant { account: 0, generation: 1, valid: Duration::from_secs(120) }]),
    }
}

#[test]
fn every_engine_slot_busy_stays_within_the_hosts_worst_case() {
    let smith = smith_agent_world::Settings::calm(31).limits;
    let largest = smith::max_turn_bytes(&smith).expect("bounded turn");
    let limits = host::Limits {
        slots: 2,
        smith,
        window: smith::Window { turns: 2, bytes: largest.checked_mul(2).expect("two turns") },
        cancel_grace: Duration::from_secs(2),
    };
    let bound = host::worst_case(&limits).expect("host limits have a bound");
    let first = assignment(1);
    let second = assignment(2);
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
    let mut out = Queue::with_capacity(host::max_out(&limits));
    let meter = Meter::new();
    let mut domain = host::Host::new(&limits, Box::new([smith::run::charter::Endpoint(0)]), 31);
    for (slot, assignment) in [(0, first), (1, second)] {
        meter.start();
        host::step(&mut domain, &env, Event::Assign { slot, assignment: Box::new(assignment) }, &mut out);
        let measured = meter.end();
        while out.pop().is_some() {}
        meter.check(measured, bound, &limits);
    }
    assert_eq!(domain.hosted(), 2);
    assert!(meter.held() <= bound, "both busy slots fit the declared worst case");
}
