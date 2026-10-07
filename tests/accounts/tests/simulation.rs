use jig_accounts_world::World;
use jig_core_accounts::{Event, Fact, Failure, Grant, Request, State};
use skein_lib::Duration;

fn fresh(world: &mut World) {
    assert_eq!(
        world.step(Event::Add { account: 7, generation: 4, valid: Some(Duration::from_secs(100)) }),
        vec![
            Request::Granted { grant: Grant { account: 7, generation: 4, valid: Duration::from_secs(100) } },
            Request::Availability { account: 7, usable: true }
        ]
    );
}

#[test]
fn refresh_due_early_rejection_throttle_and_stale_generation() {
    let mut world = World::default();
    fresh(&mut world);
    assert!(world.step(Event::Rejected { account: 7, generation: 4 }).is_empty());
    world.fire_at(3);
    assert!(world.step(Event::Rejected { account: 7, generation: 3 }).is_empty());
    assert_eq!(
        world.step(Event::Rejected { account: 7, generation: 4 }),
        vec![Request::Refresh { account: 7, generation: 5 }]
    );
    assert_eq!(
        world.step(Event::Refreshed { account: 7, generation: 5, valid: Duration::from_secs(100) }),
        vec![Request::Granted { grant: Grant { account: 7, generation: 5, valid: Duration::from_secs(100) } }]
    );
    assert_eq!(world.fire_at(93), vec![Request::Refresh { account: 7, generation: 6 }]);
}

#[test]
fn unsaved_rotation_retries_only_the_write_and_keeps_old_grant_until_expiry() {
    let mut world = World::default();
    fresh(&mut world);
    assert_eq!(world.fire_at(90), vec![Request::Refresh { account: 7, generation: 5 }]);
    assert!(
        world
            .step(Event::Failed {
                account: 7,
                generation: 5,
                failure: Failure::Unsaved { valid: Duration::from_secs(100) }
            })
            .is_empty()
    );
    assert_eq!(world.domain.grant(7, world.now).expect("old token still usable").generation, 4);
    assert_eq!(world.fire_at(92), vec![Request::Keep { account: 7, generation: 5 }]);
    assert!(world.step(Event::Failed { account: 7, generation: 5, failure: Failure::Unavailable }).is_empty());
    assert_eq!(world.fire_at(96), vec![Request::Keep { account: 7, generation: 5 }]);
    assert_eq!(world.fire_at(100), vec![Request::Availability { account: 7, usable: false }]);
    assert_eq!(
        world.step(Event::Refreshed { account: 7, generation: 5, valid: Duration::from_secs(90) }),
        vec![
            Request::Granted { grant: Grant { account: 7, generation: 5, valid: Duration::from_secs(90) } },
            Request::Availability { account: 7, usable: true }
        ]
    );
}

#[test]
fn starting_backoff_rate_limit_revocation_and_shutdown_settle() {
    let mut world = World::default();
    assert_eq!(
        world.step(Event::Add { account: 7, generation: 4, valid: None }),
        vec![Request::Refresh { account: 7, generation: 5 }]
    );
    world.step(Event::Failed {
        account: 7,
        generation: 5,
        failure: Failure::RateLimited { retry_after: Duration::from_secs(20) },
    });
    assert!(world.fire_at(19).is_empty());
    assert_eq!(world.fire_at(20), vec![Request::Refresh { account: 7, generation: 5 }]);
    world.step(Event::Failed { account: 7, generation: 5, failure: Failure::Refused });
    assert!(!world.domain.usable(7));
    assert!(world.domain.next_deadline().is_none());
    assert_eq!(world.step(Event::Close { account: 7 }), vec![Request::Closed { account: 7 }]);
    assert_eq!(world.domain.accounts(), 0);
    world.step(Event::Add { account: 7, generation: 9, valid: None });
    assert_eq!(world.step(Event::Close { account: 7 }), vec![Request::Cancel { account: 7, generation: 10 }]);
    assert_eq!(
        world.step(Event::Failed { account: 7, generation: 10, failure: Failure::Cancelled }),
        vec![Request::Closed { account: 7 }]
    );
    assert_eq!(world.domain.accounts(), 0);
}

