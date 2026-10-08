#![expect(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    clippy::arithmetic_side_effects,
    reason = "step tests are ordinary Rust (testing-strategy.md, section 4)"
)]
use crate::*;
use skein_lib::{Duration, Env, List, Queue, ReplyTo, Time, Token, Wall};

const LIMITS: Limits = Limits {
    people: 4,
    inbox_entries: 2,
    sign_ins: 4,
    projects: 3,
    holdings: 4,
    goals: 4,
    initial_owners: 3,
    requests: 4,
    pending: 2,
    waiters: 2,
    identity_bytes: 16,
    words: 8,
    amendment_bytes: 256,
    sign_in_lifetime: Duration::from_secs(60),
    request_retention: Duration::from_secs(120),
    facts: 8,
};

#[test]
fn result_cache_is_bounded_and_read_position_restores() {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let mut out = Queue::with_capacity(max_out(&LIMITS));
    let mut domain = Domain::new(&LIMITS, Box::new([]), 2);
    step(
        &mut domain,
        &env,
        Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } },
        &mut out,
    );
    step(&mut domain, &env, Event::Restored, &mut out);
    for (task, position) in [(3, 3), (1, 1), (2, 2)] {
        domain.remember_result(&LIMITS, 1, ResultRef { task, position });
    }
    assert_eq!(
        domain.cached_results(1),
        Some(&[ResultRef { task: 3, position: 3 }, ResultRef { task: 2, position: 2 }][..])
    );
    let row = domain.advance_read_position(1, 2).expect("monotonic read");
    assert_eq!(row, Stored::ReadPosition { person: 1, position: 2 });
    assert_eq!(domain.cached_results(1), Some(&[ResultRef { task: 3, position: 3 }][..]));
    assert_eq!(domain.advance_read_position(1, 1), None, "position never moves backward");
    let mut restored = Domain::new(&LIMITS, Box::new([]), 2);
    step(&mut restored, &env, Event::Restore { record: row }, &mut out);
    step(
        &mut restored,
        &env,
        Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } },
        &mut out,
    );
    step(&mut restored, &env, Event::Restored, &mut out);
    assert_eq!(restored.read_position(1), Some(2));
}

#[test]
fn waiting_cache_keeps_newest_references_and_removes_one_task() {
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let mut out = Queue::with_capacity(max_out(&LIMITS));
    let mut domain = Domain::new(&LIMITS, Box::new([]), 2);
    step(
        &mut domain,
        &env,
        Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } },
        &mut out,
    );
    step(&mut domain, &env, Event::Restored, &mut out);
    for task in 1..=3 {
        let entry = Entry { task, project: 1, whom: Whom::Person(1), kind: EntryKind::PersonTask, at: Wall::EPOCH };
        step(&mut domain, &env, Event::Waiting { task, entries: Box::new([entry]) }, &mut out);
    }
    let cached = domain.cached_waiting(&LIMITS, 1).expect("restored person");
    assert_eq!(cached.iter().map(|entry| entry.task).collect::<Vec<_>>(), [3, 2]);
    step(&mut domain, &env, Event::Waiting { task: 3, entries: Box::new([]) }, &mut out);
    let cached = domain.cached_waiting(&LIMITS, 1).expect("restored person");
    assert_eq!(cached.iter().map(|entry| entry.task).collect::<Vec<_>>(), [2]);
}

fn identity(provider: u16, subject: u64) -> Identity {
    Identity {
        key: IdentityKey { provider, subject: subject.to_be_bytes().into() },
        login: Box::new([b'l']),
        name: Box::new([b'n']),
    }
}

#[test]
fn an_owner_routes_a_connector_resource_adoption_once_by_its_key() {
    let mut test = Test::new(LIMITS);
    test.signin(1, 10, identity(0, 1));
    test.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Owner }]) });
    let ask = Ask::Adopt {
        project: 1,
        adoption: Adoption {
            resource: ResourceName { connector: 7, path: Box::new([b"resource".as_slice().into()]) },
            role: ResourceRole::Context,
            options: Box::new([b"read".as_slice().into()]),
        },
    };
    let request = route(&test.request(10, 1, ask.clone()));
    assert_eq!(
        reply(&test.send(Event::Decided { request, outcome: Outcome::Adopted { project: 1 } })),
        Reply::Outcome(Outcome::Adopted { project: 1 })
    );
    assert_eq!(reply(&test.request(10, 1, ask)), Reply::Outcome(Outcome::Adopted { project: 1 }));
}

