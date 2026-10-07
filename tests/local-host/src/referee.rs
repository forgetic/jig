//! Boundary-only checks for core commitments and host terminals
//! (domain/hosts.md, section 11; domain/testing.md, section 4).

use std::collections::BTreeMap;

use jig_local_host::Budget;
use smith_domain::run;

use crate::Observation;

/// Check that each emitted turn, answer and call has one matching settlement.
///
/// # Errors
///
/// Returns a short description of the first unsettled or duplicated observation.
pub fn judge(seen: &[Observation], answer: Option<&run::Answer>, budget: Budget) -> Result<(), &'static str> {
    let mut turns: BTreeMap<u32, u32> = BTreeMap::new();
    let mut calls = 0;
    let mut answered_calls = 0;
    let mut answers = 0;
    let mut stopped = 0;
    for observation in seen {
        match observation {
            Observation::Turn(number) => {
                if turns.insert(*number, 0).is_some() {
                    return Err("a turn was emitted twice");
                }
            }
            Observation::TurnAcknowledged(number) => {
                let Some(count) = turns.get_mut(number) else { return Err("an unknown turn was acknowledged") };
                *count += 1;
                if *count > 1 {
                    return Err("a turn was acknowledged twice");
                }
            }
            Observation::Call => calls += 1,
            Observation::CallAnswered => {
                answered_calls += 1;
                if answered_calls > calls {
                    return Err("a call was answered without a request");
                }
            }
            Observation::Answer => answers += 1,
            Observation::Stopped => stopped += 1,
            Observation::Admitted
            | Observation::Completion
            | Observation::ProviderFailed
            | Observation::CompletionCancelled
            | Observation::Waiting => {}
        }
    }
    if turns.values().any(|count| *count != 1) {
        return Err("an emitted turn was not acknowledged");
    }
    if calls != answered_calls {
        return Err("a call was not answered");
    }
    if answers + stopped != 1 {
        return Err("the run has no unique terminal");
    }
    let spend = match answer {
        Some(
            run::Answer::Accepted { spent, .. } | run::Answer::Parked { spent, .. } | run::Answer::Failed { spent, .. },
        ) => spent.units,
        Some(run::Answer::Refused(_)) => 0,
        None if stopped == 1 => 0,
        None => return Err("the answer was not retained"),
    };
    if spend > budget.spend {
        return Err("run spent more than its budget");
    }
    Ok(())
}