#[test]
fn spent_overlay_waits_and_recovers_without_cancelling_live_work() {
    let mut world = World::default();
    fresh(&mut world);
    assert_eq!(
        world.step(Event::Exhausted { account: 7, retry_after: Duration::from_secs(30) }),
        vec![Request::Availability { account: 7, usable: false }]
    );
    assert!(world.domain.grant(7, world.now).is_none());
    assert_eq!(world.fire_at(30), vec![Request::Availability { account: 7, usable: true }]);
    assert_eq!(world.domain.grant(7, world.now).expect("cooldown ended").generation, 4);
}

#[test]
fn tiny_capacity_refuses_only_the_entrance_and_duplicate_names() {
    let mut world = World::default();
    fresh(&mut world);
    assert_eq!(
        world.step(Event::Add { account: 7, generation: 0, valid: None }),
        vec![Request::Refused { account: 7 }]
    );
    world.step(Event::Add { account: 8, generation: 0, valid: None });
    assert_eq!(
        world.step(Event::Add { account: 9, generation: 0, valid: None }),
        vec![Request::Refused { account: 9 }]
    );
    assert!(world.domain.usable(7), "admission refusal leaves existing accounts usable");
    assert_eq!(world.domain.accounts(), 2);
}

fn last_fact(world: &mut World) -> Fact {
    let mut last = None;
    while let Some(fact) = world.domain.pop_fact() {
        last = Some(fact);
    }
    last.expect("the transition kept a fact")
}

#[test]
fn spent_attention_survives_refresh_and_resolves_when_the_overlay_ends() {
    let mut world = World::default();
    fresh(&mut world);
    world.step(Event::Exhausted { account: 7, retry_after: Duration::from_secs(40) });
    assert!(last_fact(&mut world).attention);
    world.fire_at(3);
    world.step(Event::Rejected { account: 7, generation: 4 });
    world.step(Event::Refreshed { account: 7, generation: 5, valid: Duration::from_secs(100) });
    let refreshed = last_fact(&mut world);
    assert!(refreshed.attention, "saving fresh credentials does not end a spent overlay");
    assert_eq!(refreshed.spent_until, Some(skein_lib::Time::from_nanos(40_000_000_000)));
    world.fire_at(40);
    let recovered = last_fact(&mut world);
    assert!(!recovered.attention, "the long cooldown has ended");
    assert_eq!(recovered.spent_until, None);
}

#[test]
fn exhausted_startup_generations_are_refused_before_any_refresh_or_grant() {
    for valid in [None, Some(Duration::from_secs(100))] {
        let mut world = World::default();
        assert_eq!(
            world.step(Event::Add { account: 7, generation: u64::MAX, valid }),
            vec![Request::Refused { account: 7 }]
        );
        assert_eq!(world.domain.accounts(), 0);
        assert!(world.step(Event::Rejected { account: 7, generation: u64::MAX }).is_empty());
        assert!(world.fire_at(100).is_empty());
    }
}

#[test]
fn the_last_rotation_revokes_when_rejection_or_expiry_needs_a_new_generation() {
    for rejected in [false, true] {
        let mut world = World::default();
        assert_eq!(
            world.step(Event::Add { account: 7, generation: u64::MAX - 1, valid: None }),
            vec![Request::Refresh { account: 7, generation: u64::MAX }]
        );
        world.step(Event::Refreshed { account: 7, generation: u64::MAX, valid: Duration::from_secs(100) });
        assert_eq!(world.domain.grant(7, world.now).expect("the saved last generation is usable").generation, u64::MAX);
        let requests = if rejected {
            world.fire_at(3);
            world.step(Event::Rejected { account: 7, generation: u64::MAX })
        } else {
            world.fire_at(90)
        };
        assert_eq!(requests, vec![Request::Availability { account: 7, usable: false }]);
        let fact = last_fact(&mut world);
        assert_eq!(fact.state, State::Revoked);
        assert!(fact.attention);
        assert!(world.domain.grant(7, world.now).is_none());
        assert!(world.step(Event::Rejected { account: 7, generation: u64::MAX }).is_empty());
        assert!(world.fire_at(100).is_empty());
    }
}
