//! Scope indexes and recall (jig's domain/engine.md, section 10). Indexes
//! are loaded a page at a time and kept only while recently used. A recall
//! loads entry bodies on demand; its index search reads descriptions only.

use alloc::boxed::Box;

use skein_lib::{List, Map, Queue, Token, bytes};

use crate::boundary::{Entry, Line, Range, Recall, Record, Refusal, Request, Rows, Scope};
use crate::domain::{Domain, push, valid_scope};
use crate::limits::Limits;

/// One scope's in-memory index and when it was last used.
#[derive(Debug)]
pub(crate) struct Cached {
    pub(crate) lines: Map<u64, Line>,
    pub(crate) used: u64,
}

/// One read waiting for the store, or ready to answer from cached indexes.
#[derive(Debug)]
pub(crate) struct ReadPending {
    owner: Token,
    load_owner: Token,
    kind: Kind,
    stage: Stage,
}

#[derive(Debug)]
enum Kind {
    Index { scopes: List<Scope>, most: u32 },
    Name { name: u64 },
    Search { scopes: List<Scope>, query: Box<[u8]>, page: u32 },
}

#[derive(Debug)]
enum Stage {
    Ready,
    Lines { scope: Scope, after: Option<u64>, lines: Map<u64, Line> },
    Name,
    Entries { names: List<u64>, at: u32, entries: List<Entry>, more: bool },
}

pub(crate) fn index(
    domain: &mut Domain,
    limits: &Limits,
    owner: Token,
    scopes: List<Scope>,
    most: u32,
    out: &mut Queue<Request>,
) {
    if domain.busy() {
        push(out, Request::Refused { owner, why: Refusal::Busy });
    } else if !valid_scopes(&scopes, limits) {
        push(out, Request::Refused { owner, why: Refusal::Oversized });
    } else {
        domain.read = Some(ReadPending {
            owner,
            load_owner: domain.next_load_token(),
            kind: Kind::Index { scopes, most },
            stage: Stage::Ready,
        });
        drive(domain, limits, out);
    }
}

pub(crate) fn recall(
    domain: &mut Domain,
    limits: &Limits,
    owner: Token,
    by: Recall,
    page: u32,
    out: &mut Queue<Request>,
) {
    if domain.busy() {
        push(out, Request::Refused { owner, why: Refusal::Busy });
        return;
    }
    match by {
        Recall::Name { name } => {
            if page > 0 {
                push(out, Request::Recalled { owner, entries: List::with_capacity(0), more: false });
            } else {
                let load_owner = domain.next_load_token();
                domain.read = Some(ReadPending { owner, load_owner, kind: Kind::Name { name }, stage: Stage::Name });
                push(out, Request::Load { owner: load_owner, range: Range::Entry { name } });
            }
        }
        Recall::Search { scopes, query } => {
            let query_fits = match u32::try_from(query.len()) {
                Ok(len) => len <= limits.description_bytes,
                Err(_) => false,
            };
            if !valid_scopes(&scopes, limits) || !query_fits {
                push(out, Request::Refused { owner, why: Refusal::Oversized });
            } else {
                let load_owner = domain.next_load_token();
                domain.read = Some(ReadPending {
                    owner,
                    load_owner,
                    kind: Kind::Search { scopes, query, page },
                    stage: Stage::Ready,
                });
                drive(domain, limits, out);
            }
        }
    }
}

fn valid_scopes(scopes: &List<Scope>, limits: &Limits) -> bool {
    if scopes.capacity() > limits.scopes {
        return false;
    }
    for scope in scopes {
        if !valid_scope(scope, limits) {
            return false;
        }
    }
    true
}

/// Check a recall's bounded shape before a parent retains its payload.
#[must_use]
pub fn valid_recall(by: &Recall, limits: &Limits) -> bool {
    match by {
        Recall::Name { name } => *name != 0,
        Recall::Search { scopes, query } => {
            valid_scopes(scopes, limits)
                && match u32::try_from(query.len()) {
                    Ok(bytes) => bytes <= limits.description_bytes,
                    Err(_) => false,
                }
        }
    }
}