#[test]
fn owner_makes_a_service_which_can_request_work_but_cannot_start_a_chat() {
    let mut test = Test::new(LIMITS);
    test.signin(1, 10, identity(0, 1));
    test.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Owner }]) });

    let ask = Ask::MakeService { project: 1, name: b"builder".as_slice().into(), role: Role::Member };
    let request = route(&test.request(10, 1, ask.clone()));
    let made = test.send(Event::MakeService { request, person: 2 });
    let mut person_saved = false;
    let mut roles_saved = false;
    let mut service_made = false;
    for row in &made {
        if let Request::Save { record: Stored::Person { number: 2, .. } } = row {
            person_saved = true;
        }
        if let Request::Save { record: Stored::Roles { project: 1, .. } } = row {
            roles_saved = true;
        }
        if let Request::ServiceMade { outcome: Outcome::ServiceMade { person: 2 }, .. } = row {
            service_made = true;
        }
    }
    assert!(person_saved && roles_saved && service_made);
    let decided = test.send(Event::Decided { request, outcome: Outcome::ServiceMade { person: 2 } });
    assert_eq!(reply(&decided), Reply::Outcome(Outcome::ServiceMade { person: 2 }));
    assert_eq!(reply(&test.request(10, 1, ask)), Reply::Outcome(Outcome::ServiceMade { person: 2 }));

    let unknown_to = test.to();
    let unknown = test.send(Event::SignedIn {
        reply_to: unknown_to,
        person: 3,
        sign_in: 20,
        identity: identity(2, 3),
        kind: Kind::Service,
    });
    assert_eq!(reply(&unknown), Reply::Refused(Refusal::SignIn));
    let service_to = test.to();
    let service = test.send(Event::SignedIn {
        reply_to: service_to,
        person: 4,
        sign_in: 21,
        identity: identity(2, 2),
        kind: Kind::Service,
    });
    assert_eq!(reply(&service), Reply::SignedIn { person: 2, expires: Wall::from_nanos(60_000_000_000) });
    assert_eq!(
        reply(&test.request(21, 2, Ask::StartChat { project: 1, words: Box::new([]) })),
        Reply::Outcome(Outcome::Refused(Refusal::Role))
    );
    let routed = test.request(
        21,
        3,
        Ask::SetGoal { project: 1, spec: b"work".as_slice().into(), charter: 1, budget: 1, priority: 1 },
    );
    let mut service_routed = false;
    for row in &routed {
        if let Request::Route { person, .. } = row {
            assert_eq!(*person, 2);
            service_routed = true;
        }
    }
    assert!(service_routed);
}

fn ask(project: u32) -> Ask {
    Ask::StartChat { project, words: Box::new([1]) }
}

struct Test {
    d: Domain,
    env: Env<Limits>,
    serial: u64,
}

impl Test {
    fn new(limits: Limits) -> Test {
        let mut test = Test {
            d: Domain::new(&limits, Box::new([]), 2),
            env: Env { limits, now: Time::ZERO, wall: Wall::EPOCH },
            serial: 0,
        };
        assert!(test.send(Event::Restored).is_empty());
        test
    }

    fn to(&mut self) -> ReplyTo {
        self.serial += 1;
        ReplyTo::new(Token::new(self.serial))
    }

    fn send(&mut self, event: Event) -> Vec<Request> {
        let mut out = Queue::with_capacity(max_out(&self.env.limits));
        step(&mut self.d, &self.env, event, &mut out);
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            requests.push(request);
        }
        self.d.reclaim();
        requests
    }

    fn send_bounded(&mut self, event: Event) -> List<Request> {
        let capacity = max_out(&self.env.limits);
        let mut out = Queue::with_capacity(capacity);
        step(&mut self.d, &self.env, event, &mut out);
        let mut requests = List::with_capacity(capacity);
        for _ in 0..capacity {
            let Some(request) = out.pop() else {
                break;
            };
            requests.push(request).expect("one bounded output snapshot");
        }
        assert!(out.is_empty());
        self.d.reclaim();
        requests
    }

    fn signin_bounded(&mut self, person: u64, sign_in: u64, identity: Identity) -> List<Request> {
        let reply_to = self.to();
        self.send_bounded(Event::SignedIn { reply_to, person, sign_in, identity, kind: Kind::Person })
    }

    fn request_bounded(&mut self, sign_in: u64, key: u8, ask: Ask) -> List<Request> {
        let reply_to = self.to();
        self.send_bounded(Event::Ask { reply_to, sign_in, key: [key; 16], ask })
    }

    fn signin(&mut self, person: u64, sign_in: u64, identity: Identity) -> Vec<Request> {
        let reply_to = self.to();
        self.send(Event::SignedIn { reply_to, person, sign_in, identity, kind: Kind::Person })
    }

    fn request(&mut self, sign_in: u64, key: u8, ask: Ask) -> Vec<Request> {
        let reply_to = self.to();
        self.send(Event::Ask { reply_to, sign_in, key: [key; 16], ask })
    }

    fn member(&mut self) {
        self.signin(1, 10, identity(0, 1));
        self.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Member }]) });
    }
}

fn reply(rows: &[Request]) -> Reply {
    rows.iter()
        .find_map(|r| match r {
            Request::Reply { reply, .. } => Some(*reply),
            Request::Save { .. }
            | Request::Erase { .. }
            | Request::Route { .. }
            | Request::RolesApplied { .. }
            | Request::RolesRefused { .. }
            | Request::ServiceMade { .. }
            | Request::RestoreRefused { .. } => None,
        })
        .expect("reply emitted")
}

fn route(rows: &[Request]) -> Token {
    rows.iter()
        .find_map(|r| match r {
            Request::Route { request, .. } => Some(*request),
            Request::Save { .. }
            | Request::Erase { .. }
            | Request::Reply { .. }
            | Request::RolesApplied { .. }
            | Request::RolesRefused { .. }
            | Request::ServiceMade { .. }
            | Request::RestoreRefused { .. } => None,
        })
        .expect("route emitted")
}

fn saved_answer(rows: &[Request]) -> Stored {
    rows.iter()
        .find_map(|row| match row {
            Request::Save { record } => match record {
                Stored::Answer { .. } => Some(record.clone()),
                Stored::Person { .. }
                | Stored::ReadPosition { .. }
                | Stored::SignIn { .. }
                | Stored::Roles { .. }
                | Stored::Policy { .. } => None,
            },
            Request::Reply { .. }
            | Request::Erase { .. }
            | Request::Route { .. }
            | Request::RolesApplied { .. }
            | Request::RolesRefused { .. }
            | Request::ServiceMade { .. }
            | Request::RestoreRefused { .. } => None,
        })
        .expect("keyed answer saved")
}

