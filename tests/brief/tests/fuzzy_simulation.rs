//! Small deterministic inventories sweep sizes, priorities and budgets.

use jig_brief_world::{LIMITS, World, referee};
use jig_core_brief::{Core, GatherEvent, GatherRequest, Planned};
use skein_lib::{Duration, Time, Token};

#[test]
fn seeded_connector_sizes_and_priorities_never_exceed_the_budget_or_leak_a_token() {
    for seed in 0_u64..64 {
        let mut world = World::new(LIMITS);
        let size = usize::try_from(seed + 1).expect("small seed");
        let budget = u32::try_from(12 + seed % 37).expect("small budget");
        let token = Token::new(2);
        world.put(token, &vec![b'x'; size]);
        let asked = world.step(GatherEvent::Plan {
            brief: Token::new(1),
            budget,
            deadline: Time::ZERO.saturating_add(Duration::from_secs(10)),
            sections: Box::new([
                Planned::Connector {
                    connector: 3,
                    kind: 7,
                    token,
                    size: 0,
                    limit: 80,
                    priority: u16::try_from(seed % 3).expect("small priority"),
                    required: seed % 2 == 0,
                },
                Planned::Core {
                    kind: Core::Task,
                    text: b"do work".as_slice().into(),
                    limit: 32,
                    priority: 1,
                    required: true,
                },
            ]),
        });
        let done = world.settle(asked);
        assert!(referee::within_budget(&done, budget), "seed {seed}");
        assert!(
            matches!(done.as_slice(), [GatherRequest::Complete { .. } | GatherRequest::Failed { .. }]),
            "seed {seed}: {done:?}"
        );
        assert!(world.closed(token), "seed {seed}: token closed");
    }
}
