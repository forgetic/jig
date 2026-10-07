use std::collections::BTreeMap;

use jig_core_notes::{Key, Line, Record, Scope};
use jig_notes_world::{Intent, LIMITS, Referee, Write, key_of, saved_entry};
use skein_lib::Token;

fn line(entry: &jig_core_notes::Entry) -> Line {
    Line {
        scope: entry.scope.clone(),
        name: entry.name,
        description: entry.description.clone(),
        revision: entry.revision,
    }
}

#[test]
#[should_panic(expected = "entry and its line together")]
fn the_referee_rejects_a_commit_missing_its_index_line() {
    let entry = saved_entry(20, Scope::Project { project: 7 }, 1);
    Referee.before_commit(
        &BTreeMap::new(),
        &[Write::Save(Record::Entry(entry))],
        &Intent::Write { owner: Token::new(1), name: 20, recalled: None },
    );
}

#[test]
#[should_panic(expected = "revision names the one the writer saw")]
fn the_referee_rejects_a_revision_over_one_the_writer_did_not_see() {
    let old = saved_entry(20, Scope::Project { project: 7 }, 1);
    let mut store = BTreeMap::new();
    store.insert(key_of(&Record::Entry(old.clone())), Record::Entry(old.clone()));
    store.insert(key_of(&Record::Line(line(&old))), Record::Line(line(&old)));
    let newer = saved_entry(20, Scope::Project { project: 7 }, 2);
    Referee.before_commit(
        &store,
        &[Write::Save(Record::Entry(newer.clone())), Write::Save(Record::Line(line(&newer)))],
        &Intent::Write { owner: Token::new(1), name: 20, recalled: Some(0) },
    );
}

#[test]
#[should_panic(expected = "nothing held beyond entries per scope")]
fn the_referee_rejects_more_entries_than_a_scope_allows() {
    let mut store = BTreeMap::<Key, Record>::new();
    for name in 20..24 {
        let entry = saved_entry(name, Scope::Project { project: 7 }, 1);
        let index = line(&entry);
        store.insert(key_of(&Record::Entry(entry.clone())), Record::Entry(entry));
        store.insert(key_of(&Record::Line(index.clone())), Record::Line(index));
    }
    Referee.after_commit(&store, &LIMITS);
}