#[test]
fn provider_and_subject_identify_a_party_independent_of_display_fields() {
    let mut test = Test::new(LIMITS);
    assert_eq!(signed_person(reply(&test.signin(1, 10, identity(0, 1)))), Some(1));
    let mut renamed = identity(0, 1);
    renamed.login = b"new".as_slice().into();
    assert_eq!(signed_person(reply(&test.signin(2, 11, renamed))), Some(1));
    assert_eq!(signed_person(reply(&test.signin(3, 12, identity(1, 1)))), Some(3));
    assert_eq!(reply(&test.signin(4, 10, identity(0, 4))), Reply::Refused(Refusal::SignIn));
    let directory = Identity {
        key: IdentityKey { provider: 1, subject: b"alice".as_slice().into() },
        login: b"a".as_slice().into(),
        name: b"A".as_slice().into(),
    };
    assert_eq!(signed_person(reply(&test.signin(4, 13, directory))), Some(4));
}

#[test]
fn all_roles_are_checked_and_refusals_are_saved() {
    for role in [Role::Owner, Role::Maintainer, Role::Member, Role::Observer, Role::Policy { role: 4 }] {
        let mut test = Test::new(LIMITS);
        test.member();
        test.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role }]) });
        let rows = test.request(10, 1, ask(1));
        if role == Role::Observer {
            assert_eq!(reply(&rows), Reply::Outcome(Outcome::Refused(Refusal::Role)));
            assert!(rows.iter().any(is_answer));
        } else {
            route(&rows);
        }
    }
    let mut test = Test::new(LIMITS);
    test.member();
    assert_eq!(reply(&test.request(10, 1, ask(2))), Reply::Outcome(Outcome::Refused(Refusal::Role)));
}

#[test]
fn policy_role_receives_shared_waiting_and_replays_its_key_after_restart() {
    let mut live = Test::new(LIMITS);
    live.signin(1, 10, identity(0, 1));
    live.signin(2, 20, identity(0, 2));
    let holdings = Box::new([
        Holding { person: 1, role: Role::Policy { role: 4 } },
        Holding { person: 2, role: Role::Policy { role: 4 } },
    ]);
    live.send(Event::Roles { project: 1, holdings: holdings.clone() });
    let entry = Entry {
        task: 7,
        project: 1,
        whom: Whom::Role { project: 1, role: 4 },
        kind: EntryKind::PersonTask,
        at: Wall::EPOCH,
    };
    live.send(Event::Waiting { task: 7, entries: Box::new([entry]) });
    assert_eq!(live.d.cached_waiting(&LIMITS, 1).as_deref(), Some(&[entry][..]));
    assert_eq!(live.d.cached_waiting(&LIMITS, 2).as_deref(), Some(&[entry][..]));
    let ask = Ask::TakePerson { project: 1, task: 7 };
    let request = route(&live.request(10, 1, ask.clone()));
    let answered = live.send(Event::Decided { request, outcome: Outcome::PersonTaken { task: 7 } });
    let saved = saved_answer(&answered);
    live.send(Event::Waiting { task: 7, entries: Box::new([]) });
    assert_eq!(live.d.cached_waiting(&LIMITS, 1).as_deref(), Some(&[][..]));
    assert_eq!(live.d.cached_waiting(&LIMITS, 2).as_deref(), Some(&[][..]));

    let mut cold = Test::new(LIMITS);
    cold.d = Domain::new(&LIMITS, Box::new([]), 2);
    cold.send(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } });
    cold.send(Event::Restore { record: Stored::Person { number: 2, identity: identity(0, 2) } });
    cold.send(Event::Restore { record: Stored::Roles { project: 1, holdings } });
    cold.send(Event::Restore {
        record: Stored::SignIn { number: 10, person: 1, expires: Wall::from_nanos(Duration::from_secs(60).as_nanos()) },
    });
    cold.send(Event::Restore { record: saved });
    cold.send(Event::Restored);
    assert_eq!(reply(&cold.request(10, 1, ask)), Reply::Outcome(Outcome::PersonTaken { task: 7 }));
}

#[test]
fn a_watch_key_is_volatile_and_replays_only_while_open() {
    let mut test = Test::new(LIMITS);
    test.member();
    let ask = Ask::Watch { project: 1, subject: WatchSubject::Goals };
    let request = route(&test.request(10, 13, ask.clone()));
    let opened = test.send(Event::Decided { request, outcome: Outcome::Watching { watcher: Token::new(7) } });
    assert_eq!(reply(&opened), Reply::Outcome(Outcome::Watching { watcher: Token::new(7) }));
    assert!(!opened.iter().any(is_answer), "a watch has no durable answer row");
    assert_eq!(reply(&test.request(10, 13, ask.clone())), Reply::Outcome(Outcome::Watching { watcher: Token::new(7) }));
    assert!(test.send(Event::WatchClosed { watcher: Token::new(7) }).is_empty());
    route(&test.request(10, 13, ask));
}

#[test]
fn a_note_edit_checks_membership_then_retains_the_refusal() {
    let ask = Ask::EditNote {
        project: 1,
        name: 9,
        scope: Box::new(NoteScope::Project),
        change: Box::new(NoteChange::Delete { recalled: 1 }),
    };
    let mut member = Test::new(LIMITS);
    member.member();
    let request = route(&member.request(10, 14, ask.clone()));
    let refused = member.send(Event::Decided { request, outcome: Outcome::Refused(Refusal::NotOffered) });
    assert!(refused.iter().any(is_answer));
    assert_eq!(reply(&member.request(10, 14, ask.clone())), Reply::Outcome(Outcome::Refused(Refusal::NotOffered)));

    let mut observer = Test::new(LIMITS);
    observer.member();
    observer.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Observer }]) });
    assert_eq!(reply(&observer.request(10, 14, ask)), Reply::Outcome(Outcome::Refused(Refusal::Role)));
}

