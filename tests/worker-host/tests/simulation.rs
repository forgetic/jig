//! The host in its world: scenarios, replay, and a sweep of random worlds.

use skein_lib::Duration;
use jig_worker_host::Limits;
use jig_worker_host_world::{Outage, Settings, Span, Stats, World};

const ITERATIONS: u32 = 200_000;

fn run(settings: &Settings) -> World {
    let mut world = World::new(*settings);
    world.run(ITERATIONS);
    world
}

fn count(stats: &Stats, ending: &str) -> u32 {
    stats.endings.get(ending).copied().unwrap_or(0)
}

#[test]
fn a_calm_world_ends_or_parks_every_run_and_settles() {
    let world = run(&Settings::calm(1));
    let stats = world.stats();
    let (ended, parked) = (count(&stats, "ended"), count(&stats, "parked"));
    assert!(ended > 0 && parked > 0, "runs end, or park when idle: {stats:?}");
    assert_eq!(ended + parked, 8, "and nothing else: {stats:?}");
    assert!(stats.parent.relays > 0 && stats.parent.deliveries > 0 && stats.parent.yields > 0, "{stats:?}");
    assert!(stats.parent.saves > 0, "work that did not land is saved: {stats:?}");
    assert_eq!(stats.parent.releases, 8, "every workspace is released: {stats:?}");
    assert!(stats.engine.events > 0, "inbound events went down: {stats:?}");
}

#[test]
fn assignments_beyond_the_slots_are_refused_as_busy() {
    let calm = Settings::calm(2);
    let settings = Settings {
        host: Limits { slots: 1, ..calm.host },
        engine: jig_worker_host_world::engine::Script { spacing: Span::millis(0, 100), ..calm.engine },
        ..calm
    };
    let stats = run(&settings).stats();
    assert!(count(&stats, "refused: busy") > 0, "{stats:?}");
    assert!(count(&stats, "ended") > 0, "{stats:?}");
    assert_eq!(stats.peak, 1, "one slot: {stats:?}");
}

#[test]
fn assignments_beyond_the_limits_are_refused_as_invalid_and_never_admitted() {
    let calm = Settings::calm(3);
    let settings =
        Settings { engine: jig_worker_host_world::engine::Script { invalid: 1000, ..calm.engine }, ..calm };
    let stats = run(&settings).stats();
    assert_eq!(count(&stats, "refused: invalid"), 8, "{stats:?}");
    assert_eq!(stats.parent.prepares, 0, "{stats:?}");
}

#[test]
fn cancelled_runs_are_stopped_saved_and_their_calls_answered_as_unavailable() {
    let calm = Settings::calm(4);
    let settings = Settings {
        engine: jig_worker_host_world::engine::Script {
            cancels: 1000,
            cancel_after: Span::millis(0, 20_000),
            ..calm.engine
        },
        parent: jig_worker_host_world::parent::Script { relays: 600, late: 1000, ..calm.parent },
        ..calm
    };
    let stats = run(&settings).stats();
    assert!(count(&stats, "failed: cancelled, engine") > 0, "{stats:?}");
    assert!(stats.unavailable > 0, "{stats:?}");
    assert!(stats.engine.unanswered > 0 || stats.parent.late_calls > 0, "{stats:?}");
}

#[test]
fn stale_attempts_change_nothing() {
    let calm = Settings::calm(5);
    let settings = Settings { engine: jig_worker_host_world::engine::Script { stale: 1000, ..calm.engine }, ..calm };
    let stats = run(&settings).stats();
    assert!(stats.stale > 8, "{stats:?}");
    assert_eq!(count(&stats, "ended") + count(&stats, "parked"), 8, "{stats:?}");
}

#[test]
fn losing_contact_past_the_grace_cancels_every_run_and_saves_their_work_first() {
    let calm = Settings::calm(6);
    let settings = Settings {
        engine: jig_worker_host_world::engine::Script {
            assignments: 4,
            spacing: Span::millis(0, 1_000),
            ..calm.engine
        },
        parent: jig_worker_host_world::parent::Script { steps: 30, ..calm.parent },
        outage: Some(Outage {
            at: Span::millis(5_000, 5_000),
            length: Span::millis(60_000, 60_000),
            grace: Duration::from_secs(10),
        }),
        ..calm
    };
    let stats = run(&settings).stats();
    assert_eq!(stats.cancel_alls, 1, "{stats:?}");
    assert!(count(&stats, "failed: cancelled, contact") > 0, "{stats:?}");
    assert!(stats.parent.saves > 0, "{stats:?}");
    assert_eq!(stats.reports, 1, "{stats:?}");
}

#[test]
fn losing_contact_within_the_grace_keeps_the_runs_and_reports_them() {
    let calm = Settings::calm(7);
    let settings = Settings {
        engine: jig_worker_host_world::engine::Script {
            assignments: 4,
            spacing: Span::millis(0, 1_000),
            ..calm.engine
        },
        parent: jig_worker_host_world::parent::Script { steps: 30, ..calm.parent },
        outage: Some(Outage {
            at: Span::millis(5_000, 5_000),
            length: Span::millis(5_000, 5_000),
            grace: Duration::from_secs(10),
        }),
        ..calm
    };
    let stats = run(&settings).stats();
    assert_eq!((stats.cancel_alls, stats.reports), (0, 1), "{stats:?}");
    assert_eq!(count(&stats, "ended") + count(&stats, "parked"), 4, "kept, and none failed: {stats:?}");
}

#[test]
fn shutdown_cancels_every_run_and_takes_no_more() {
    let calm = Settings::calm(8);
    let settings = Settings {
        parent: jig_worker_host_world::parent::Script { steps: 30, ..calm.parent },
        shutdown: Some(Span::millis(30_000, 30_000)),
        ..calm
    };
    let stats = run(&settings).stats();
    assert_eq!(stats.cancel_alls, 1, "{stats:?}");
    assert!(count(&stats, "failed: cancelled, shutdown") > 0, "{stats:?}");
    assert!(count(&stats, "refused: busy") > 0, "assignments after shutdown are refused: {stats:?}");
}

/// A rough world, varied by seed: some shut down, some lose contact for
/// longer than the grace, some let runs make several calls at once and hold
/// several inbound events.
fn rough(seed: u64) -> Settings {
    let rough = Settings::rough(seed);
    let shutdown = if seed.is_multiple_of(4) { Some(Span::millis(20_000, 120_000)) } else { None };
    let host = if seed.is_multiple_of(2) { rough.host } else { Limits { run_calls: 3, held: 3, ..rough.host } };
    Settings { shutdown, host, ..rough }
}

#[test]
fn a_seed_replays_to_the_same_run() {
    let trace = skein_world::domain::assert_replays(11, 12, |seed| {
        let world = run(&rough(seed));
        (world.trace().to_vec(), (world.stats(), world.now()))
    });
    assert!(trace.len() > 50, "the world did something: {} events", trace.len());
}

#[test]
fn facts_change_nothing_when_none_are_kept() {
    let settings = rough(13);
    let kept = run(&settings);
    let none = run(&Settings { host: Limits { facts: 0, ..settings.host }, ..settings });
    assert_eq!(kept.trace(), none.trace(), "nothing depends on whether a fact is kept");
    assert!(none.stats().facts_lost > 0);
}
