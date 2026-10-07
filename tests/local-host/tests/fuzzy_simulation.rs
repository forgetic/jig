//! Seeded provider failures, messages during completions, cancellations and
//! delayed core acknowledgement (domain/hosts.md, section 11).

use jig_local_host::Budget;
use jig_local_host_world::{Observation, Script, World, referee};
use skein_lib::{Duration, Rng};

fn budget() -> Budget {
    Budget { turns: 8, spend: 1, time: Duration::from_secs(60) }
}

#[test]
fn seeded_provider_and_core_races_settle_with_one_answer_and_committed_turns() {
    let mut failed = 0;
    let mut messaged = 0;
    let mut cancelled = 0;
    let mut cancel_cuts = [0_u32; 3];
    let mut delayed = 0;
    for seed in 0_u64..64 {
        let mut rng = Rng::new(seed ^ 0x65);
        let case = seed % 4;
        let mut world = match case {
            0 => World::seeded(Script::Answer, true, seed, 1000),
            1 => World::seeded(Script::Wait, true, seed, 0),
            2 => World::seeded(Script::Call, true, seed, 0),
            3 => World::seeded(Script::Answer, false, seed, 0),
            _ => unreachable!("four seeded scenarios"),
        };
        world.assign(budget(), case == 2);
        match case {
            0 => {
                world.until(Observation::Answer);
                assert!(world.observations().contains(&Observation::ProviderFailed), "seed {seed}");
                failed += 1;
            }
            1 => {
                world.until(Observation::Completion);
                world.message(seed + 1, b"arrived during the turn");
                world.until(Observation::Answer);
                messaged += 1;
            }
            2 => {
                let cut = rng.between(0, 2);
                cancel_cuts[usize::try_from(cut).expect("three cuts")] += 1;
                match cut {
                    0 => {}
                    1 => world.until(Observation::Completion),
                    2 => world.until(Observation::Call),
                    _ => unreachable!("three cancellation cuts"),
                }
                world.cancel();
                world.until(Observation::Answer);
                cancelled += 1;
            }
            3 => {
                world.until(Observation::Turn(1));
                let later = world.now().saturating_add(Duration::from_millis(rng.between(1, 5)));
                world.tick(later);
                world.acknowledge(1);
                world.cycle();
                world.until(Observation::Answer);
                delayed += 1;
            }
            _ => unreachable!("four seeded scenarios"),
        }
        referee::judge(world.observations(), world.answer(), budget())
            .unwrap_or_else(|reason| panic!("seed {seed}: {reason}"));
    }
    assert_eq!((failed, messaged, cancelled, delayed), (16, 16, 16, 16));
    assert!(cancel_cuts.iter().all(|count| *count > 0), "each cancellation cut was exercised: {cancel_cuts:?}");
}