#[test]
fn only_held_policy_roles_consume_inbox_cache_room() {
    let mut test = Test::new(Limits { holdings: 1, ..LIMITS });
    test.member();
    test.send(Event::Roles {
        project: 1,
        holdings: Box::new([Holding { person: 1, role: Role::Policy { role: 99 } }]),
    });
    let absent = Entry {
        task: 7,
        project: 1,
        whom: Whom::Role { project: 1, role: 100 },
        kind: EntryKind::PersonTask,
        at: Wall::EPOCH,
    };
    test.send(Event::Waiting { task: 7, entries: Box::new([absent]) });
    let held = Entry { task: 8, whom: Whom::Role { project: 1, role: 99 }, ..absent };
    test.send(Event::Waiting { task: 8, entries: Box::new([held]) });
    assert_eq!(test.d.cached_waiting(&test.env.limits, 1).as_deref(), Some(&[held][..]));
    test.send(Event::Roles { project: 1, holdings: Box::new([]) });
    assert_eq!(test.d.cached_waiting(&test.env.limits, 1).as_deref(), Some(&[][..]));
}

#[test]
fn pending_duplicates_share_one_route_and_reserve_answer_room() {
    let mut test = Test::new(Limits { requests: 1, ..LIMITS });
    test.member();
    let routed = test.request(10, 1, ask(1));
    let token = route(&routed);
    assert!(test.request(10, 1, ask(1)).is_empty());
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Refused(Refusal::Busy));
    assert_eq!(reply(&test.request(10, 2, ask(1))), Reply::Refused(Refusal::Busy));
    assert_eq!(reply(&test.request(10, 1, ask(2))), Reply::Refused(Refusal::KeyConflict));
    let completed = test.send(Event::Decided { request: token, outcome: Outcome::Started { task: 90 } });
    assert_eq!(completed.iter().filter(|r| is_reply(r)).count(), 2);
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Outcome(Outcome::Started { task: 90 }));
}

#[test]
fn answered_keys_follow_people_across_signins_and_role_changes() {
    let mut test = Test::new(LIMITS);
    test.member();
    let rows = test.request(10, 1, ask(1));
    let token = route(&rows);
    test.send(Event::Decided { request: token, outcome: Outcome::Started { task: 90 } });
    let reply_to = test.to();
    test.send(Event::SignOut { reply_to, sign_in: 10 });
    test.signin(2, 11, identity(0, 1));
    test.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Observer }]) });
    assert_eq!(reply(&test.request(11, 1, ask(1))), Reply::Outcome(Outcome::Started { task: 90 }));
    assert_eq!(reply(&test.request(11, 1, ask(2))), Reply::Refused(Refusal::KeyConflict));
}

#[test]
fn completed_key_replays_until_its_deadline_then_can_be_reused() {
    let mut test = Test::new(LIMITS);
    test.member();
    let token = route(&test.request(10, 1, ask(1)));
    test.send(Event::Decided { request: token, outcome: Outcome::Started { task: 90 } });
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Outcome(Outcome::Started { task: 90 }));
    test.env.now = Time::from_nanos(Duration::from_secs(120).as_nanos());
    test.env.wall = Wall::from_nanos(Duration::from_secs(120).as_nanos());
    // The old sign-in has expired too; a new sign-in still belongs to the same person.
    test.signin(2, 11, identity(0, 1));
    let fresh = test.request(11, 1, ask(1));
    assert!(fresh.contains(&Request::Erase { key: Key::Answer(RequestKey { person: 1, key: [1; 16] }) }));
    let token = route(&fresh);
    assert_ne!(token, Token::new(0));
}

#[test]
fn expired_completed_key_is_erased_from_a_restore_page_and_frees_its_slot() {
    let limits = Limits { requests: 1, ..LIMITS };
    let mut test = Test::new(limits);
    test.d = Domain::new(&limits, Box::new([]), 2);
    test.env.now = Time::from_nanos(Duration::from_secs(120).as_nanos());
    test.env.wall = Wall::from_nanos(Duration::from_secs(120).as_nanos());
    let key = RequestKey { person: 1, key: [1; 16] };
    let erased = test.send(Event::Restore {
        record: Stored::Answer { key, ask: Box::new(ask(1)), outcome: Outcome::Started { task: 90 }, at: Wall::EPOCH },
    });
    assert_eq!(erased, [Request::Erase { key: Key::Answer(key) }]);
    test.send(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } });
    test.send(Event::Restore {
        record: Stored::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Member }]) },
    });
    test.send(Event::Restore {
        record: Stored::SignIn {
            number: 10,
            person: 1,
            expires: Wall::from_nanos(Duration::from_secs(180).as_nanos()),
        },
    });
    test.send(Event::Restored);
    assert!(test.d.ready());
    let _routed = route(&test.request(10, 1, ask(1)));
}

