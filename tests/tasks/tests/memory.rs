use jig_core_tasks::{
    self as tasks, Active, AuthorityExecutor, Contract, Domain, End, Event, Funder, Grant, Hold, Last, Limits,
    Parameter, Party, Pattern, Request, Stored, TaskResult, Verdict,
};
use jig_tasks_world::{LIMITS, task};
use skein_lib::{Env, Queue, ReplyTo, Time, Token, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

struct Measured {
    d: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
    meter: Meter,
    bound: u64,
    serial: u64,
    refused: bool,
}

impl Measured {
    fn new(limits: &Limits) -> Measured {
        let limits = *limits;
        let out = Queue::with_capacity(tasks::max_out(&limits));
        let meter = Meter::new();
        let d = Domain::new(&limits, 1, Box::new([1]));
        Measured {
            d,
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            out,
            meter,
            bound: tasks::worst_case(&limits).expect("admitted bounds"),
            serial: 0,
            refused: false,
        }
    }

    fn to(&mut self) -> ReplyTo {
        self.serial += 1;
        ReplyTo::new(Token::new(self.serial))
    }

    fn event(&mut self, event: Event) {
        self.meter.start();
        tasks::step(&mut self.d, &self.env, event, &mut self.out);
        let measured = self.meter.end();
        self.refused = false;
        while let Some(request) = self.out.pop() {
            self.refused |= matches!(request, Request::Refused { .. });
            drop(request);
        }
        self.meter.check(measured, self.bound, &self.env.limits);
        self.d.reclaim();
    }

    fn bootstrap(&mut self) {
        self.event(Event::Restored);
        for period in [0, 7] {
            let reply_to = self.to();
            self.event(Event::OpenPeriod { reply_to, project: 1, period, budget: 100_000 });
        }
        let reply_to = self.to();
        self.event(Event::CarvePool { reply_to, project: 1, person: 9, period: 7, budget: 100_000 });
    }

    fn fire(&mut self) {
        self.env.now = self.env.now.saturating_add(skein_lib::Duration::from_secs(1));
        self.env.wall = Wall::from_nanos(self.env.now.as_nanos());
        self.meter.start();
        tasks::fire(&mut self.d, &self.env, &mut self.out);
        let measured = self.meter.end();
        while let Some(request) = self.out.pop() {
            drop(request);
        }
        self.meter.check(measured, self.bound, &self.env.limits);
        self.d.reclaim();
    }

    fn claim(&mut self, number: u64) {
        let reply_to = self.to();
        self.event(Event::Prepare { reply_to, task: number });
        let reply_to = self.to();
        self.event(Event::Claim { reply_to, task: number, attempt: number, budget: 100 });
        self.event(Event::Started { task: number, attempt: number });
    }
}

#[test]
fn saturated_payloads_graph_backoff_held_closing_retirement_and_restore_fit() {
    for l in [
        Limits {
            tasks: 4,
            tree_tasks: 4,
            depth: 2,
            delegates: 3,
            batch: 4,
            dependencies: 3,
            inputs: 4,
            parameters: 4,
            ..LIMITS
        },
        Limits {
            tasks: 1,
            tree_tasks: 1,
            depth: 0,
            delegates: 0,
            batch: 1,
            dependencies: 0,
            inputs: 1,
            parameters: 1,
            facts: 0,
            ..LIMITS
        },
    ] {
        let mut m = Measured::new(&l);
        m.bootstrap();
        let batch = (1..=u64::from(l.tasks))
            .map(|number| {
                let mut new = filled(&l, number);
                new.funder = Funder::Pool { project: 1, person: 9, period: 7 };
                new
            })
            .collect::<Vec<_>>();
        let reply_to = m.to();
        m.event(Event::Make { reply_to, creator: Party::Person(1), batch: batch.into_boxed_slice() });
        assert!(!m.refused);
        for number in 1..=u64::from(l.tasks) {
            m.claim(number);
            let reply_to = m.to();
            m.event(Event::Activation {
                saved: None,
                reply_to,
                task: number,
                attempt: number,
                end: End::Refused,
                cause: tasks::Cause::Unpriced,
            });
            m.fire();
            let reply_to = m.to();
            m.event(Event::Prepare { reply_to, task: number });
            let reply_to = m.to();
            m.event(Event::Claim { reply_to, task: number, attempt: number + 10, budget: 100 });
            let reply_to = m.to();
            m.event(Event::Turn {
                reply_to,
                task: number,
                attempt: number + 10,
                turn: 1,
                read: None,
                offered: None,
                cumulative: 7,
            });
            let reply_to = m.to();
            m.event(Event::Activation {
                saved: None,
                reply_to,
                task: number,
                attempt: number + 10,
                end: End::Finished {
                    result: TaskResult::Verdict { code: 0, words: vec![1; l.result_bytes as usize].into_boxed_slice() },
                    cancel_delegates: false,
                },
                cause: tasks::Cause::Priced { cumulative: 10 },
            });
            m.event(Event::Hold { task: number, why: Hold::Effects });
            m.event(Event::EffectsSettled { task: number });
            m.event(Event::Settled { task: number });
        }
    }
}

#[test]
fn full_delegate_tree_dependency_edges_and_cold_restored_claims_fit() {
    let l = Limits { tasks: 4, tree_tasks: 4, delegates: 3, batch: 3, dependencies: 3, inputs: 0, ..LIMITS };
    let mut source = jig_tasks_world::World::new(2, l);
    source.make(Party::Person(1), vec![task(1, &[])]);
    source.claim(1, 1);
    source.make(Party::Task(1), vec![task(2, &[]), task(3, &[2]), task(4, &[2, 3])]);
    source.claim(2, 2);
    let mut m = Measured::new(&l);
    for row in source.records.values() {
        m.event(Event::Restore { record: row.clone() });
    }
    m.event(Event::Restored);
    let reply_to = m.to();
    m.event(Event::Activation {
        saved: None,
        reply_to,
        task: 1,
        attempt: 1,
        end: End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true },
        cause: tasks::Cause::Unpriced,
    });
    let reply_to = m.to();
    m.event(Event::Activation {
        reply_to,
        task: 2,
        attempt: 2,
        end: End::Parked,
        saved: None,
        cause: tasks::Cause::Unpriced,
    });
    for task in [3, 4, 2, 1] {
        m.event(Event::EffectsSettled { task });
        m.event(Event::Settled { task });
    }
    assert!(tasks::worst_case(&Limits { tasks: u32::MAX, ..l }).is_none());
    let mut w = jig_tasks_world::World::new(3, l);
    w.make(Party::Person(1), vec![task(10, &[])]);
    w.claim(10, 10);
    w.finish(10);
    w.settle(10);
    assert!(
        matches!(w.records.get(&tasks::Key::Ended(10)),Some(Stored::Ended(record)) if record.phase!=tasks::Phase::Active(Active::Due))
    );
}

