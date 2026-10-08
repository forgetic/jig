//! Unit tests exercise the boundary and the requests it emits.

use alloc::boxed::Box;

use skein_lib::{Env, List, Queue, Time, Token, Wall};

use crate::{
    Author, Change, Domain, Entry, Event, Limits, Line, New, Range, Recall, Record, Refusal, Request, Rows, Scope,
    max_out, step, worst_case,
};

const LIMITS: Limits = Limits {
    scopes: 2,
    entries_per_scope: 1,
    pattern_bytes: 16,
    description_bytes: 16,
    body_bytes: 32,
    references: 2,
    load_rows: 2,
    lines: 2,
    recalled: 2,
};

fn new(name: u64, description: &[u8]) -> New {
    New {
        name,
        scope: Scope::Project { project: 7 },
        description: Box::from(description),
        body: Box::from(&b"body"[..]),
        references: List::with_capacity(2),
        author: Author::Task { task: 9, attempt: 1 },
    }
}

fn rows(records: &[Record]) -> Rows {
    rows_with_capacity(records, 2)
}

fn rows_with_capacity(records: &[Record], capacity: u32) -> Rows {
    let mut list = List::with_capacity(capacity);
    for record in records {
        list.push(record.clone()).expect("test page fits");
    }
    Rows { records: list }
}

struct Harness {
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
}

impl Harness {
    fn new() -> Harness {
        Harness::with_limits(LIMITS)
    }

    fn with_limits(limits: Limits) -> Harness {
        Harness {
            domain: Domain::new(&limits),
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            out: Queue::with_capacity(max_out(&limits)),
        }
    }

    fn step(&mut self, event: Event) -> Box<[Request]> {
        step(&mut self.domain, &self.env, event, &mut self.out);
        let mut got = List::with_capacity(self.out.len());
        while let Some(request) = self.out.pop() {
            got.push(request).expect("test output fits");
        }
        got.into_boxed()
    }
}

fn scopes(projects: &[u32], capacity: u32) -> List<Scope> {
    let mut scopes = List::with_capacity(capacity);
    for project in projects {
        scopes.push(Scope::Project { project: *project }).expect("test scopes fit");
    }
    scopes
}

fn line(project: u32, name: u64, description: &[u8]) -> Line {
    Line { scope: Scope::Project { project }, name, description: Box::from(description), revision: 1 }
}

fn entry(project: u32, name: u64, description: &[u8]) -> Entry {
    Entry {
        name,
        scope: Scope::Project { project },
        description: Box::from(description),
        body: Box::from(&b"body"[..]),
        references: List::with_capacity(2),
        author: Author::Task { task: 9, attempt: 1 },
        revision: 1,
    }
}

#[test]
fn a_new_entry_is_saved_with_its_scope_line_in_one_decision() {
    let mut h = Harness::new();
    let asked = h.step(Event::Write { owner: Token::new(4), entry: new(20, b"warmup"), recalled: None });
    let [Request::Load { owner, range: Range::Entry { name: 20 } }] = &*asked else {
        panic!("entry lookup: {asked:?}");
    };
    let owner = *owner;
    let asked = h.step(Event::Loaded { owner, rows: rows(&[]), more: false });
    let [Request::Load { owner, range: Range::Lines { .. } }] = &*asked else {
        panic!("scope lookup: {asked:?}");
    };
    let got = h.step(Event::Loaded { owner: *owner, rows: rows(&[]), more: false });
    let [
        Request::Save { record: Record::Entry(_) },
        Request::Save { record: Record::Line(_) },
        Request::Written { owner, name: 20, revision: 1 },
    ] = &*got
    else {
        panic!("new entry writes: {got:?}");
    };
    assert_eq!(*owner, Token::new(4));
    assert!(!h.domain.busy());
}

#[test]
fn a_revision_must_name_the_entry_the_writer_recalled() {
    let mut h = Harness::new();
    let asked = h.step(Event::Write { owner: Token::new(5), entry: new(20, b"revised"), recalled: Some(1) });
    let [Request::Load { owner, .. }] = &*asked else {
        panic!("entry lookup");
    };
    let old = Entry {
        name: 20,
        scope: Scope::Project { project: 7 },
        description: Box::from(&b"old"[..]),
        body: Box::from(&b"body"[..]),
        references: List::with_capacity(2),
        author: Author::Party { party: 2 },
        revision: 2,
    };
    let got = h.step(Event::Loaded { owner: *owner, rows: rows(&[Record::Entry(old)]), more: false });
    assert_eq!(&*got, &[Request::Refused { owner: Token::new(5), why: Refusal::Moved }]);
}

