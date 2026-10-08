use jig_brief_world::{LIMITS, World, referee};
use jig_core_brief::{Core, GatherEvent, GatherMissing, GatherPlaced, GatherRequest, Planned};
use skein_lib::{Duration, Time, Token};

fn connector(token: u64, required: bool, priority: u16) -> Planned {
    Planned::Connector { connector: 3, kind: 7, token: Token::new(token), size: 0, limit: 128, priority, required }
}

fn begin(world: &mut World, sections: Box<[Planned]>, budget: u32) -> Vec<GatherRequest> {
    world.step(GatherEvent::Plan {
        brief: Token::new(1),
        budget,
        deadline: Time::ZERO.saturating_add(Duration::from_secs(10)),
        sections,
    })
}

#[test]
fn a_connector_section_stays_with_its_owner_until_the_root_takes_it() {
    let mut world = World::new(LIMITS);
    let token = Token::new(2);
    world.put(token, b"connector words");
    let requests = begin(
        &mut world,
        Box::new([
            Planned::Core { kind: Core::Task, text: b"task".as_slice().into(), limit: 40, priority: 0, required: true },
            connector(2, true, 1),
        ]),
        40,
    );
    assert!(world.held(token), "brief has only the token");
    assert!(matches!(requests.as_slice(), [GatherRequest::Gather { connector: 3, section, .. }] if *section == token));
    let done = world.settle(requests);
    assert!(referee::within_budget(&done, 40));
    assert!(matches!(done.as_slice(), [GatherRequest::Complete { order, .. }]
        if matches!(order.as_ref(), [GatherPlaced::Core { kind: Core::Task, .. }, GatherPlaced::Connector { token: kept, .. }] if *kept == token)));
    assert!(world.closed(token));
    assert!(!world.held(token));
}

#[test]
fn required_sections_claim_room_before_optional_sections_and_cuts_go_to_the_owner() {
    let mut world = World::new(LIMITS);
    let token = Token::new(2);
    world.put(token, &[b'x'; 90]);
    let requests = begin(
        &mut world,
        Box::new([
            connector(2, false, 8),
            Planned::Core {
                kind: Core::Task,
                text: b"required task words".as_slice().into(),
                limit: 80,
                priority: 0,
                required: true,
            },
        ]),
        45,
    );
    let done = world.settle(requests);
    assert!(referee::within_budget(&done, 45));
    assert!(matches!(done.as_slice(), [GatherRequest::Complete { order, .. }]
        if matches!(order.as_ref(), [GatherPlaced::Connector { size, .. }, GatherPlaced::Core { kind: Core::Task, .. }] if *size < 90)));
    assert!(world.closed(token));
}

#[test]
fn a_required_section_missing_fails_the_brief_and_drops_its_token() {
    let mut world = World::new(LIMITS);
    let asked = begin(&mut world, Box::new([connector(2, true, 0)]), 40);
    let done = world.settle(asked);
    assert_eq!(done, [GatherRequest::Failed { brief: Token::new(1), why: GatherMissing::Failed }]);
    assert!(world.closed(Token::new(2)));
}

#[test]
fn a_late_optional_section_is_marked_missing_and_dropped() {
    let mut world = World::new(LIMITS);
    let token = Token::new(2);
    world.put(token, b"late words");
    world.pause(token);
    let asked = begin(&mut world, Box::new([connector(2, false, 0)]), 40);
    assert!(matches!(asked.as_slice(), [GatherRequest::Gather { .. }]));
    let expired = world.fire(Time::ZERO.saturating_add(Duration::from_secs(10)));
    let done = world.settle(expired);
    assert!(matches!(done.as_slice(), [GatherRequest::Complete { order, .. }]
        if matches!(order.as_ref(), [GatherPlaced::Missing { why: GatherMissing::Late, .. }])));
    assert!(world.closed(token));
}

#[test]
fn an_amendment_while_gathering_drops_every_section_gathered() {
    let mut world = World::new(LIMITS);
    for number in [2, 3] {
        let token = Token::new(number);
        world.put(token, b"held");
        world.pause(token);
    }
    let asked = begin(&mut world, Box::new([connector(2, false, 0), connector(3, false, 1)]), 40);
    assert_eq!(asked.len(), 2);
    let abandoned = world.step(GatherEvent::Abandon { brief: Token::new(1) });
    let done = world.settle(abandoned);
    assert!(done.is_empty());
    assert!(world.closed(Token::new(2)) && world.closed(Token::new(3)));
}

#[test]
fn sections_that_do_not_fit_are_cut_in_their_kinds_order_and_say_how_much() {
    let mut world = World::new(LIMITS);
    let attempts = b"early attempt\nrecent attempt\n";
    let asked = begin(
        &mut world,
        Box::new([
            Planned::Core {
                kind: Core::TranscriptTail,
                text: b"last transcript turn".as_slice().into(),
                limit: 30,
                priority: 2,
                required: false,
            },
            Planned::Core {
                kind: Core::Task,
                text: b"do work\n".as_slice().into(),
                limit: 40,
                priority: 0,
                required: true,
            },
            Planned::Core {
                kind: Core::Attempts,
                text: attempts.as_slice().into(),
                limit: 25,
                priority: 1,
                required: false,
            },
        ]),
        33,
    );
    assert!(referee::within_budget(&asked, 33));
    let [GatherRequest::Complete { order, .. }] = asked.as_slice() else { panic!("completed brief: {asked:?}") };
    let [
        GatherPlaced::CoreMissing { kind: Core::TranscriptTail, why: GatherMissing::Budget },
        GatherPlaced::Core { kind: Core::Task, text: task },
        GatherPlaced::Core { kind: Core::Attempts, text: cut },
    ] = order.as_ref()
    else {
        panic!("source order retained: {order:?}")
    };
    assert_eq!(task.as_ref(), b"do work\n");
    assert!(cut.len() <= 25);
    assert!(cut.ends_with(b"attempt\n"), "newest attempt bytes retained: {cut:?}");
    let mut counted = false;
    for lost in 1..attempts.len() {
        let marker = format!("[{lost} bytes cut]\n");
        if cut.starts_with(marker.as_bytes()) {
            assert_eq!(&cut[marker.len()..], &attempts[lost..]);
            counted = true;
        }
    }
    assert!(counted, "cut names exactly the omitted bytes: {cut:?}");
}

#[test]
fn a_required_section_missing_at_the_deadline_fails_before_completion() {
    let mut world = World::new(LIMITS);
    let token = Token::new(2);
    world.put(token, b"required words");
    world.pause(token);
    let asked = begin(&mut world, Box::new([connector(2, true, 0)]), 40);
    assert!(matches!(asked.as_slice(), [GatherRequest::Gather { .. }]));
    let expired = world.fire(Time::ZERO.saturating_add(Duration::from_secs(10)));
    let done = world.settle(expired);
    assert_eq!(done, [GatherRequest::Failed { brief: Token::new(1), why: GatherMissing::Late }]);
    assert!(world.closed(token));
}
