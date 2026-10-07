use alloc::boxed::Box;
use skein_lib::{Env, List, Queue, Time, Wall};

use crate::{
    Adoption, Class, Classed, Config, Domain, Event, Hold, HoldMode, Limits, MAX_OUT, Origin, Path, PoolSpec, Record,
    Request, ResourceRole, ResourceSpec, SystemEvent, TopicSpec, step,
};

const LIMITS: Limits = Limits {
    tasks: 2,
    adoptions: 3,
    subscriptions: 3,
    pools: 1,
    resources: 2,
    topics: 1,
    kinds: 0,
    staged: 0,
    entries: 0,
    made: 0,
    resources_per_effect: 0,
    max_attempts: 1,
    write_lifetime: skein_lib::Duration::from_secs(1),
    clock_margin: skein_lib::Duration::from_secs(1),
    resources_per_task: 2,
    subscribers_per_topic: 2,
    lost_per_pool: 2,
    path_segments: 3,
    segment_bytes: 16,
};

fn path(parts: &[&[u8]]) -> Path {
    let mut segments = List::with_capacity(3);
    for part in parts {
        segments.push(Box::from(*part)).expect("fixture path fits");
    }
    Path::new(segments, LIMITS.segment_bytes).expect("fixture path has segments")
}

fn prefix() -> Path {
    path(&[b"unit"])
}

fn service() -> Path {
    path(&[b"unit", b"service"])
}

fn pool() -> Path {
    path(&[b"unit", b"pool"])
}

fn domain() -> Domain {
    Domain::new(
        Config {
            prefix: prefix(),
            resources: Box::from([
                ResourceSpec { path: service(), hold: Hold::Exclusive { mode: HoldMode::Wait }, writable: true },
                ResourceSpec { path: pool(), hold: Hold::Pooled { mode: HoldMode::Refuse }, writable: true },
            ]),
            pools: Box::from([PoolSpec { path: pool(), slots: 2 }]),
            topics: Box::from([TopicSpec { topic: 7 }]),
            deployment: 1,
            kinds: Box::from([]),
        },
        &LIMITS,
    )
}

fn run(domain: &mut Domain, event: Event) -> Box<[Request]> {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let mut out = Queue::with_capacity(MAX_OUT);
    step(domain, &env, event, &mut out);
    let mut requests = List::with_capacity(MAX_OUT);
    while let Some(request) = out.pop() {
        requests.push(request).expect("the request came from this queue");
    }
    requests.into_boxed()
}

#[test]
fn adoption_and_names_report_roles_and_holds_from_the_connector() {
    let mut connector = domain();
    assert_eq!(
        run(&mut connector, Event::Names { task: 4, project: 1, resources: Box::from([service()]) }),
        Box::from([Request::Unknown { task: 4, resource: service() }])
    );
    let adopted = run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Owned });
    assert_eq!(adopted.len(), 2);
    assert_eq!(adopted[1], Request::Adopted { project: 1, resource: service(), result: Adoption::Added });
    let named = run(&mut connector, Event::Names { task: 4, project: 1, resources: Box::from([service()]) });
    assert_eq!(named.len(), 2);
    assert_eq!(
        named[0],
        Request::Save { record: Record::Task { task: 4, project: 1, resources: Box::from([service()]) } }
    );
    assert_eq!(
        named[1],
        Request::Named {
            task: 4,
            resources: Box::from([crate::Named {
                path: service(),
                role: ResourceRole::Owned,
                hold: Hold::Exclusive { mode: HoldMode::Wait },
            }]),
        }
    );
    assert_eq!(connector.live_tasks(), 1);
    assert_eq!(
        run(&mut connector, Event::Unnamed { task: 4 }),
        Box::from([Request::Erase { key: crate::RecordKey::Task(4) }])
    );
    assert_eq!(connector.live_tasks(), 0);
}