fn filled(limits: &Limits, number: u64) -> jig_core_tasks::New {
    let limits = *limits;
    let mut new = task(number, &[]);
    new.spec.words = vec![1; usize::try_from(limits.spec_bytes).expect("small bound")].into_boxed_slice();
    new.spec.parameters =
        vec![Parameter::Number { name: 1, value: 1 }; usize::try_from(limits.parameters).expect("small bound")]
            .into_boxed_slice();

    new.contract = Contract::Verdict {
        choices: (0..limits.contract_choices)
            .map(|code| Verdict { code, words: limits.result_bytes, followups: 0 })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };
    new.authority.grants = (0..limits.authority_grants)
        .map(|kind| Grant {
            connector: 0,
            kind: u16::try_from(kind).expect("small bound"),
            pattern: Pattern {
                segments: (0..limits.authority_segments)
                    .map(|_| Box::new([]) as Box<[u8]>)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                last: Last::Exact(if kind == 0 {
                    vec![1; usize::try_from(limits.authority_bytes).expect("small bound")].into_boxed_slice()
                } else {
                    Box::new([])
                }),
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    new.authority.delegation.kinds =
        vec![AuthorityExecutor::Charter(1); usize::try_from(limits.executor_kinds).expect("small bound")]
            .into_boxed_slice();
    new
}

#[test]
fn saturated_finite_sources_and_oversized_refusals_fit_without_input_copies() {
    let l = Limits { tasks: 1, funders: 3, ..LIMITS };
    let mut m = Measured::new(&l);
    m.bootstrap();
    let reply_to = m.to();
    m.event(Event::OpenPeriod { reply_to, project: 1, period: 8, budget: 100 });
    assert!(m.refused);
    let reply_to = m.to();
    let mut new = task(1, &[]);
    new.funder = Funder::Pool { project: 1, person: 9, period: 7 };
    m.event(Event::Make { reply_to, creator: Party::Person(1), batch: Box::new([new]) });
    m.claim(1);
    let reply_to = m.to();
    m.event(Event::Activation {
        saved: None,
        reply_to,
        task: 1,
        attempt: 1,
        end: End::Finished {
            result: TaskResult::Report { words: vec![1; 1024].into_boxed_slice() },
            cancel_delegates: false,
        },
        cause: tasks::Cause::Priced { cumulative: 5 },
    });
    assert!(m.refused);
    let reply_to = m.to();
    m.event(Event::Turn { reply_to, task: 1, attempt: 1, turn: 1, read: Some(1), offered: None, cumulative: 5 });
    assert!(m.refused);
}

#[test]
fn borrowed_stored_bytes_matches_allocator_for_every_retained_row() {
    let mut world = jig_tasks_world::World::new(80, LIMITS);
    world.make(Party::Person(1), vec![task(1, &[])]);
    let mut record = world.record(1).clone();
    let new = filled(&LIMITS, 1);
    record.spec = new.spec;
    record.spec.parameters = Box::new([
        Parameter::Bytes { name: 1, value: vec![1; 4096].into_boxed_slice() },
        Parameter::Number { name: 2, value: 5 },
        Parameter::Resource { name: 3, connector: 1, resource: 2 },
    ]);
    record.authority = new.authority;
    record.authority.grants[0].pattern.segments = Box::new([Box::new([1_u8, 2]) as Box<[u8]>, Box::new([3])]);
    record.authority.grants[1].pattern.last = Last::Open(Box::new([4, 5]));
    record.contract = new.contract;
    record.dependencies = Box::new([1, 2, 3]);
    record.waiting_on = Box::new([2, 3]);
    record.delegates = Box::new([4, 5]);
    let ending = tasks::Ending::Cancelled {
        reason: Box::new([1, 2]),
        result: Some(TaskResult::Report { words: Box::new([3, 4, 5]) }),
    };
    record.phase = tasks::Phase::Held {
        was: tasks::Was::Closing(tasks::Closing { stage: tasks::Stage::Delegates, ending: ending.clone() }),
        why: Hold::Budget,
    };
    record.escalation = tasks::Escalation::Rejected { revision: 1, by: 1, reason: Box::new([6, 7, 8, 9]) };
    let mut rows = world.records.values().cloned().collect::<Vec<_>>();
    rows.push(Stored::Live(Box::new(record.clone())));
    record.phase = tasks::Phase::Ended(ending);
    rows.push(Stored::Ended(Box::new(record)));
    for row in rows {
        let meter = Meter::new();
        let cloned = row.clone();
        let actual = meter.held();
        assert_eq!(tasks::stored_bytes(&cloned), Some(actual));
        assert_eq!(meter.held(), actual, "borrowed row measurement allocates nothing");
    }
}
