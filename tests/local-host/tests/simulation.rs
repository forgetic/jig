use jig_local_host::{Budget, Prices};
use jig_local_host_world::{Observation, Script, World, referee};
use skein_lib::Duration;
use smith_domain::run;

fn budget() -> Budget {
    Budget { turns: 8, spend: 1, time: Duration::from_secs(60) }
}

#[test]
fn a_run_answers_and_its_turns_are_kept_until_acknowledged() {
    let mut world = World::new(Script::Answer, false);
    world.assign(budget(), false);
    world.until(Observation::Turn(1));
    assert!(world.retained(1));
    world.acknowledge(1);
    world.cycle();
    world.until(Observation::Answer);
    assert!(!world.retained(1));
    assert!(matches!(world.answer(), Some(run::Answer::Accepted { .. })));
    referee::judge(world.observations(), world.answer(), budget()).expect("one committed turn and answer");
}

#[test]
fn a_run_waits_for_a_message_then_parks_past_its_threshold_and_resumes_from_its_transcript() {
    let mut world = World::new(Script::Wait, true);
    world.assign(budget(), false);
    world.until(Observation::Waiting);
    world.until(Observation::Answer);
    assert!(matches!(world.answer(), Some(run::Answer::Parked { .. })));
    referee::judge(world.observations(), world.answer(), budget()).expect("parked history was committed");
    let transcript = world.transcript();
    let mut resumed = World::new(Script::Wait, true);
    resumed.assign_history(budget(), false, Prices { input: 0, cached: 0, output: 0, unit: 1 }, Some(transcript));
    resumed.until(Observation::Admitted);
    resumed.message(7, b"hello");
    resumed.until(Observation::Answer);
    assert!(matches!(resumed.answer(), Some(run::Answer::Accepted { .. })));
    referee::judge(resumed.observations(), resumed.answer(), budget()).expect("resumed turn and answer settled");
}

#[test]
fn a_run_cancelled_mid_turn_winds_down_and_answers() {
    let mut world = World::new(Script::Answer, true);
    world.assign(budget(), false);
    world.until(Observation::Completion);
    world.cancel();
    world.until(Observation::Answer);
    assert!(matches!(world.answer(), Some(run::Answer::Failed { failure: run::Failure::Cancelled, .. })));
    referee::judge(world.observations(), world.answer(), budget()).expect("cancelled run settled once");
}

#[test]
fn a_run_whose_next_completion_does_not_fit_its_budget_ends_for_its_budget() {
    let mut world = World::new(Script::Answer, true);
    world.assign_priced(budget(), false, Prices { input: 1, cached: 1, output: 1, unit: 1 });
    world.until(Observation::Answer);
    assert!(matches!(world.answer(), Some(run::Answer::Failed { failure: run::Failure::Budget(_), .. })));
    referee::judge(world.observations(), world.answer(), budget()).expect("budget ended the run once");
}

#[test]
fn a_declared_host_call_is_answered_once_before_the_run_finishes() {
    let mut world = World::new(Script::Call, true);
    world.assign(budget(), true);
    world.until(Observation::Answer);
    assert!(matches!(world.answer(), Some(run::Answer::Accepted { .. })));
    referee::judge(world.observations(), world.answer(), budget()).expect("host call settled once");
}