#[test]
fn a_party_deletes_only_the_revision_it_saw() {
    let mut h = Harness::new();
    let asked = h.step(Event::Edit {
        owner: Token::new(6),
        party: 2,
        name: 20,
        scope: Scope::Project { project: 7 },
        change: Change::Delete { recalled: 2 },
    });
    let [Request::Load { owner, .. }] = &*asked else {
        panic!("entry lookup");
    };
    let old = Entry {
        name: 20,
        scope: Scope::Project { project: 7 },
        description: Box::from(&b"old"[..]),
        body: Box::from(&b"body"[..]),
        references: List::with_capacity(2),
        author: Author::Task { task: 1, attempt: 1 },
        revision: 2,
    };
    let got = h.step(Event::Loaded { owner: *owner, rows: rows(&[Record::Entry(old)]), more: false });
    let [Request::Erase { .. }, Request::Erase { .. }, Request::Deleted { owner, name: 20 }] = &*got else {
        panic!("delete writes: {got:?}");
    };
    assert_eq!(*owner, Token::new(6));
}

#[test]
fn a_full_scope_refuses_a_new_entry() {
    let mut h = Harness::new();
    let asked = h.step(Event::Write { owner: Token::new(7), entry: new(21, b"new"), recalled: None });
    let [Request::Load { owner, .. }] = &*asked else {
        panic!("entry lookup");
    };
    let owner = *owner;
    let asked = h.step(Event::Loaded { owner, rows: rows(&[]), more: false });
    let [Request::Load { owner, range: Range::Lines { .. } }] = &*asked else {
        panic!("scope lookup");
    };
    let owner = *owner;
    let line =
        Line { scope: Scope::Project { project: 7 }, name: 20, description: Box::from(&b"old"[..]), revision: 1 };
    let got = h.step(Event::Loaded { owner, rows: rows(&[Record::Line(line)]), more: false });
    assert_eq!(&*got, &[Request::Refused { owner: Token::new(7), why: Refusal::Full }]);
}

#[test]
fn the_memory_bound_accepts_the_test_limits() {
    assert!(worst_case(&LIMITS).is_some());
}

#[test]
fn an_index_pages_scope_lines_and_counts_what_does_not_fit() {
    let mut limits = LIMITS;
    limits.entries_per_scope = 2;
    limits.load_rows = 1;
    limits.lines = 1;
    let mut h = Harness::with_limits(limits);
    let asked = h.step(Event::Index { owner: Token::new(30), scopes: scopes(&[7], 1), most: 1 });
    let [Request::Load { owner, range: Range::Lines { scope: Scope::Project { project: 7 }, after: None } }] = &*asked
    else {
        panic!("first index page: {asked:?}");
    };
    let asked = h.step(Event::Loaded {
        owner: *owner,
        rows: rows_with_capacity(&[Record::Line(line(7, 20, b"first"))], 1),
        more: true,
    });
    let [Request::Load { owner, range: Range::Lines { scope: Scope::Project { project: 7 }, after: Some(20) } }] =
        &*asked
    else {
        panic!("second index page: {asked:?}");
    };
    let got = h.step(Event::Loaded {
        owner: *owner,
        rows: rows_with_capacity(&[Record::Line(line(7, 21, b"second"))], 1),
        more: false,
    });
    let [Request::Indexed { owner, lines, more: 1 }] = &*got else {
        panic!("index answer: {got:?}");
    };
    assert_eq!(*owner, Token::new(30));
    assert_eq!(lines.get(0).expect("one line").name, 20);
    assert_eq!(h.domain.cached_scopes(), 1);
}

