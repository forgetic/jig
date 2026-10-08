//! The notes referee checks only callers' intents and durable store records.
//! It never inspects the child domain (domain/testing.md, section 7).

use std::collections::BTreeMap;

use jig_core_notes::{Author, Change, Entry, Event, Key, Limits, Record, Scope};
use skein_lib::Token;

use crate::world::key_of;

/// One write staged by the parent before its atomic commit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Write {
    /// Save this durable record.
    Save(Record),
    /// Erase this durable key.
    Erase(Key),
}

/// What the caller asked before the child loaded any records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    /// An index or recall.
    Read { owner: Token },
    /// A task's write, with the revision it saw.
    Write { owner: Token, name: u64, recalled: Option<u32> },
    /// A party's correction or deletion, with the revision it saw.
    Edit { owner: Token, party: u64, name: u64, recalled: u32 },
}

impl Intent {
    /// Observe a caller event, before it is moved into the child.
    #[must_use]
    pub fn from_event(event: &Event) -> Intent {
        match event {
            Event::Index { owner, scopes: _, most: _ } | Event::Recall { owner, by: _, page: _ } => {
                Intent::Read { owner: *owner }
            }
            Event::Write { owner, entry, recalled } => {
                Intent::Write { owner: *owner, name: entry.name, recalled: *recalled }
            }
            Event::Edit { owner, party, name, change, .. } => {
                let recalled = match change {
                    Change::Correct { description: _, body: _, references: _, recalled }
                    | Change::Delete { recalled } => *recalled,
                };
                Intent::Edit { owner: *owner, party: *party, name: *name, recalled }
            }
            Event::Loaded { .. } | Event::LoadFailed { .. } | Event::Restore { .. } | Event::Restored => {
                unreachable!("a caller starts with a call")
            }
        }
    }

    /// The token the caller expects back.
    #[must_use]
    pub const fn owner(self) -> Token {
        match self {
            Intent::Read { owner } | Intent::Write { owner, .. } | Intent::Edit { owner, .. } => owner,
        }
    }
}

/// Checks commits and the resulting store from outside the notes child.
#[derive(Debug)]
pub struct Referee;

impl Referee {
    /// A commit saves or erases both records and never advances over an unseen revision.
    pub fn before_commit(&self, store: &BTreeMap<Key, Record>, writes: &[Write], intent: &Intent) {
        match writes {
            [Write::Save(Record::Entry(entry)), Write::Save(Record::Line(line))] => {
                assert_eq!(entry.name, line.name, "entry and index line name the same note");
                assert_eq!(entry.scope, line.scope, "entry and index line have one scope");
                assert_eq!(entry.description, line.description, "index line has the committed description");
                assert_eq!(entry.revision, line.revision, "index line has the committed revision");
                let recalled = match intent {
                    Intent::Write { name, recalled, .. } => {
                        assert_eq!(*name, entry.name, "write names its entry");
                        *recalled
                    }
                    Intent::Edit { party, name, recalled, .. } => {
                        assert_eq!(*name, entry.name, "edit names its entry");
                        assert_eq!(entry.author, Author::Party { party: *party }, "party authored the correction");
                        Some(*recalled)
                    }
                    Intent::Read { .. } => panic!("a read makes no write"),
                };
                match store.get(&Key::Entry { name: entry.name }) {
                    None => {
                        assert_eq!(recalled, None, "a new entry was not recalled");
                        assert_eq!(entry.revision, 1, "a new entry starts at revision one");
                    }
                    Some(Record::Entry(old)) => {
                        assert_eq!(recalled, Some(old.revision), "a revision names the one the writer saw");
                        assert_eq!(
                            entry.revision,
                            old.revision.checked_add(1).expect("revision fits"),
                            "revision advances once"
                        );
                        assert_eq!(entry.scope, old.scope, "an entry does not move between scopes");
                    }
                    Some(Record::Line(_)) => unreachable!("an entry key holds an entry"),
                }
            }
            [Write::Erase(Key::Entry { name }), Write::Erase(Key::Line { scope, name: line_name })] => {
                assert_eq!(name, line_name, "deletion erases entry and line together");
                let Intent::Edit { name: asked, recalled, .. } = intent else {
                    panic!("a party deletes an entry");
                };
                assert_eq!(name, asked, "deletion names the asked entry");
                let Some(Record::Entry(old)) = store.get(&Key::Entry { name: *name }) else {
                    panic!("a deletion has an entry");
                };
                assert_eq!(old.scope, *scope, "deletion erases the right scope line");
                assert_eq!(old.revision, *recalled, "deletion names the revision the party saw");
            }
            _ => panic!("one commit saves or erases an entry and its line together: {writes:?}"),
        }
    }

    /// Every durable entry and index line agree, and every scope stays bounded.
    pub fn after_commit(&self, store: &BTreeMap<Key, Record>, limits: &Limits) {
        let mut counts = BTreeMap::<Scope, u32>::new();
        for (key, record) in store {
            assert_eq!(key, &key_of(record), "record is stored under its own key");
            match record {
                Record::Entry(entry) => {
                    let line_key = Key::Line { scope: entry.scope.clone(), name: entry.name };
                    let Some(Record::Line(line)) = store.get(&line_key) else {
                        panic!("committed entry has its index line");
                    };
                    assert_eq!(line.description, entry.description, "index describes the committed entry");
                    assert_eq!(line.revision, entry.revision, "index gives the committed revision");
                    assert!(entry.description.len() <= usize::try_from(limits.description_bytes).expect("bound fits"));
                    assert!(entry.body.len() <= usize::try_from(limits.body_bytes).expect("bound fits"));
                    assert!(entry.references.len() <= limits.references);
                }
                Record::Line(line) => {
                    let Some(Record::Entry(entry)) = store.get(&Key::Entry { name: line.name }) else {
                        panic!("index line has its entry");
                    };
                    assert_eq!(entry.scope, line.scope, "index line belongs to the entry's scope");
                    let count = counts.entry(line.scope.clone()).or_insert(0);
                    *count = count.checked_add(1).expect("scope count fits");
                    assert!(*count <= limits.entries_per_scope, "nothing held beyond entries per scope");
                }
            }
        }
    }
}

/// A durable entry fixture for the referee's negative cases.
#[must_use]
pub fn saved_entry(name: u64, scope: Scope, revision: u32) -> Entry {
    Entry {
        name,
        scope,
        description: b"entry".as_slice().into(),
        body: b"body".as_slice().into(),
        references: skein_lib::List::with_capacity(0),
        author: Author::Task { task: 1, attempt: 1 },
        revision,
    }
}