#[test]
fn every_admission_point_refuses_without_partial_state() {
    let mut test = Test::new(Limits { people: 1, sign_ins: 1, projects: 1, holdings: 1, words: 1, ..LIMITS });
    test.member();
    assert_eq!(reply(&test.signin(2, 11, identity(0, 2))), Reply::Refused(Refusal::Busy));
    let reply_to = test.to();
    test.send(Event::SignOut { reply_to, sign_in: 10 });
    assert_eq!(reply(&test.signin(2, 11, identity(0, 2))), Reply::Refused(Refusal::Busy));
    assert_eq!(signed_person(reply(&test.signin(3, 11, identity(0, 1)))), Some(1));
    let big = Ask::StartChat { project: 1, words: Box::new([1, 2]) };
    assert_eq!(reply(&test.request(11, 1, big)), Reply::Refused(Refusal::Limit));
    let roles = test.send(Event::Roles { project: 2, holdings: Box::new([]) });
    assert_eq!(roles, [Request::RolesRefused { project: 2, refusal: Refusal::Busy }]);
    let roles = test.send(Event::Roles {
        project: 1,
        holdings: Box::new([Holding { person: 1, role: Role::Owner }, Holding { person: 2, role: Role::Member }]),
    });
    assert_eq!(roles, [Request::RolesRefused { project: 1, refusal: Refusal::Limit }]);
}

#[test]
fn expiry_is_monotonic_even_when_wall_clock_moves_back() {
    let mut test = Test::new(LIMITS);
    test.member();
    test.env.now = Time::from_nanos(Duration::from_secs(60).as_nanos());
    test.env.wall = Wall::EPOCH;
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Refused(Refusal::SignIn));
    let mut out = Queue::with_capacity(max_out(&LIMITS));
    fire(&mut test.d, &test.env, &mut out);
    assert_eq!(out.pop(), Some(Request::Erase { key: Key::SignIn(10) }));
}

#[test]
fn result_query_checks_monotonic_expiry_before_fire_after_backward_wall_jump() {
    let mut test = Test::new(LIMITS);
    test.env.wall = Wall::from_nanos(Duration::from_secs(100).as_nanos());
    test.member();
    assert_eq!(test.d.person(10, test.env.now, test.env.wall), Some(1));
    test.env.now = Time::from_nanos(Duration::from_secs(60).as_nanos());
    test.env.wall = Wall::EPOCH;
    assert_eq!(test.d.person(10, test.env.now, test.env.wall), None);
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Refused(Refusal::SignIn));
}

#[test]
fn restore_order_is_independent_and_bad_or_oversized_records_refuse_start() {
    let mut test = Test::new(LIMITS);
    test.d = Domain::new(&LIMITS, Box::new([]), 2);
    test.send(Event::Restore {
        record: Stored::SignIn { number: 10, person: 1, expires: Wall::from_nanos(Duration::from_secs(60).as_nanos()) },
    });
    test.send(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } });
    test.send(Event::Restored);
    assert!(test.d.ready());
    test.d = Domain::new(&LIMITS, Box::new([]), 2);
    test.send(Event::Restore { record: Stored::SignIn { number: 10, person: 99, expires: Wall::EPOCH } });
    assert_eq!(
        test.send(Event::Restored),
        [Request::RestoreRefused { key: Key::SignIn(10), refusal: Refusal::Unknown }]
    );
    assert!(!test.d.ready());
    test.env.limits.people = 0;
    test.d = Domain::new(&test.env.limits, Box::new([]), 2);
    assert_eq!(
        test.send(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } }),
        [Request::RestoreRefused { key: Key::Person(1), refusal: Refusal::Limit }]
    );
}

#[test]
fn role_success_restores_before_identities_and_replays_after_roster_changes() {
    let mut live = Test::new(LIMITS);
    live.signin_bounded(1, 10, identity(0, 1));
    live.signin_bounded(2, 20, identity(0, 2));
    live.send_bounded(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Owner }]) });
    let ask = Ask::SetRoles { project: 1, holdings: Box::new([Holding { person: 2, role: Role::Member }]) };
    let request = route(live.request_bounded(10, 7, ask.clone()).as_slice());
    let reply_token = live.to().into_token();
    assert_eq!(
        live.send_bounded(Event::ApplyRoles { reply_to: ReplyTo::new(reply_token), request }).as_slice(),
        [
            Request::Save {
                record: Stored::Roles { project: 1, holdings: Box::new([Holding { person: 2, role: Role::Member }]) }
            },
            Request::RolesApplied { reply_to: ReplyTo::new(reply_token), request, result: Ok(()) },
        ]
    );
    let outcome = Outcome::RolesSet { project: 1 };
    let completed = live.send_bounded(Event::Decided { request, outcome });
    assert_eq!(reply(completed.as_slice()), Reply::Outcome(outcome));
    let mut cold = Test::new(LIMITS);
    cold.d = Domain::new(&LIMITS, Box::new([]), 2);
    assert!(cold.send_bounded(Event::Restore { record: saved_answer(completed.as_slice()) }).is_empty());
    // The historical roster differs from today's; the original owner is now an observer.
    assert!(
        cold.send_bounded(Event::Restore {
            record: Stored::Roles { project: 1, holdings: Box::new([Holding { person: 1, role: Role::Observer }]) }
        })
        .is_empty()
    );
    for person in [2, 1] {
        assert!(
            cold.send_bounded(Event::Restore {
                record: Stored::Person { number: person, identity: identity(0, person) }
            })
            .is_empty()
        );
    }
    assert!(cold.send_bounded(Event::Restored).is_empty());
    assert!(cold.d.ready());
    cold.signin_bounded(3, 30, identity(0, 1));
    let replayed = cold.request_bounded(30, 7, ask.clone());
    assert_eq!(replayed.len(), 1, "replay emits no route or role write");
    assert_eq!(reply(replayed.as_slice()), Reply::Outcome(outcome));
    assert_eq!(cold.d.role(1, 1), Some(Role::Observer));
    assert_eq!(cold.d.role(2, 1), None);
    assert_eq!(reply(cold.request_bounded(30, 8, ask).as_slice()), Reply::Outcome(Outcome::Refused(Refusal::Role)));
}

