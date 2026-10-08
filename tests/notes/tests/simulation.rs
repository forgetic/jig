use jig_core_notes::{Author, Event, Recall, Refusal, Scope};
use jig_notes_world::{Answer, World, correction, one_scope, search, task_entry};
use skein_lib::{List, Token};

const PROJECT: Scope = Scope::Project { project: 7 };

#[test]
fn a_revision_of_an_entry_changed_since_it_was_recalled_is_refused_as_moved() {
    let mut world = World::default();
    assert_eq!(
        world.call(Event::Write {
            owner: Token::new(1),
            entry: task_entry(20, PROJECT, b"warmup", b"old", 9),
            recalled: None
        }),
        Answer::Written { name: 20, revision: 1 }
    );
    let recalled = world.call(Event::Recall { owner: Token::new(2), by: Recall::Name { name: 20 }, page: 0 });
    let Answer::Recalled { entries, more: false } = recalled else {
        panic!("entry recalled");
    };
    assert_eq!(entries.get(0).expect("one entry").revision, 1);
    assert_eq!(
        world.call(Event::Edit {
            owner: Token::new(3),
            party: 5,
            name: 20,
            scope: PROJECT,
            change: correction(1, b"warmup corrected", b"new")
        }),
        Answer::Written { name: 20, revision: 2 }
    );
    assert_eq!(
        world.call(Event::Write {
            owner: Token::new(4),
            entry: task_entry(20, PROJECT, b"stale", b"stale", 9),
            recalled: Some(1)
        }),
        Answer::Refused { why: Refusal::Moved }
    );
    assert_eq!(world.stored_entry(20).expect("entry retained").revision, 2);
    assert_eq!(world.commits(), 2);
}

#[test]
fn a_party_corrects_a_note_and_the_next_recall_sees_the_correction() {
    let mut world = World::default();
    let _written = world.call(Event::Write {
        owner: Token::new(1),
        entry: task_entry(20, PROJECT, b"warmup", b"old", 9),
        recalled: None,
    });
    let _corrected = world.call(Event::Edit {
        owner: Token::new(2),
        party: 5,
        name: 20,
        scope: PROJECT,
        change: correction(1, b"slow warmup", b"new"),
    });
    let got = world.call(Event::Recall { owner: Token::new(3), by: Recall::Name { name: 20 }, page: 0 });
    let Answer::Recalled { entries, more: false } = got else {
        panic!("entry recalled");
    };
    let entry = entries.get(0).expect("one entry");
    assert_eq!(&*entry.body, b"new");
    assert_eq!(entry.author, Author::Party { party: 5 });
    assert_eq!(entry.revision, 2);
}

#[test]
fn an_index_past_its_bound_says_how_many_more_and_recall_pages_through_them() {
    let mut world = World::default();
    for name in 20..23 {
        let _written = world.call(Event::Write {
            owner: Token::new(name),
            entry: task_entry(name, PROJECT, b"warm item", b"body", 9),
            recalled: None,
        });
    }
    let got = world.call(Event::Index { owner: Token::new(30), scopes: one_scope(PROJECT), most: 1 });
    let Answer::Indexed { lines, more: 2 } = got else {
        panic!("one line and two more: {got:?}");
    };
    assert_eq!(lines.get(0).expect("first line").name, 20);
    let got = world.call(Event::Recall { owner: Token::new(31), by: search(one_scope(PROJECT), b"warm"), page: 0 });
    let Answer::Recalled { entries, more: true } = got else {
        panic!("first recall page: {got:?}");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.get(0).expect("first").name, 20);
    assert_eq!(entries.get(1).expect("second").name, 21);
    let got = world.call(Event::Recall { owner: Token::new(32), by: search(one_scope(PROJECT), b"warm"), page: 1 });
    let Answer::Recalled { entries, more: false } = got else {
        panic!("last recall page: {got:?}");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries.get(0).expect("last").name, 22);
}

#[test]
fn an_index_orders_the_requested_scopes_and_evicts_the_least_recently_used() {
    let mut world = World::default();
    let deployment = Scope::Deployment;
    let goal = Scope::Goal { project: 7, goal: 11 };
    let project = Scope::Project { project: 7 };
    for (name, scope) in [(20, deployment.clone()), (21, goal.clone()), (22, project.clone())] {
        let _written = world.call(Event::Write {
            owner: Token::new(name),
            entry: task_entry(name, scope, b"warm item", b"body", 9),
            recalled: None,
        });
    }
    let mut scopes = List::with_capacity(2);
    scopes.push(goal.clone()).expect("scope fits");
    scopes.push(deployment.clone()).expect("scope fits");
    let got = world.call(Event::Index { owner: Token::new(30), scopes, most: 2 });
    let Answer::Indexed { lines, more: 0 } = got else {
        panic!("two scopes indexed: {got:?}");
    };
    assert_eq!(lines.get(0).expect("goal first").name, 21);
    assert_eq!(lines.get(1).expect("deployment second").name, 20);
    let _touch = world.call(Event::Index { owner: Token::new(31), scopes: one_scope(goal), most: 1 });
    let _new = world.call(Event::Index { owner: Token::new(32), scopes: one_scope(project), most: 1 });
    let loads_before = world.trace().iter().filter(|line| line.contains("out Load")).count();
    let got = world.call(Event::Index { owner: Token::new(33), scopes: one_scope(deployment), most: 1 });
    let Answer::Indexed { lines, more: 0 } = got else {
        panic!("evicted scope indexed: {got:?}");
    };
    assert_eq!(lines.get(0).expect("reloaded line").name, 20);
    let loads_after = world.trace().iter().filter(|line| line.contains("out Load")).count();
    assert_eq!(loads_after, loads_before + 1, "least recently used scope was reloaded");
}
