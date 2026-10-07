//! The fleet's world, scenario by scenario, and random worlds swept over many
//! seeds.

use std::collections::BTreeMap;

use jig_core_fleet::Limits;
use jig_fleet_world::{LIMITS, Settings, Span, Stats, World};
use skein_lib::Duration;
use skein_world::domain::assert_replays;

const ITERATIONS: u32 = 400_000;

fn run(settings: &Settings) -> World {
    let mut world = World::new(*settings);
    world.run(ITERATIONS);
    world
}

fn ending(stats: &Stats, ending: &str) -> u32 {
    stats.endings.get(ending).copied().unwrap_or(0)
}

#[test]
fn calm_worlds_settle_with_every_run_answered() {
    let mut endings = BTreeMap::new();
    for seed in 0..5 {
        let world = run(&Settings::calm(seed));
        let stats = world.stats();
        assert!(stats.starts >= 12, "every item starts: {stats:?}");
        assert!(stats.admitted > 0 && stats.calls > 0 && stats.replies > 0 && stats.delivered > 0, "{stats:?}");
        for name in ["lost", "cancelled", "replaced", "fenced", "stray", "busy", "refused", "turned away"] {
            assert_eq!(ending(&stats, name), 0, "nothing goes wrong in a calm world: {stats:?}");
        }
        let (ends, sent) = world.judged();
        assert!(ends > 0 && sent > 0, "the referee judged something");
        for (ending, count) in stats.endings {
            *endings.entry(ending).or_insert(0) += count;
        }
    }
    for name in ["ended", "parked", "failed"] {
        assert!(endings.get(name).copied().unwrap_or(0) > 0, "{name} reached: {endings:?}");
    }
}

#[test]
fn a_seed_replays_to_the_same_run() {
    let trace = assert_replays(7, 8, |seed| {
        let world = run(&Settings::random(seed));
        (world.trace().to_vec(), (world.stats(), world.now()))
    });
    assert!(trace.len() > 100, "the world did something");
}

#[test]
fn facts_change_nothing() {
    for seed in 0..10 {
        let settings = Settings::random(seed);
        let few = run(&Settings { limits: Limits { facts: 1, ..settings.limits }, ..settings });
        let many = run(&Settings { limits: Limits { facts: 256, ..settings.limits }, ..settings });
        assert_eq!(few.trace(), many.trace(), "seed {seed}: facts dropped or kept change nothing");
    }
}

#[test]
fn workers_back_within_the_grace_keep_their_runs() {
    for seed in 0..1 {
        let calm = Settings::calm(seed);
        let settings = Settings { drops: 3, away: Span::millis(100, 5000), run: Span::millis(5000, 20_000), ..calm };
        let stats = run(&settings).stats();
        // An assignment in flight as its channel drops is lost, and only
        // presumed so past the grace: no worker hosted it.
        assert_eq!(stats.lost_hosted, 0, "seed {seed}: nothing a worker hosts is lost: {stats:?}");
    }
}

#[test]
fn workers_that_never_come_back_lose_their_runs() {
    let mut lost = 0;
    for seed in 0..1 {
        let calm = Settings::calm(seed);
        let settings = Settings { drops: 2, never_back: 1000, run: Span::millis(5000, 20_000), workers: 4, ..calm };
        lost += ending(&run(&settings).stats(), "lost");
    }
    assert!(lost > 0, "a worker gone for good loses its runs");
}

#[test]
fn an_engine_restart_adopts_what_the_workers_still_host() {
    let mut endings = BTreeMap::new();
    for seed in 0..20 {
        let calm = Settings::calm(seed);
        let settings = Settings { restarts: 1, run: Span::millis(5000, 20_000), ..calm };
        let stats = run(&settings).stats();
        assert_eq!(stats.restarts, 1, "seed {seed}: the engine restarted");
        for (ending, count) in stats.endings {
            *endings.entry(ending).or_insert(0) += count;
        }
    }
    for ending in ["adopted", "found", "ended"] {
        assert!(endings.get(ending).copied().unwrap_or(0) > 0, "{ending} reached: {endings:?}");
    }
}

#[test]
fn cancels_and_replacements_fence_their_attempts() {
    let mut endings = BTreeMap::new();
    for seed in 0..20 {
        let calm = Settings::calm(seed);
        let settings = Settings { cancels: 400, replaces: 600, run: Span::millis(2000, 10_000), ..calm };
        for (ending, count) in run(&settings).stats().endings {
            *endings.entry(ending).or_insert(0) += count;
        }
    }
    for ending in ["cancelled", "replaced", "failed", "dropped"] {
        assert!(endings.get(ending).copied().unwrap_or(0) > 0, "{ending} reached: {endings:?}");
    }
}

#[test]
fn a_tight_fleet_refuses_at_the_entrance_and_settles() {
    let calm = Settings::calm(3);
    let limits = Limits { attempts: 2, calls: 1, ..LIMITS };
    let settings =
        Settings { limits, items: 8, item_gap: Span::millis(1, 10), timeout: Duration::from_secs(20), ..calm };
    let stats = run(&settings).stats();
    assert!(ending(&stats, "refused") > 0, "starts beyond the room are refused: {stats:?}");
}
