//! A parent and store for the notes child (domain/engine.md, section 10).

use std::collections::{BTreeMap, VecDeque};

use jig_core_notes::{
    self as notes, Change, Entry, Event, Key, Limits, Line, Range, Recall, Record, Refusal, Request, Rows, Scope,
};
use skein_lib::{Env, List, Queue, Time, Token, Wall};
use skein_world::domain::{Ledger, Trace};

use crate::referee::{Intent, Referee, Write};

/// Small limits that reach every boundary in the focused stories.
pub const LIMITS: Limits = Limits {
    scopes: 2,
    entries_per_scope: 3,
    pattern_bytes: 16,
    description_bytes: 32,
    body_bytes: 64,
    references: 2,
    load_rows: 2,
    lines: 3,
    recalled: 2,
};

/// A caller-visible answer, observed after the step's writes were committed.
#[derive(PartialEq, Eq, Debug)]
pub enum Answer {
    /// An index and its remainder count.
    Indexed { lines: List<Line>, more: u32 },
    /// A page of entries.
    Recalled { entries: List<Entry>, more: bool },
    /// An entry was written at this revision.
    Written { name: u64, revision: u32 },
    /// An entry was deleted.
    Deleted { name: u64 },
    /// The call changed nothing.
    Refused { why: Refusal },
}

/// The notes child, its scripted store, and boundary observations.
#[derive(Debug)]
pub struct World {
    domain: notes::Domain,
    store: BTreeMap<Key, Record>,
    loads: Ledger<Token, Range>,
    referee: Referee,
    limits: Limits,
    commits: u64,
    trace: Trace,
}

impl Default for World {
    fn default() -> World {
        World::new(LIMITS)
    }
}

impl World {
    /// A fresh world with an empty durable store.
    #[must_use]
    pub fn new(limits: Limits) -> World {
        World {
            domain: notes::Domain::new(&limits),
            store: BTreeMap::new(),
            loads: Ledger::new("notes store load"),
            referee: Referee,
            limits,
            commits: 0,
            trace: Trace::default(),
        }
    }

    /// Drive one caller event through store loads and one committed answer.
    pub fn call(&mut self, event: Event) -> Answer {
        let intent = Intent::from_event(&event);
        let caller = intent.owner();
        let mut incoming = VecDeque::from([event]);
        let mut answer = None;
        for _ in 0..128 {
            let Some(event) = incoming.pop_front() else { break };
            let requests = self.step_once(event);
            let mut writes = Vec::new();
            let mut loads = Vec::new();
            for request in requests {
                match request {
                    Request::Save { record } => writes.push(Write::Save(record)),
                    Request::Erase { key } => writes.push(Write::Erase(key)),
                    Request::Load { owner, range } => {
                        self.loads.open(owner, range.clone());
                        loads.push((owner, range));
                    }
                    Request::Indexed { owner, lines, more } => {
                        assert_eq!(owner, caller, "index answer echoes caller");
                        assert!(answer.replace(Answer::Indexed { lines, more }).is_none(), "one answer per call");
                    }
                    Request::Recalled { owner, entries, more } => {
                        assert_eq!(owner, caller, "recall answer echoes caller");
                        assert!(answer.replace(Answer::Recalled { entries, more }).is_none(), "one answer per call");
                    }
                    Request::Written { owner, name, revision } => {
                        assert_eq!(owner, caller, "write answer echoes caller");
                        assert!(answer.replace(Answer::Written { name, revision }).is_none(), "one answer per call");
                    }
                    Request::Deleted { owner, name } => {
                        assert_eq!(owner, caller, "delete answer echoes caller");
                        assert!(answer.replace(Answer::Deleted { name }).is_none(), "one answer per call");
                    }
                    Request::Refused { owner, why } => {
                        assert_eq!(owner, caller, "refusal echoes caller");
                        assert!(answer.replace(Answer::Refused { why }).is_none(), "one answer per call");
                    }
                }
            }
            if !writes.is_empty() {
                self.referee.before_commit(&self.store, &writes, &intent);
                for write in writes {
                    match write {
                        Write::Save(record) => {
                            self.store.insert(key_of(&record), record);
                        }
                        Write::Erase(key) => {
                            self.store.remove(&key);
                        }
                    }
                }
                self.commits = self.commits.checked_add(1).expect("world commit count fits");
                self.referee.after_commit(&self.store, &self.limits);
            }
            for (owner, range) in loads {
                let returned = self.loads.end(owner);
                assert_eq!(returned, range, "store answers the range it was asked for");
                let (rows, more) = self.serve(&range);
                incoming.push_back(Event::Loaded { owner, rows, more });
            }
        }
        assert!(incoming.is_empty(), "a bounded notes call settles");
        self.loads.assert_settled();
        answer.expect("a call has one answer")
    }

