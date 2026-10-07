use jig_local_host::Budget;
use jig_local_host_world::{Observation, referee};
use skein_lib::Duration;
use smith_domain::run;

fn budget() -> Budget {
    Budget { turns: 4, spend: 1, time: Duration::from_secs(5) }
}

#[test]
fn the_referee_rejects_a_duplicate_turn_and_an_unanswered_call() {
    assert_eq!(
        referee::judge(
            &[Observation::Turn(1), Observation::Turn(1), Observation::TurnAcknowledged(1), Observation::Answer],
            None,
            budget(),
        ),
        Err("a turn was emitted twice")
    );
    assert_eq!(
        referee::judge(&[Observation::Call, Observation::Stopped], None, budget()),
        Err("a call was not answered")
    );
    let overspent = run::Answer::Parked {
        spent: run::Spend { turns: 1, input: 1, output: 1, cache_read: 0, cache_write: 0, units: 2 },
        turns: 1,
    };
    assert_eq!(
        referee::judge(&[Observation::Answer], Some(&overspent), budget()),
        Err("run spent more than its budget")
    );
}
