use jig_core_people::{RequestKey, Role};
use jig_people_world::referee::{People, Seen};
use skein_lib::Time;
use skein_world::domain::{Referee, Verdict};

fn fails(observations: &[Seen]) {
    let mut referee = Referee::new(People::default());
    for seen in observations {
        referee.observe(Time::ZERO, *seen, &mut Vec::new());
    }
    assert!(matches!(referee.verdict(), Verdict::Failed(_)));
}

#[test]
fn rejects_a_reply_before_its_commit() {
    fails(&[Seen::Called { call: 1 }, Seen::Replied { call: 1, after: 1 }]);
}

#[test]
fn rejects_a_repeated_reply() {
    fails(&[Seen::Called { call: 1 }, Seen::Replied { call: 1, after: 0 }, Seen::Replied { call: 1, after: 0 }]);
}

#[test]
fn rejects_an_observer_routed() {
    fails(&[Seen::Routed { role: Role::Observer }]);
}

#[test]
fn rejects_a_key_made_twice() {
    let key = RequestKey { person: 1, key: [1; 16] };
    fails(&[Seen::Created { key }, Seen::Created { key }]);
}

#[test]
fn a_missing_reply_fails_its_deadline() {
    let mut referee = Referee::new(People::default());
    referee.observe(Time::ZERO, Seen::Called { call: 1 }, &mut Vec::new());
    referee.fire(Time::from_nanos(11_000_000_000), &mut Vec::new());
    assert!(matches!(referee.verdict(), Verdict::Failed(_)));
}