#[test]
fn a_description_search_recalls_one_page_of_bodies_at_a_time() {
    let mut limits = LIMITS;
    limits.entries_per_scope = 2;
    limits.recalled = 1;
    let mut h = Harness::with_limits(limits);
    let asked = h.step(Event::Index { owner: Token::new(30), scopes: scopes(&[7], 1), most: 2 });
    let [Request::Load { owner, .. }] = &*asked else {
        panic!("index load");
    };
    let _indexed = h.step(Event::Loaded {
        owner: *owner,
        rows: rows(&[Record::Line(line(7, 20, b"warm first")), Record::Line(line(7, 21, b"warm second"))]),
        more: false,
    });
    let by = Recall::Search { scopes: scopes(&[7], 1), query: Box::from(&b"warm"[..]) };
    let asked = h.step(Event::Recall { owner: Token::new(31), by, page: 0 });
    let [Request::Load { owner, range: Range::Entry { name: 20 } }] = &*asked else {
        panic!("first recall: {asked:?}");
    };
    let got =
        h.step(Event::Loaded { owner: *owner, rows: rows(&[Record::Entry(entry(7, 20, b"warm first"))]), more: false });
    let [Request::Recalled { owner, entries, more: true }] = &*got else {
        panic!("first page: {got:?}");
    };
    assert_eq!(*owner, Token::new(31));
    assert_eq!(entries.get(0).expect("one entry").name, 20);
    let by = Recall::Search { scopes: scopes(&[7], 1), query: Box::from(&b"warm"[..]) };
    let asked = h.step(Event::Recall { owner: Token::new(32), by, page: 1 });
    let [Request::Load { owner, range: Range::Entry { name: 21 } }] = &*asked else {
        panic!("second recall: {asked:?}");
    };
    let got = h.step(Event::Loaded {
        owner: *owner,
        rows: rows(&[Record::Entry(entry(7, 21, b"warm second"))]),
        more: false,
    });
    let [Request::Recalled { owner, entries, more: false }] = &*got else {
        panic!("second page: {got:?}");
    };
    assert_eq!(*owner, Token::new(32));
    assert_eq!(entries.get(0).expect("one entry").name, 21);
}

#[test]
fn a_new_scope_evicts_the_least_recently_used_index() {
    let mut h = Harness::new();
    for project in [7, 8] {
        let asked =
            h.step(Event::Index { owner: Token::new(u64::from(project)), scopes: scopes(&[project], 1), most: 1 });
        let [Request::Load { owner, .. }] = &*asked else {
            panic!("scope load");
        };
        let _answer = h.step(Event::Loaded { owner: *owner, rows: rows(&[]), more: false });
    }
    let got = h.step(Event::Index { owner: Token::new(70), scopes: scopes(&[7], 1), most: 1 });
    let [Request::Indexed { .. }] = &*got else {
        panic!("cached index");
    };
    let asked = h.step(Event::Index { owner: Token::new(9), scopes: scopes(&[9], 1), most: 1 });
    let [Request::Load { owner, .. }] = &*asked else {
        panic!("new scope load");
    };
    let _answer = h.step(Event::Loaded { owner: *owner, rows: rows(&[]), more: false });
    assert!(h.domain.holds(&Scope::Project { project: 7 }));
    assert!(!h.domain.holds(&Scope::Project { project: 8 }));
    assert!(h.domain.holds(&Scope::Project { project: 9 }));
}

#[test]
fn a_write_invalidates_the_cached_scope_index() {
    let mut h = Harness::new();
    let asked = h.step(Event::Index { owner: Token::new(30), scopes: scopes(&[7], 1), most: 1 });
    let [Request::Load { owner, .. }] = &*asked else {
        panic!("scope load");
    };
    let _answer =
        h.step(Event::Loaded { owner: *owner, rows: rows(&[Record::Line(line(7, 20, b"old"))]), more: false });
    assert!(h.domain.holds(&Scope::Project { project: 7 }));
    let asked = h.step(Event::Write { owner: Token::new(31), entry: new(20, b"new"), recalled: Some(1) });
    let [Request::Load { owner, range: Range::Entry { name: 20 } }] = &*asked else {
        panic!("entry load");
    };
    let got = h.step(Event::Loaded { owner: *owner, rows: rows(&[Record::Entry(entry(7, 20, b"old"))]), more: false });
    let [Request::Save { .. }, Request::Save { .. }, Request::Written { revision: 2, .. }] = &*got else {
        panic!("entry revision: {got:?}");
    };
    assert!(!h.domain.holds(&Scope::Project { project: 7 }));
}

#[test]
fn a_named_recall_loads_its_entry_without_caching_the_body() {
    let mut h = Harness::new();
    let asked = h.step(Event::Recall { owner: Token::new(50), by: Recall::Name { name: 20 }, page: 0 });
    let [Request::Load { owner, range: Range::Entry { name: 20 } }] = &*asked else {
        panic!("entry lookup: {asked:?}");
    };
    let got = h.step(Event::Loaded { owner: *owner, rows: rows(&[Record::Entry(entry(7, 20, b"warm"))]), more: false });
    let [Request::Recalled { owner, entries, more: false }] = &*got else {
        panic!("entry answer: {got:?}");
    };
    assert_eq!(*owner, Token::new(50));
    assert_eq!(entries.get(0).expect("one entry").name, 20);
    assert_eq!(h.domain.cached_scopes(), 0);
}