#[test]
fn restored_role_success_requires_matching_ask_kind_and_project() {
    let key = RequestKey { person: 1, key: [7; 16] };
    for (ask, outcome) in [
        (Ask::SetRoles { project: 1, holdings: Box::new([]) }, Outcome::RolesSet { project: 2 }),
        (ask(1), Outcome::RolesSet { project: 1 }),
        (
            Ask::DecideEscalation { project: 1, task: 9, revision: 2, decision: EscalationDecision::Pass },
            Outcome::RolesSet { project: 1 },
        ),
        (Ask::SetRoles { project: 1, holdings: Box::new([]) }, Outcome::Started { task: 9 }),
        (
            Ask::SetRoles { project: 1, holdings: Box::new([]) },
            Outcome::EscalationDecided { task: 9, revision: 2, by: 1, choice: EscalationChoice::Passed },
        ),
    ] {
        let mut test = Test::new(LIMITS);
        test.d = Domain::new(&LIMITS, Box::new([]), 2);
        assert_eq!(
            test.send_bounded(Event::Restore {
                record: Stored::Answer { key, ask: Box::new(ask), outcome, at: Wall::EPOCH }
            })
            .as_slice(),
            [Request::RestoreRefused { key: Key::Answer(key), refusal: Refusal::Limit }]
        );
        assert!(test.send_bounded(Event::Restored).is_empty());
        assert!(!test.d.ready());
    }
}

#[test]
fn restored_successful_role_rosters_require_positive_unique_people() {
    let key = RequestKey { person: 1, key: [7; 16] };
    for holdings in [
        Box::from([Holding { person: 0, role: Role::Member }]),
        Box::from([Holding { person: 2, role: Role::Owner }, Holding { person: 2, role: Role::Member }]),
    ] {
        let mut test = Test::new(LIMITS);
        test.d = Domain::new(&LIMITS, Box::new([]), 2);
        assert_eq!(
            test.send_bounded(Event::Restore {
                record: Stored::Answer {
                    key,
                    ask: Box::new(Ask::SetRoles { project: 1, holdings }),
                    outcome: Outcome::RolesSet { project: 1 },
                    at: Wall::EPOCH,
                }
            })
            .as_slice(),
            [Request::RestoreRefused { key: Key::Answer(key), refusal: Refusal::Limit }]
        );
        assert!(test.send_bounded(Event::Restored).is_empty());
        assert!(!test.d.ready());
    }
}

#[test]
fn restored_role_success_checks_historical_people_and_project_after_all_rows_arrive() {
    let key = RequestKey { person: 1, key: [7; 16] };
    for (project, person) in [(1, 99), (2, 1)] {
        let mut test = Test::new(LIMITS);
        test.d = Domain::new(&LIMITS, Box::new([]), 2);
        assert!(
            test.send_bounded(Event::Restore {
                record: Stored::Answer {
                    key,
                    ask: Box::new(Ask::SetRoles {
                        project,
                        holdings: Box::new([Holding { person, role: Role::Member }])
                    }),
                    outcome: Outcome::RolesSet { project },
                    at: Wall::EPOCH,
                }
            })
            .is_empty()
        );
        test.send_bounded(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } });
        test.send_bounded(Event::Restore { record: Stored::Roles { project: 1, holdings: Box::new([]) } });
        assert_eq!(
            test.send_bounded(Event::Restored).as_slice(),
            [Request::RestoreRefused { key: Key::Answer(key), refusal: Refusal::Unknown }]
        );
        assert!(!test.d.ready());
    }
}

#[test]
fn refused_role_requests_restore_invalid_rosters_and_unknown_targets_for_replay() {
    for (role, project, holdings, refusal) in [
        (
            Role::Observer,
            1,
            Box::from([Holding { person: 0, role: Role::Owner }, Holding { person: 0, role: Role::Member }]),
            Refusal::Role,
        ),
        (Role::Owner, 2, Box::from([Holding { person: 99, role: Role::Member }]), Refusal::Unknown),
        (Role::Owner, 1, Box::from([Holding { person: 0, role: Role::Member }]), Refusal::Limit),
        (
            Role::Owner,
            1,
            Box::from([Holding { person: 1, role: Role::Owner }, Holding { person: 1, role: Role::Member }]),
            Refusal::Limit,
        ),
        (Role::Owner, 1, Box::from([Holding { person: 99, role: Role::Member }]), Refusal::Unknown),
    ] {
        let mut live = Test::new(LIMITS);
        live.signin_bounded(1, 10, identity(0, 1));
        live.send_bounded(Event::Roles { project: 1, holdings: Box::new([Holding { person: 1, role }]) });
        let ask = Ask::SetRoles { project, holdings };
        let mut rows = live.request_bounded(10, 7, ask.clone());
        if role == Role::Owner && project == 1 {
            let request = route(rows.as_slice());
            let reply_token = live.to().into_token();
            assert_eq!(
                live.send_bounded(Event::ApplyRoles { reply_to: ReplyTo::new(reply_token), request }).as_slice(),
                [Request::RolesApplied { reply_to: ReplyTo::new(reply_token), request, result: Err(refusal) }]
            );
            rows = live.send_bounded(Event::Decided { request, outcome: Outcome::Refused(refusal) });
        }
        assert_eq!(reply(rows.as_slice()), Reply::Outcome(Outcome::Refused(refusal)));
        let mut cold = Test::new(LIMITS);
        cold.d = Domain::new(&LIMITS, Box::new([]), 2);
        assert!(cold.send_bounded(Event::Restore { record: saved_answer(rows.as_slice()) }).is_empty());
        cold.send_bounded(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } });
        // Even a refused unknown project needs no present membership row.
        assert!(cold.send_bounded(Event::Restored).is_empty());
        assert!(cold.d.ready());
        cold.signin_bounded(2, 20, identity(0, 1));
        let replayed = cold.request_bounded(20, 7, ask);
        assert_eq!(replayed.len(), 1);
        assert_eq!(reply(replayed.as_slice()), Reply::Outcome(Outcome::Refused(refusal)));
    }
}

