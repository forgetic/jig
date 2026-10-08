use jig_conformance::{Harness, referee::Promise, scenarios::Scenario};
use jig_conformance_world::{Config, Testing, actions, negative::Fault};
#[global_allocator]
static HEAP: skein_world::domain::heap::Counting = skein_world::domain::heap::Counting;

fn world(fault: Fault) -> Harness<Testing> {
    let scenario = (fault == Fault::StaleVerdict).then_some(Scenario::ChangedJudge);
    Harness::new(Config::new(31, false, 2, scenario).broken(fault), 31)
}

#[test]
fn every_broken_root_is_caught_from_durable_rows_and_outside_deliveries() {
    for (fault, promise) in [
        (Fault::EarlyOutput, Promise::Commit),
        (Fault::SplitDecision, Promise::Spend),
        (Fault::AdmissionAfterMutation, Promise::Spend),
        (Fault::WrongRestart, Promise::Order),
        (Fault::UncheckedEffect, Promise::Spend),
        (Fault::WrongKind, Promise::Authority),
    ] {
        let mut world = world(fault);
        let failure = if matches!(fault, Fault::EarlyOutput | Fault::WrongRestart) {
            world.drain().expect_err("the root corruption must be visible")
        } else {
            world.drain().expect("fault waits for an effect");
            actions::effect(&mut world, 1);
            world.drain().expect_err("the root corruption must be visible")
        };
        assert_eq!(failure.promise, promise, "{fault:?}: {failure:?}");
    }
}

#[test]
fn every_broken_connector_is_caught_from_fake_systems_and_durable_attempts() {
    for fault in [Fault::RepeatedEffect, Fault::StaleVerdict, Fault::ForgottenDeadline, Fault::DifferentDecision] {
        let mut world = world(fault);
        world.drain().expect("faithful startup");
        if fault == Fault::StaleVerdict {
            world.advance(61_000_000_000);
            world.drain().expect("the recorded judge fact ages past its freshness");
        }
        if matches!(fault, Fault::ForgottenDeadline | Fault::DifferentDecision) {
            world.peers.hold_writes = true;
        }
        actions::effect(&mut world, 1);
        let failure = if matches!(fault, Fault::ForgottenDeadline | Fault::DifferentDecision) {
            world.drain().expect("initial decision is faithful and uncertain");
            let (number, pending) = world.peers.pending_writes[0].clone();
            world.crash().expect("cold store retains the initial decision");
            world.drain().expect("reconstructed connector awaits the retained system response");
            let event = world.systems[usize::from(number - 1)].answer(pending, jig_test_system::Fault::Late);
            world.send(jig_test_domain::Event::Connector { number, event: jig_test_connector::Event::System(event) });
            world.drain().expect_err("a second step must preserve decision and deadline")
        } else {
            world.drain().expect_err("the connector corruption must be visible")
        };
        let promise = if fault == Fault::StaleVerdict { Promise::Authority } else { Promise::Once };
        assert_eq!(failure.promise, promise, "{fault:?}: {failure:?}");
    }
}

#[test]
fn the_same_boundaries_accept_a_faithful_root_and_connector_across_restart() {
    let mut world = world(Fault::None);
    world.drain().expect("faithful startup");
    world.peers.hold_writes = true;
    actions::effect(&mut world, 1);
    world.drain().expect("faithful initial decision");
    world.crash().expect("faithful cold store");
    world.drain().expect("deadline and decision survive");
    world.peers.hold_writes = false;
    world.advance(2_000_000_000);
    world.drain().expect("faithful retry");
    assert_eq!(world.systems[0].observed().iter().filter(|effect| effect.applied).count(), 1);
}
