//! The referee of the fleet's world, fed observations by hand as the world
//! would feed them, fails a run that breaks an expectation, and says why; and
//! passes one that keeps them.

use jig_fleet_world::referee::{Down, End, Fleet, Kind, Said, Seen};
use skein_lib::{Duration, Time};
use skein_world::domain::{Referee, Verdict};

fn at(secs: u64) -> Time {
    Time::ZERO.saturating_add(Duration::from_secs(secs))
}

fn referee() -> Referee<Fleet> {
    Referee::new(Fleet::new(Duration::from_secs(60)))
}

fn see(referee: &mut Referee<Fleet>, secs: u64, seen: Seen) {
    referee.observe(at(secs), seen, &mut Vec::new());
}

fn why(referee: &Referee<Fleet>) -> String {
    let Verdict::Failed(failure) = referee.verdict() else { panic!("the referee failed the run") };
    failure.why
}

const SAID: Said = Said { kind: Kind::Ended, nonce: 7 };

fn assigned(worker: usize, hosting: u32, run: u64, attempt: u64, admitted: bool) -> Seen {
    Seen::Assigned { worker, slots: 2, hosting, run, attempt, admitted }
}

/// A referee that has seen attempt 11 of run 1 started, and admitted.
fn admitted() -> Referee<Fleet> {
    let mut referee = referee();
    see(&mut referee, 0, Seen::Started { run: 1, attempt: 11 });
    see(&mut referee, 1, assigned(0, 0, 1, 11, true));
    referee
}

#[test]
fn a_run_answered_made_durable_and_forgotten_passes() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 3, Seen::Ended { run: 1, attempt: 11, end: End::Answered(SAID) });
    see(&mut referee, 4, Seen::Durable { run: 1, attempt: 11 });
    see(&mut referee, 5, Seen::Forgot { run: 1, attempt: 11 });
    referee.assert_passed(0);
}

#[test]
fn an_answer_heard_again_before_it_is_durable_passes() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 3, Seen::Ended { run: 1, attempt: 11, end: End::Answered(SAID) });
    // The engine restarted before it made the answer durable.
    see(&mut referee, 4, Seen::Ended { run: 1, attempt: 11, end: End::Answered(SAID) });
    see(&mut referee, 5, Seen::Durable { run: 1, attempt: 11 });
    referee.assert_passed(0);
}

#[test]
fn a_busy_refusal_and_its_assignment_again_pass() {
    let mut referee = referee();
    see(&mut referee, 0, Seen::Started { run: 1, attempt: 11 });
    see(&mut referee, 1, assigned(0, 1, 1, 11, false));
    see(&mut referee, 2, assigned(1, 0, 1, 11, true));
    see(&mut referee, 3, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 4, Seen::Ended { run: 1, attempt: 11, end: End::Answered(SAID) });
    see(&mut referee, 5, Seen::Durable { run: 1, attempt: 11 });
    referee.assert_passed(0);
}

#[test]
fn an_assignment_beyond_a_worker_s_slots_fails() {
    let mut referee = referee();
    see(&mut referee, 1, assigned(3, 2, 1, 11, false));
    assert!(why(&referee).contains("no more runs than its 2 slots"));
}

#[test]
fn two_live_attempts_of_a_run_fail() {
    let mut referee = admitted();
    see(&mut referee, 2, assigned(1, 0, 1, 12, true));
    assert!(why(&referee).contains("while attempt Some(11) is live"));
}

#[test]
fn an_attempt_assigned_again_once_admitted_fails() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 3, assigned(1, 0, 1, 11, true));
    assert!(why(&referee).contains("not assigned once admitted"));
}

#[test]
fn an_inbound_event_after_a_cancel_fails() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Cancelled { run: 1, attempt: 11 });
    see(&mut referee, 3, Seen::Sent { run: 1, attempt: 11, down: Down::Cancel });
    see(&mut referee, 4, Seen::Sent { run: 1, attempt: 11, down: Down::Inbound });
    assert!(why(&referee).contains("no Inbound reaches a worker"));
}

#[test]
fn an_end_other_than_the_worker_s_answer_fails() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Answered { run: 1, attempt: 11, said: SAID });
    let other = Said { kind: Kind::Failed, nonce: 8 };
    see(&mut referee, 3, Seen::Ended { run: 1, attempt: 11, end: End::Answered(other) });
    assert!(why(&referee).contains("ends with what its worker answered"));
}

#[test]
fn an_end_after_the_answer_is_durable_fails() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 3, Seen::Ended { run: 1, attempt: 11, end: End::Answered(SAID) });
    see(&mut referee, 4, Seen::Durable { run: 1, attempt: 11 });
    see(&mut referee, 5, Seen::Ended { run: 1, attempt: 11, end: End::Lost });
    assert!(why(&referee).contains("ends no more once ended"));
}

#[test]
fn a_worker_forgetting_an_answer_before_it_is_durable_fails() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 3, Seen::Forgot { run: 1, attempt: 11 });
    assert!(why(&referee).contains("only once it is durable"));
}

#[test]
fn a_worker_forgetting_the_answer_of_an_attempt_no_longer_claimed_passes() {
    let mut referee = admitted();
    see(&mut referee, 2, Seen::Ended { run: 1, attempt: 11, end: End::Replaced });
    see(&mut referee, 3, Seen::Answered { run: 1, attempt: 11, said: SAID });
    see(&mut referee, 4, Seen::Forgot { run: 1, attempt: 11 });
    referee.assert_passed(0);
}

#[test]
fn an_attempt_that_never_ends_fails_at_its_deadline() {
    let mut referee = admitted();
    assert!(!referee.is_due(at(59)));
    assert!(referee.is_due(at(60)));
    referee.fire(at(60), &mut Vec::new());
    assert!(why(&referee).contains("End { run: 1, attempt: 11 } was not met"));
}