#[test]
fn context_resources_cannot_be_upgraded_to_a_write_role() {
    let mut config = Config {
        prefix: prefix(),
        resources: Box::from([ResourceSpec { path: service(), hold: Hold::None, writable: false }]),
        pools: Box::from([]),
        topics: Box::from([]),
        deployment: 1,
        kinds: Box::from([]),
    };
    let mut connector = Domain::new(config.clone(), &LIMITS);
    assert_eq!(
        run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Owned }),
        Box::from([Request::Adopted { project: 1, resource: service(), result: Adoption::Refused }])
    );
    assert_eq!(
        run(&mut connector, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Context }).len(),
        2
    );
    config.resources = Box::from([]);
    let mut other = Domain::new(config, &LIMITS);
    assert_eq!(
        run(&mut other, Event::Adopt { project: 1, resource: service(), role: ResourceRole::Context }),
        Box::from([Request::Adopted { project: 1, resource: service(), result: Adoption::Unknown }])
    );
}

#[test]
fn shrinking_a_pool_reports_slots_and_lost_allocations_without_retracting_holds() {
    let mut connector = domain();
    assert_eq!(connector.slots(&pool()), Some(2));
    let output =
        run(&mut connector, Event::System(SystemEvent::Pool { path: pool(), slots: 1, lost: Box::from([9, 10]) }));
    assert_eq!(output.len(), 3);
    assert_eq!(output[0], Request::Save { record: Record::Pool { path: pool(), slots: 1 } });
    assert_eq!(output[1], Request::Slots { pool: pool(), slots: 1 });
    assert_eq!(output[2], Request::Drift { pool: pool(), tasks: Box::from([9, 10]) });
    assert_eq!(connector.slots(&pool()), Some(1));
}

#[test]
fn topic_news_is_classified_per_subscriber_and_own_echoes_are_dropped() {
    let mut connector = domain();
    run(&mut connector, Event::Subscribe { task: 1, topic: 7, wake_at: 5, keep_at: 2 });
    run(&mut connector, Event::Subscribe { task: 2, topic: 7, wake_at: 8, keep_at: 4 });
    assert!(
        run(&mut connector, Event::System(SystemEvent::News { topic: 7, importance: 9, origin: Origin::Own }))
            .is_empty()
    );
    assert_eq!(
        run(&mut connector, Event::System(SystemEvent::News { topic: 7, importance: 6, origin: Origin::Other })),
        Box::from([Request::News {
            topic: 7,
            subscribers: Box::from([Classed { task: 1, class: Class::Wake }, Classed { task: 2, class: Class::Keep }]),
        }])
    );
    assert!(
        run(&mut connector, Event::System(SystemEvent::News { topic: 7, importance: 1, origin: Origin::Other }))
            .is_empty()
    );
    assert_eq!(
        run(&mut connector, Event::Unsubscribe { task: 1, topic: 7 }),
        Box::from([Request::Erase { key: crate::RecordKey::Subscription { topic: 7, task: 1 } }])
    );
}

#[test]
fn restored_records_rebuild_adoptions_names_topics_and_pool_slots() {
    let mut original = domain();
    let mut records = List::with_capacity(4);
    for event in [
        Event::Adopt { project: 1, resource: service(), role: ResourceRole::Participating },
        Event::Names { task: 1, project: 1, resources: Box::from([service()]) },
        Event::Subscribe { task: 1, topic: 7, wake_at: 4, keep_at: 2 },
        Event::System(SystemEvent::Pool { path: pool(), slots: 1, lost: Box::from([]) }),
    ] {
        for request in run(&mut original, event) {
            match request {
                Request::Save { record } => records.push(record).expect("four changes made four records"),
                Request::Named { .. }
                | Request::Described { .. }
                | Request::EffectRefused { .. }
                | Request::Make { .. }
                | Request::Outcome { .. }
                | Request::System(..)
                | Request::Unknown { .. }
                | Request::Refused { .. }
                | Request::Adopted { .. }
                | Request::Slots { .. }
                | Request::Drift { .. }
                | Request::News { .. }
                | Request::Erase { .. } => {}
            }
        }
    }
    let mut cold = domain();
    for record in &records {
        assert!(run(&mut cold, Event::Restore { record: record.clone() }).is_empty());
    }
    assert_eq!(cold.slots(&pool()), Some(1));
    assert_eq!(cold.live_tasks(), 1);
    assert_eq!(
        run(&mut cold, Event::System(SystemEvent::News { topic: 7, importance: 3, origin: Origin::Other })),
        Box::from([Request::News { topic: 7, subscribers: Box::from([Classed { task: 1, class: Class::Keep }]) }])
    );
}
