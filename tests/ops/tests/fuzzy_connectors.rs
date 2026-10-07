use jig_ops_domain_infrastructure as infra;
use jig_ops_domain_observability as obs;
use jig_ops_fake_production as production;
use jig_ops_world::{InfrastructureWorld, World, infra_environment, infra_service, service};
use skein_lib::Token;

fn observability(seed: u64) -> (Vec<obs::Request>, Vec<production::ObservedEffect>, u64) {
    let mut world = World::new(0);
    world.production.preload_history(u32::try_from(seed * 10).expect("seed fits"));
    world.event(obs::Event::Subscribe { task: 8, topic: obs::Topic::Alerts(service()), wake_at: 6, keep_at: 2 });
    world.event(obs::Event::Describe {
        token: Token::new(1),
        effect: obs::Effect { rule: Box::from(*b"checkout-errors"), until: 100 },
    });
    let key = obs::Key { deployment: 1, task: seed, purpose: 3 };
    let kept = world.event(obs::Event::Keep { token: Token::new(1), key });
    match seed % 4 {
        0 => {
            world.restart();
        }
        1 => {
            world.production.queue_fault(production::Fault::LostAnswer);
            world.release(&kept);
        }
        2 => {
            world.event(obs::Event::Make { key });
            world.restart();
        }
        3 => {
            world.production.queue_fault(production::Fault::ApiError);
            world.release(&kept);
        }
        _ => unreachable!(),
    }
    world.now = 30;
    world.fire();
    world.restart();
    assert!(
        world
            .production
            .observed()
            .iter()
            .filter(|row| matches!(row, production::ObservedEffect::Silence { applied: true, .. }))
            .count()
            <= 1
    );
    (world.seen, world.production.observed().to_vec(), world.production.calls())
}

fn infrastructure(seed: u64) -> (Vec<infra::Request>, Vec<production::ObservedEffect>, u64) {
    let backend = if seed.is_multiple_of(2) { infra::Backend::OperationIds } else { infra::Backend::NoOperationIds };
    let mut world = InfrastructureWorld::new(0, backend);
    world.production.preload_history(u32::try_from(seed * 10).expect("seed fits"));
    let effect = match seed % 4 {
        0 => infra::Effect::Restart { service: infra_service(), operation: seed },
        1 => infra::Effect::Scale { service: infra_service(), from: 3, to: 4 },
        2 => infra::Effect::Rollback { service: infra_service(), from: Box::from(*b"v2"), to: Box::from(*b"v1") },
        3 => infra::Effect::CreateEnvironment { environment: infra_environment(), until: 100, price: 20 },
        _ => unreachable!(),
    };
    world.event(infra::Event::Describe { token: Token::new(1), effect });
    let key = infra::Key { deployment: 1, task: seed, purpose: 3 };
    let kept = world.event(infra::Event::Keep { token: Token::new(1), key });
    match seed % 5 {
        0 => {
            world.restart();
        }
        1 => {
            world.production.queue_fault(production::Fault::LostAnswer);
            world.release(&kept);
        }
        2 => {
            world.event(infra::Event::Make { key });
            world.restart();
        }
        3 => {
            world.production.queue_fault(production::Fault::ApiError);
            world.release(&kept);
        }
        4 => {
            world.release(&kept);
        }
        _ => unreachable!(),
    }
    world.now = 30;
    world.fire();
    world.restart();
    let applied = world
        .production
        .observed()
        .iter()
        .filter(|row| match row {
            production::ObservedEffect::Restart { applied, .. }
            | production::ObservedEffect::Scale { applied, .. }
            | production::ObservedEffect::Rollback { applied, .. }
            | production::ObservedEffect::Create { applied, .. } => *applied,
            production::ObservedEffect::Silence { .. } | production::ObservedEffect::TearDown { .. } => false,
        })
        .count();
    assert!(applied <= 1, "seed {seed}");
    (world.seen, world.production.observed().to_vec(), world.production.calls())
}

#[test]
fn observability_replays_drawn_faults_and_restart_cuts() {
    for seed in 1..=64 {
        assert_eq!(observability(seed), observability(seed), "seed {seed}");
    }
}

#[test]
fn infrastructure_replays_drawn_faults_and_restart_cuts() {
    for seed in 1..=64 {
        assert_eq!(infrastructure(seed), infrastructure(seed), "seed {seed}");
    }
}