#[test]
fn initial_owners_bootstrap_all_projects_atomically_and_never_regrant() {
    let owners = Box::new([
        InitialOwner { project: 1, identity: IdentityKey { provider: 0, subject: 1_u64.to_be_bytes().into() } },
        InitialOwner { project: 2, identity: IdentityKey { provider: 0, subject: 1_u64.to_be_bytes().into() } },
    ]);
    let mut test = Test::new(Limits { holdings: 1, ..LIMITS });
    test.d = Domain::new(&test.env.limits, owners, 2);
    test.send(Event::Roles { project: 1, holdings: Box::new([]) });
    test.send(Event::Roles { project: 2, holdings: Box::new([Holding { person: 9, role: Role::Member }]) });
    test.send(Event::Restore { record: Stored::Person { number: 9, identity: identity(0, 9) } });
    test.send(Event::Restored);
    let refused = test.signin(1, 10, identity(0, 1));
    assert_eq!(reply(&refused), Reply::Refused(Refusal::Busy));
    assert_eq!(refused.len(), 1, "bootstrap refusal makes no record");
    test.send(Event::Roles { project: 2, holdings: Box::new([]) });
    let made = test.signin(2, 10, identity(0, 1));
    assert_eq!(made.iter().filter(|r| is_roles(r)).count(), 2);
    test.send(Event::Roles { project: 1, holdings: Box::new([Holding { person: 2, role: Role::Observer }]) });
    let again = test.signin(3, 11, identity(0, 1));
    assert!(!again.iter().any(is_roles));
    assert_eq!(reply(&test.request(11, 1, ask(1))), Reply::Outcome(Outcome::Refused(Refusal::Role)));
}

fn signed_person(reply: Reply) -> Option<u64> {
    match reply {
        Reply::SignedIn { person, .. } => Some(person),
        Reply::SignedOut | Reply::Outcome(_) | Reply::Refused(_) => None,
    }
}

fn is_reply(request: &Request) -> bool {
    match request {
        Request::Reply { .. } => true,
        Request::Save { .. }
        | Request::Erase { .. }
        | Request::Route { .. }
        | Request::RolesApplied { .. }
        | Request::RolesRefused { .. }
        | Request::ServiceMade { .. }
        | Request::RestoreRefused { .. } => false,
    }
}

fn is_answer(request: &Request) -> bool {
    match request {
        Request::Save { record } => match record {
            Stored::Answer { .. } => true,
            Stored::Person { .. }
            | Stored::ReadPosition { .. }
            | Stored::SignIn { .. }
            | Stored::Roles { .. }
            | Stored::Policy { .. } => false,
        },
        Request::Reply { .. }
        | Request::Erase { .. }
        | Request::Route { .. }
        | Request::RolesApplied { .. }
        | Request::RolesRefused { .. }
        | Request::ServiceMade { .. }
        | Request::RestoreRefused { .. } => false,
    }
}

fn is_roles(request: &Request) -> bool {
    match request {
        Request::Save { record } => match record {
            Stored::Roles { .. } => true,
            Stored::Person { .. }
            | Stored::ReadPosition { .. }
            | Stored::SignIn { .. }
            | Stored::Answer { .. }
            | Stored::Policy { .. } => false,
        },
        Request::Reply { .. }
        | Request::Erase { .. }
        | Request::Route { .. }
        | Request::RolesApplied { .. }
        | Request::RolesRefused { .. }
        | Request::ServiceMade { .. }
        | Request::RestoreRefused { .. } => false,
    }
}

#[test]
fn equal_keys_of_different_people_are_independent() {
    let mut test = Test::new(LIMITS);
    test.member();
    test.signin(2, 20, identity(0, 2));
    test.send(Event::Roles {
        project: 1,
        holdings: Box::new([Holding { person: 1, role: Role::Member }, Holding { person: 2, role: Role::Member }]),
    });
    let first = route(&test.request(10, 1, ask(1)));
    let second = route(&test.request(20, 1, ask(1)));
    assert_ne!(first, second);
    test.send(Event::Decided { request: first, outcome: Outcome::Started { task: 90 } });
    test.send(Event::Decided { request: second, outcome: Outcome::Started { task: 91 } });
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Outcome(Outcome::Started { task: 90 }));
    assert_eq!(reply(&test.request(20, 1, ask(1))), Reply::Outcome(Outcome::Started { task: 91 }));
}

#[test]
fn pending_capacity_refuses_before_routing_and_the_key_can_retry() {
    let mut test = Test::new(Limits { pending: 1, ..LIMITS });
    test.member();
    let first = route(&test.request(10, 1, ask(1)));
    assert_eq!(reply(&test.request(10, 2, ask(1))), Reply::Refused(Refusal::Busy));
    test.send(Event::Decided { request: first, outcome: Outcome::Refused(Refusal::Authority) });
    route(&test.request(10, 2, ask(1)));
}

