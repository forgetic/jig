use jig_accounts_world::World;
use jig_core_accounts::{Event, Failure, Request};
use skein_lib::{Duration, Rng};

fn respond(world: &mut World, rng: &mut Rng, requests: Vec<Request>, trace: &mut Vec<Request>) {
    for request in &requests {
        let terminal = match request {
            Request::Refresh { account, generation } | Request::Keep { account, generation } => {
                Some(if rng.chance(300) {
                    Event::Failed { account: *account, generation: *generation, failure: Failure::Unavailable }
                } else {
                    Event::Refreshed { account: *account, generation: *generation, valid: Duration::from_secs(40) }
                })
            }
            Request::Cancel { account, generation } => {
                Some(Event::Failed { account: *account, generation: *generation, failure: Failure::Cancelled })
            }
            Request::Granted { .. }
            | Request::Availability { .. }
            | Request::Refused { .. }
            | Request::Closed { .. } => None,
        };
        if let Some(event) = terminal {
            trace.extend(world.step(event));
        }
    }
    trace.extend(requests);
}

fn run(seed: u64) -> Vec<Request> {
    let mut world = World::default();
    let mut rng = Rng::new(seed);
    let mut trace = world.step(Event::Add { account: 7, generation: 4, valid: Some(Duration::from_secs(40)) });
    for seconds in 1..160 {
        if rng.chance(40) {
            let generation = world.domain.grant(7, world.now).map_or(4, |grant| grant.generation);
            let requests = world.step(Event::Rejected { account: 7, generation });
            respond(&mut world, &mut rng, requests, &mut trace);
        }
        if rng.chance(30) {
            trace.extend(world.step(Event::Exhausted { account: 7, retry_after: Duration::from_secs(5) }));
        }
        let due = world.fire_at(seconds);
        respond(&mut world, &mut rng, due, &mut trace);
        assert!(world.domain.accounts() <= 2);
    }
    let closing = world.step(Event::Close { account: 7 });
    respond(&mut world, &mut rng, closing, &mut trace);
    assert_eq!(world.domain.accounts(), 0, "every configured account closed");
    assert!(world.domain.next_deadline().is_none(), "no timers after shutdown");
    trace
}

#[test]
fn seeded_account_timers_and_faults_replay() {
    for seed in 0..64 {
        assert_eq!(run(seed), run(seed), "seed {seed}");
    }
}