fn drive(domain: &mut Domain, limits: &Limits, out: &mut Queue<Request>) {
    let mut read = domain.read.take().expect("a read is ready to advance");
    match &read.kind {
        Kind::Name { name: _ } => unreachable!("name recall starts its entry load directly"),
        Kind::Index { scopes, most: _ } | Kind::Search { scopes, query: _, page: _ } => {
            if let Some(scope) = first_missing(domain, scopes) {
                read.stage = Stage::Lines {
                    scope: scope.clone(),
                    after: None,
                    lines: Map::with_capacity(limits.entries_per_scope),
                };
                read.load_owner = domain.next_load_token();
                push(out, Request::Load { owner: read.load_owner, range: Range::Lines { scope, after: None } });
                domain.read = Some(read);
                return;
            }
        }
    }
    match &read.kind {
        Kind::Index { scopes, most } => {
            let (lines, more) = collect_index(domain, scopes, (*most).min(limits.lines));
            push(out, Request::Indexed { owner: read.owner, lines, more });
        }
        Kind::Search { scopes, query, page } => {
            let (names, more) = select_names(domain, scopes, query, *page, limits.recalled);
            match names.get(0) {
                None => push(
                    out,
                    Request::Recalled { owner: read.owner, entries: List::with_capacity(limits.recalled), more: false },
                ),
                Some(name) => {
                    let name = *name;
                    read.stage = Stage::Entries { names, at: 0, entries: List::with_capacity(limits.recalled), more };
                    read.load_owner = domain.next_load_token();
                    push(out, Request::Load { owner: read.load_owner, range: Range::Entry { name } });
                    domain.read = Some(read);
                }
            }
        }
        Kind::Name { name: _ } => unreachable!("name recall starts its entry load directly"),
    }
}

fn first_missing(domain: &mut Domain, scopes: &List<Scope>) -> Option<Scope> {
    let mut missing = None;
    for scope in scopes {
        match domain.indexes.get_mut(scope) {
            Some(index) => {
                domain.clock = domain.clock.checked_add(1).expect("index use count fits u64");
                index.used = domain.clock;
            }
            None => {
                if missing.is_none() {
                    missing = Some(scope.clone());
                }
            }
        }
    }
    missing
}

fn collect_index(domain: &Domain, scopes: &List<Scope>, most: u32) -> (List<Line>, u32) {
    let mut lines = List::with_capacity(most);
    let mut more = 0u32;
    for scope in scopes {
        let index = domain.indexes.get(scope).expect("requested scope is cached");
        for (_, line) in &index.lines {
            if lines.room() > 0 {
                lines.push(line.clone()).expect("index line fits answer");
            } else {
                more = more.checked_add(1).expect("scope and entry limits bound the count");
            }
        }
    }
    (lines, more)
}

fn select_names(domain: &Domain, scopes: &List<Scope>, query: &[u8], page: u32, recalled: u32) -> (List<u64>, bool) {
    let skip = u64::from(page).checked_mul(u64::from(recalled)).expect("u32 page multiplication fits u64");
    let mut seen = 0u64;
    let mut names = List::with_capacity(recalled);
    let mut more = false;
    for scope in scopes {
        let index = domain.indexes.get(scope).expect("requested scope is cached");
        for (_, line) in &index.lines {
            if bytes::find(&line.description, query).is_some() {
                if seen >= skip {
                    if names.room() > 0 {
                        names.push(line.name).expect("recall page has room");
                    } else {
                        more = true;
                    }
                }
                seen = seen.checked_add(1).expect("scope and entry limits bound the count");
            }
        }
    }
    (names, more)
}