#[test]
fn root_pressure_answers_every_waiter_and_releases_the_key_for_retry() {
    for refusal in [Refusal::Busy, Refusal::NotReady] {
        let mut test = Test::new(Limits { requests: 1, ..LIMITS });
        test.member();
        let first = route(&test.request(10, 1, ask(1)));
        assert!(test.request(10, 1, ask(1)).is_empty());
        let rows = test.send(Event::Decided { request: first, outcome: Outcome::Refused(refusal) });
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(is_reply), "retryable pressure saves no completed answer");
        assert_eq!(reply(&rows), Reply::Outcome(Outcome::Refused(refusal)));
        let retry = route(&test.request(10, 1, ask(1)));
        assert_ne!(first, retry, "retired route tokens fence earlier completions");
        test.send(Event::Decided { request: retry, outcome: Outcome::Started { task: 90 } });
        assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Outcome(Outcome::Started { task: 90 }));
    }
}

#[test]
fn identity_limits_clock_overflow_and_duplicate_holdings_leave_no_records() {
    let mut test = Test::new(Limits { identity_bytes: 1, ..LIMITS });
    assert_eq!(test.signin(1, 10, identity(0, 1)).len(), 1);
    test.env.limits = LIMITS;
    test.d = Domain::new(&LIMITS, Box::new([]), 2);
    test.send(Event::Restored);
    test.env.wall = Wall::from_nanos(u64::MAX);
    assert_eq!(reply(&test.signin(1, 10, identity(0, 1))), Reply::Refused(Refusal::Limit));
    test.env.wall = Wall::EPOCH;
    test.member();
    let rows = test.send(Event::Roles {
        project: 1,
        holdings: Box::new([Holding { person: 1, role: Role::Owner }, Holding { person: 1, role: Role::Member }]),
    });
    assert_eq!(rows, [Request::RolesRefused { project: 1, refusal: Refusal::Limit }]);
    route(&test.request(10, 1, ask(1)));
}

#[test]
fn calls_before_restore_and_after_failed_restore_are_answered_not_ready() {
    let mut test = Test::new(LIMITS);
    test.d = Domain::new(&LIMITS, Box::new([]), 2);
    assert_eq!(reply(&test.signin(1, 10, identity(0, 1))), Reply::Refused(Refusal::NotReady));
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Refused(Refusal::NotReady));
    test.send(Event::Restore {
        record: Stored::Answer {
            key: RequestKey { person: 99, key: [1; 16] },
            ask: Box::new(ask(1)),
            outcome: Outcome::Started { task: 90 },
            at: Wall::EPOCH,
        },
    });
    test.send(Event::Restored);
    assert!(!test.d.ready());
    assert_eq!(reply(&test.signin(1, 10, identity(0, 1))), Reply::Refused(Refusal::NotReady));
}

#[test]
fn expiry_projection_uses_the_environment_when_restore_finishes() {
    let mut test = Test::new(LIMITS);
    test.d = Domain::new(&LIMITS, Box::new([]), 2);
    test.send(Event::Restore { record: Stored::Person { number: 1, identity: identity(0, 1) } });
    test.send(Event::Restore {
        record: Stored::SignIn { number: 10, person: 1, expires: Wall::from_nanos(Duration::from_secs(60).as_nanos()) },
    });
    test.env.now = Time::from_nanos(Duration::from_secs(30).as_nanos());
    test.env.wall = Wall::from_nanos(Duration::from_secs(50).as_nanos());
    test.send(Event::Restored);
    test.env.now = Time::from_nanos(Duration::from_secs(45).as_nanos());
    test.env.wall = Wall::from_nanos(Duration::from_secs(55).as_nanos());
    assert_eq!(reply(&test.request(10, 1, ask(1))), Reply::Refused(Refusal::SignIn));
}

#[test]
fn authenticated_escalation_decisions_route_without_membership_and_io_pressure_is_retryable() {
    let mut test = Test::new(LIMITS);
    test.signin(1, 10, identity(0, 1));
    let ask = Ask::DecideEscalation { project: 1, task: 9, revision: 2, decision: EscalationDecision::Pass };
    let routed = test.request(10, 2, ask.clone());
    assert!(routed.iter().any(|row| match row {
        Request::Route { person, role, ask: routed, .. } => *person == 1 && role.is_none() && **routed == ask,
        Request::Reply { .. }
        | Request::Save { .. }
        | Request::Erase { .. }
        | Request::RolesApplied { .. }
        | Request::RolesRefused { .. }
        | Request::ServiceMade { .. }
        | Request::RestoreRefused { .. } => false,
    }));
    let request = route(&routed);
    assert_eq!(
        reply(&test.send(Event::Decided { request, outcome: Outcome::Refused(Refusal::Busy) })),
        Reply::Outcome(Outcome::Refused(Refusal::Busy))
    );
    let retried = test.request(10, 2, ask.clone());
    let request = route(&retried);
    let outcome = Outcome::EscalationDecided { task: 9, revision: 2, by: 7, choice: EscalationChoice::Passed };
    test.send(Event::Decided { request, outcome });
    assert_eq!(reply(&test.request(10, 2, ask)), Reply::Outcome(outcome));
    assert_eq!(
        reply(&test.request(
            10,
            2,
            Ask::DecideEscalation { project: 1, task: 9, revision: 2, decision: EscalationDecision::Release }
        )),
        Reply::Refused(Refusal::KeyConflict)
    );
    test.env.now = Time::from_nanos(Duration::from_secs(61).as_nanos());
    assert_eq!(
        reply(&test.request(
            10,
            3,
            Ask::DecideEscalation { project: 1, task: 9, revision: 2, decision: EscalationDecision::Pass }
        )),
        Reply::Refused(Refusal::SignIn)
    );
}
