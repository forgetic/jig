//! The composed hub, inline Smith agent and fake provider fit their bounds.

use jig_charter::{Budget, Prices};
use jig_host_world::inline_world::{Script, Seen, World};
use skein_lib::Duration;
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

#[test]
fn inline_and_hub_peak_memory_stays_below_their_composed_worst_case() {
    let meter = Meter::new();
    let mut world = World::new(Script::Answer, true);
    let bound = world.memory_bound();
    meter.start();
    world.assign(
        Budget { turns: 8, spend: 1, time: Duration::from_secs(60) },
        Prices { input: 0, cached: 0, output: 0, unit: 1 },
        Box::new([]),
    );
    world.until(Seen::Answer);
    let measured = meter.end();
    meter.check(measured, bound, &"inline hub and fake provider");
}