pub(crate) fn loaded(
    domain: &mut Domain,
    limits: &Limits,
    owner: Token,
    rows: Rows,
    more: bool,
    out: &mut Queue<Request>,
) {
    let mut read = domain.read.take().expect("a store page answers a read");
    assert!(read.load_owner == owner, "a store page echoes its load token");
    assert!(rows.records.capacity() <= limits.load_rows, "the store page fits its bound");
    match read.stage {
        Stage::Lines { scope, mut after, mut lines } => {
            assert!(!rows.records.is_empty() || !more, "a continued page advances");
            for record in rows.records.as_slice() {
                match record {
                    Record::Line(line) => {
                        assert!(line.scope == scope, "scope load returns its own lines");
                        match after {
                            None => {}
                            Some(last) => assert!(line.name > last, "scope lines are ordered"),
                        }
                        after = Some(line.name);
                        match lines.insert(line.name, line.clone()) {
                            Ok(None) => {}
                            Ok(Some(_)) => unreachable!("scope line keys are unique"),
                            Err(_) => unreachable!("entries per scope stay within their limit"),
                        }
                    }
                    Record::Entry(_) => unreachable!("scope load returns lines"),
                }
            }
            if more {
                read.stage = Stage::Lines { scope: scope.clone(), after, lines };
                read.load_owner = domain.next_load_token();
                push(out, Request::Load { owner: read.load_owner, range: Range::Lines { scope, after } });
                domain.read = Some(read);
            } else {
                let required = match &read.kind {
                    Kind::Index { scopes, most: _ } | Kind::Search { scopes, query: _, page: _ } => scopes,
                    Kind::Name { name: _ } => unreachable!("name recall does not load scope lines"),
                };
                insert_cached(domain, limits, required, scope, lines);
                read.stage = Stage::Ready;
                domain.read = Some(read);
                drive(domain, limits, out);
            }
        }
        Stage::Name => {
            assert!(!more, "a named entry fits one page");
            let name = match read.kind {
                Kind::Name { name } => name,
                Kind::Index { .. } | Kind::Search { .. } => unreachable!("name stage belongs to name recall"),
            };
            let mut entries = List::with_capacity(1);
            for record in rows.records.as_slice() {
                match record {
                    Record::Entry(entry) => {
                        assert!(entry.name == name, "entry lookup returns its named entry");
                        entries.push(entry.clone()).expect("one globally named entry");
                    }
                    Record::Line(_) => unreachable!("entry lookup returns entries"),
                }
            }
            push(out, Request::Recalled { owner: read.owner, entries, more: false });
        }
        Stage::Entries { names, at, mut entries, more: has_more } => {
            assert!(!more, "a named entry fits one page");
            let name = *names.get(at).expect("outstanding entry name exists");
            for record in rows.records.as_slice() {
                match record {
                    Record::Entry(entry) => {
                        assert!(entry.name == name, "entry lookup returns its named entry");
                        entries.push(entry.clone()).expect("recall page has room");
                    }
                    Record::Line(_) => unreachable!("entry lookup returns entries"),
                }
            }
            let next = at.checked_add(1).expect("recall page fits u32");
            match names.get(next) {
                None => push(out, Request::Recalled { owner: read.owner, entries, more: has_more }),
                Some(next_name) => {
                    let next_name = *next_name;
                    read.stage = Stage::Entries { names, at: next, entries, more: has_more };
                    read.load_owner = domain.next_load_token();
                    push(out, Request::Load { owner: read.load_owner, range: Range::Entry { name: next_name } });
                    domain.read = Some(read);
                }
            }
        }
        Stage::Ready => unreachable!("ready reads have no load outstanding"),
    }
}

/// Release a failed store read so the caller can retry with the same name.
pub(crate) fn failed(domain: &mut Domain, owner: Token, out: &mut Queue<Request>) {
    let pending = domain.read.take();
    match pending {
        Some(read) if read.load_owner == owner => push(out, Request::Refused { owner: read.owner, why: Refusal::Busy }),
        Some(read) => domain.read = Some(read),
        None => {}
    }
}

fn insert_cached(domain: &mut Domain, limits: &Limits, required: &List<Scope>, scope: Scope, lines: Map<u64, Line>) {
    if domain.indexes.len() >= limits.scopes {
        let mut victim = None;
        let mut oldest = u64::MAX;
        for (key, cached) in &domain.indexes {
            if !is_required(required, key) && cached.used <= oldest {
                victim = Some(key.clone());
                oldest = cached.used;
            }
        }
        let victim = victim.expect("a missing required scope has a nonrequired victim");
        drop(domain.indexes.remove(&victim));
    }
    domain.clock = domain.clock.checked_add(1).expect("index use count fits u64");
    domain.indexes.insert(scope, Cached { lines, used: domain.clock }).expect("room after eviction");
}

fn is_required(required: &List<Scope>, scope: &Scope) -> bool {
    for held in required {
        if held == scope {
            return true;
        }
    }
    false
}