    /// Restart the child from the durable store and its end marker.
    pub fn restart(&mut self) {
        self.domain = notes::Domain::new(&self.limits);
        let records: Vec<_> = self.store.values().cloned().collect();
        for record in records {
            assert!(self.step_once(Event::Restore { record }).is_empty(), "restore emits nothing");
        }
        assert!(self.step_once(Event::Restored).is_empty(), "restored emits nothing");
        self.referee.after_commit(&self.store, &self.limits);
    }

    /// One stored entry, as a caller would read it through the store.
    #[must_use]
    pub fn stored_entry(&self, name: u64) -> Option<&Entry> {
        match self.store.get(&Key::Entry { name }) {
            Some(Record::Entry(entry)) => Some(entry),
            Some(Record::Line(_)) => unreachable!("an entry key holds an entry"),
            None => None,
        }
    }

    /// The number of committed write decisions.
    #[must_use]
    pub const fn commits(&self) -> u64 {
        self.commits
    }

    /// The boundary trace for deterministic replay.
    #[must_use]
    pub fn trace(&self) -> &[String] {
        self.trace.lines()
    }

    /// The durable records for replay comparison.
    #[must_use]
    pub fn records(&self) -> &BTreeMap<Key, Record> {
        &self.store
    }

    fn step_once(&mut self, event: Event) -> Vec<Request> {
        self.trace.log(Time::ZERO, format!("in {event:?}"));
        let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: self.limits };
        let mut out = Queue::with_capacity(notes::max_out(&self.limits));
        notes::step(&mut self.domain, &env, event, &mut out);
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            self.trace.log(Time::ZERO, format!("out {request:?}"));
            requests.push(request);
        }
        requests
    }

    fn serve(&self, range: &Range) -> (Rows, bool) {
        let mut records = List::with_capacity(self.limits.load_rows);
        match range {
            Range::Entry { name } => {
                if let Some(record) = self.store.get(&Key::Entry { name: *name }) {
                    records.push(record.clone()).expect("one entry fits one load page");
                }
                (Rows { records }, false)
            }
            Range::Lines { scope, after } => {
                let mut more = false;
                for (key, record) in &self.store {
                    match key {
                        Key::Entry { .. } => {}
                        Key::Line { scope: line_scope, name } => {
                            if line_scope == scope && after.is_none_or(|last| name > &last) {
                                if records.room() > 0 {
                                    records.push(record.clone()).expect("store page has room");
                                } else {
                                    more = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                (Rows { records }, more)
            }
        }
    }
}

/// The parent maps a child's record to its own store key.
#[must_use]
pub fn key_of(record: &Record) -> Key {
    match record {
        Record::Entry(entry) => Key::Entry { name: entry.name },
        Record::Line(line) => Key::Line { scope: line.scope.clone(), name: line.name },
    }
}

/// A task's entry for the stories.
#[must_use]
pub fn task_entry(name: u64, scope: Scope, description: &[u8], body: &[u8], task: u64) -> notes::New {
    notes::New {
        name,
        scope,
        description: description.into(),
        body: body.into(),
        references: List::with_capacity(LIMITS.references),
        author: notes::Author::Task { task, attempt: 1 },
    }
}

/// A single scope in a bounded list.
#[must_use]
pub fn one_scope(scope: Scope) -> List<Scope> {
    let mut scopes = List::with_capacity(1);
    scopes.push(scope).expect("one scope fits");
    scopes
}

/// A party's correction at the revision seen.
#[must_use]
pub fn correction(recalled: u32, description: &[u8], body: &[u8]) -> Change {
    Change::Correct {
        description: description.into(),
        body: body.into(),
        references: List::with_capacity(LIMITS.references),
        recalled,
    }
}

/// Search the requested scopes for descriptions containing `query`.
#[must_use]
pub fn search(scopes: List<Scope>, query: &[u8]) -> Recall {
    Recall::Search { scopes, query: query.into() }
}
