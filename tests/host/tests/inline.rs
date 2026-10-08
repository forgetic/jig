//! The hub on an engine slot, with a real inline Smith domain and fake LLM.

use jig_charter::{Budget, Prices};
use jig_host::Ending;
use jig_host_world::inline_world::{Script, Seen, World};
use skein_lib::{Duration, Token};

fn budget() -> Budget {
    Budget { turns: 8, spend: 1, time: Duration::from_secs(60) }
}

fn free() -> Prices {
    Prices { input: 0, cached: 0, output: 0, unit: 1 }
}

#[test]
fn inline_run_turn_is_held_by_the_hub_until_the_core_commits_it() {
    let mut world = World::new(Script::Answer, false);
    world.assign(budget(), free(), Box::new([]));
    world.until(Seen::Turn(1));
    assert!(world.retained(1), "unacknowledged turn stays in the hub");
    world.acknowledge(1);
    world.until(Seen::Answer);
    assert!(!world.retained(1), "exact ACK frees the turn");
    assert!(matches!(world.answer().map(|answer| &answer.ending), Some(Ending::Ended { .. })));
    assert!(world.seen().contains(&Seen::Gone), "the agent was gone before the hub answered");
}

#[test]
fn inline_run_parks_then_a_fresh_hub_resumes_its_committed_transcript() {
    let mut first = World::new(Script::Wait, true);
    first.assign(budget(), free(), Box::new([]));
    first.until(Seen::Waiting);
    first.until(Seen::Answer);
    assert!(matches!(first.answer().map(|answer| &answer.ending), Some(Ending::Parked { .. })));
    let transcript = first.transcript();
    assert!(!transcript.is_empty(), "the parked run left a concrete turn");

    let mut resumed = World::new(Script::Wait, true);
    resumed.assign(budget(), free(), transcript);
    resumed.until(Seen::Admitted);
    resumed.message(Token::new(7), b"hello");
    resumed.until(Seen::Answer);
    assert!(matches!(resumed.answer().map(|answer| &answer.ending), Some(Ending::Ended { .. })));
}

#[test]
fn cancelling_an_inline_run_mid_completion_winds_down_once() {
    let mut world = World::new(Script::Answer, true);
    world.assign(budget(), free(), Box::new([]));
    world.until(Seen::Completion);
    world.cancel();
    world.until(Seen::Answer);
    assert!(matches!(world.answer().map(|answer| &answer.ending), Some(Ending::Failed { .. })));
    assert!(world.seen().contains(&Seen::Gone), "the hub waited for the inline agent to go");
    assert_eq!(world.seen().iter().filter(|seen| **seen == Seen::Answer).count(), 1);
}

#[test]
fn completion_that_cannot_fit_the_run_budget_is_never_sent() {
    let mut world = World::new(Script::Answer, true);
    world.assign(budget(), Prices { input: 1, cached: 1, output: 1, unit: 1 }, Box::new([]));
    world.until(Seen::Answer);
    assert!(matches!(world.answer().map(|answer| &answer.ending), Some(Ending::Failed { .. })));
    assert!(!world.seen().contains(&Seen::Completion), "unaffordable completion never reached the provider");
}
