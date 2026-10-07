use jig_fleet_world::turns::{self, Seen, Settings, Turns};
use skein_lib::{Duration, Time};
use skein_world::domain::{Referee, Verdict, assert_replays};

#[test]
fn turns_survive_reconnect_and_restart_under_commit_pressure() {
    let report = turns::run(Settings::new(3));
    assert_eq!(report.committed, 8);
    assert_eq!(report.replays, 2);
    assert!(report.parent_busy > 0 && report.capacity_busy > 0);
}

#[test]
fn turns_from_cancelled_attempts_are_forgotten_without_commitment() {
    let report = turns::run(Settings { cancel: true, ..Settings::new(5) });
    assert!(report.committed < 8);
}

#[test]
fn turn_world_replays_and_facts_change_nothing() {
    assert_replays(19, 4, |seed| {
        let report = turns::run(Settings::new(seed));
        (report.trace.clone(), report)
    });
    for seed in 0..4 {
        assert_eq!(
            turns::run(Settings { facts: 0, ..Settings::new(seed) }),
            turns::run(Settings { facts: 100, ..Settings::new(seed) })
        );
    }
}

fn bad(seen: &[Seen]) -> String {
    let mut referee = Referee::new(Turns::default());
    for &event in seen {
        referee.observe(Time::ZERO, event, &mut Vec::new());
    }
    let Verdict::Failed(failure) = referee.verdict() else { panic!("the referee rejects the violation") };
    failure.why
}

#[test]
fn referee_rejects_early_forgetting_duplicate_handoff_and_changed_body() {
    let made = Seen::Produced { turn: 1, body: 42 };
    let heard = Seen::Handed { turn: 1, body: 42 };
    assert!(bad(&[made, Seen::Forgot { turn: 1 }]).contains("only a committed or fenced"));
    assert!(bad(&[made, heard, heard]).contains("handed once"));
    assert!(bad(&[made, Seen::Handed { turn: 1, body: 99 }]).contains("opaque turn body"));
    assert!(bad(&[made, heard, Seen::Committed { turn: 1 }, Seen::Restart, heard]).contains("committed turn"));
}

#[test]
fn referee_requires_every_kept_turn_to_be_forgotten_in_time() {
    let mut referee = Referee::new(Turns::default());
    referee.observe(Time::ZERO, Seen::Produced { turn: 1, body: 42 }, &mut Vec::new());
    referee.fire(Time::ZERO.saturating_add(Duration::from_secs(5)), &mut Vec::new());
    assert!(matches!(referee.verdict(), Verdict::Failed(_)));
}

#[test]
fn a_worker_whose_stop_bound_reaches_the_engines_grace_is_refused_at_its_hello() {
    for secs in [20, 21] {
        let report = turns::run(Settings { stop_bound: Duration::from_secs(secs), ..Settings::new(1) });
        assert!(report.refused);
        assert_eq!(report.committed, 0);
    }
}
