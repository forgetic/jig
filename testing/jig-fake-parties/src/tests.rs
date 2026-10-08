use super::*;

#[test]
fn duplicate_requests_keep_their_key_payload_and_session() {
    let mut party = Party::new(
        1,
        Role::Member,
        100,
        Box::new([
            Act::SignIn { provider: 0, subject: Box::from([7]) },
            Act::Request { key: [5; 16], ask: Ask::Chat { words: Box::from(&b"hello"[..]) } },
            Act::Twice,
        ]),
    );
    assert!(matches!(party.tick(Time::ZERO), Some(Up::SignIn { to: 100, .. })));
    assert!(party.tick(Time::ZERO).is_none());
    party.reply(100, Reply::SignedIn { person: 1, sign_in: 2 });
    let first = party.tick(Time::ZERO).expect("first request");
    party.reply(101, Reply::Started { task: 3 });
    let duplicate = party.tick(Time::ZERO).expect("duplicate");
    match (first, duplicate) {
        (
            Up::Request { sign_in, project, key, ask, .. },
            Up::Request { sign_in: second, project: second_project, key: second_key, ask: second_ask, .. },
        ) => {
            assert_eq!((sign_in, project, key, ask), (second, second_project, second_key, second_ask));
        }
        other => panic!("two keyed requests: {other:?}"),
    }
    party.reply(102, Reply::Started { task: 3 });
    assert!(party.quiescent());
}

#[test]
fn a_choice_waits_until_a_person_task_is_visible() {
    let mut party = Party::new(
        1,
        Role::Maintainer,
        100,
        Box::new([Act::AnswerWaiting { code: 2, words: Box::from(&b"second"[..]) }]),
    );
    party.sign_in = Some(1);
    assert!(party.tick(Time::ZERO).is_none());
    party.waiting(Some(Waiting::Choice { task: 7 }));
    assert!(matches!(
        party.tick(Time::ZERO),
        Some(Up::Request { ask: Ask::Choose { task: Task::Number(7), code: 2, .. }, .. })
    ));
}

#[test]
fn random_scripts_replay_and_respect_each_roles_actions() {
    for role in [Role::Owner, Role::Maintainer, Role::Member, Role::Observer] {
        let acts = random_acts(7, role, 8);
        assert_eq!(acts, random_acts(7, role, 8));
        for act in acts {
            match role {
                Role::Owner | Role::Maintainer => {
                    assert!(matches!(act, Act::Request { ask: Ask::Chat { .. } | Ask::Goal { .. }, .. }));
                }
                Role::Member => assert!(matches!(act, Act::Request { ask: Ask::Chat { .. }, .. })),
                Role::Observer => assert!(matches!(act, Act::Pause { .. })),
            }
        }
    }
}
