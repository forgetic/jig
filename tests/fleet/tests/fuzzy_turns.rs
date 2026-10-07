use jig_fleet_world::turns::{self, Settings};

#[test]
fn turn_world_sweeps_replay_backpressure_and_fencing() {
    let mut busy = 0;
    for seed in 0..100 {
        let report = turns::run(Settings {
            drops: seed % 2 == 0,
            restart: seed % 3 != 0,
            cancel: seed % 5 == 0,
            capacity: 1 + u32::try_from(seed % 3).expect("capacity at most three"),
            ..Settings::new(seed)
        });
        busy += report.parent_busy;
        assert!(report.committed <= 8);
    }
    assert!(busy > 0);
}
