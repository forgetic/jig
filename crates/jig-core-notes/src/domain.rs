//! Notes' machine (jig's domain/engine.md, sections 5.3, 5.6 and 10).
//! It holds one pending operation and the recently used scope indexes, never
//! entry bodies beyond the pending operation. [`step`] is its entry point;
//! the parent echoes each store load token and commits emitted writes.
//!
//! | State | Event | Next | Emits |
//! |---|---|---|---|
//! | idle | write or edit | waiting for entry | load |
//! | waiting for entry | loaded | waiting for scope or idle | load, writes, or refusal |
//! | waiting for scope | loaded | waiting or idle | load, writes, or refusal |
//! | idle | index or recall | waiting for store or idle | load or answer |
//! | waiting for read | loaded | waiting or idle | load or answer |

use skein_lib::{Env, Map, Queue, Token};

use crate::boundary::{Author, Change, Entry, Event, Key, Line, New, Range, Record, Refusal, Request, Rows, Scope};
use crate::limits::Limits;
use crate::read::{self, Cached, ReadPending};

/// The largest number of requests one step emits.
pub const MAX_OUT: u32 = 3;

/// One write waiting for the store.
#[derive(Debug)]
pub(crate) struct Pending {
    owner: Token,
    load_owner: Token,
    action: Action,
    phase: Phase,
}

#[derive(Debug)]
enum Action {
    Write { entry: New, recalled: Option<u32> },
    Edit { party: u64, name: u64, scope: Scope, change: Change },
}

#[derive(Debug)]
enum Phase {
    Entry,
    Count { scope: Scope, count: u32, after: Option<u64> },
}

/// The notes child keeps pending work and scope indexes; entries live in the store.
#[derive(Debug)]
pub struct Domain {
    pending: Option<Pending>,
    pub(crate) read: Option<ReadPending>,
    pub(crate) indexes: Map<Scope, Cached>,
    pub(crate) clock: u64,
    next_load: u64,
}

impl Domain {
    /// Create an empty notes child.
    #[must_use]
    pub fn new(limits: &Limits) -> Domain {
        Domain { pending: None, read: None, indexes: Map::with_capacity(limits.scopes), clock: 0, next_load: 1 }
    }

    /// Whether a call is waiting for a store page.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.read.is_some()
    }

    /// The number of scope indexes held in memory.
    #[must_use]
    pub fn cached_scopes(&self) -> u32 {
        self.indexes.len()
    }

    /// Whether a scope's index is currently held.
    #[must_use]
    pub fn holds(&self, scope: &Scope) -> bool {
        self.indexes.contains_key(scope)
    }

    pub(crate) fn next_load_token(&mut self) -> Token {
        let token = Token::new(self.next_load);
        self.next_load = self.next_load.checked_add(1).expect("store load token space is sufficient");
        token
    }

    pub(crate) fn invalidate(&mut self, scope: &Scope) {
        drop(self.indexes.remove(scope));
    }
}

/// Room the parent reserves before stepping the child.
#[must_use]
pub const fn max_out(_limits: &Limits) -> u32 {
    MAX_OUT
}

/// Decide one event and emit writes or a store load.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::Index { owner, scopes, most } => read::index(domain, &env.limits, owner, scopes, most, out),
        Event::Recall { owner, by, page } => read::recall(domain, &env.limits, owner, by, page, out),
        Event::Write { owner, entry, recalled } => {
            if domain.busy() {
                push(out, Request::Refused { owner, why: Refusal::Busy });
            } else if !valid_new(&entry, &env.limits) {
                push(out, Request::Refused { owner, why: Refusal::Oversized });
            } else {
                begin(domain, owner, Action::Write { entry, recalled }, out);
            }
        }
        Event::Edit { owner, party, name, scope, change } => {
            if domain.busy() {
                push(out, Request::Refused { owner, why: Refusal::Busy });
            } else if !valid_scope(&scope, &env.limits) || !valid_change(&change, &env.limits) {
                push(out, Request::Refused { owner, why: Refusal::Oversized });
            } else {
                begin(domain, owner, Action::Edit { party, name, scope, change }, out);
            }
        }
        Event::LoadFailed { owner } => {
            let pending = domain.pending.take();
            match pending {
                Some(pending) if pending.load_owner == owner => {
                    push(out, Request::Refused { owner: pending.owner, why: Refusal::Busy });
                }
                Some(pending) => domain.pending = Some(pending),
                None => read::failed(domain, owner, out),
            }
        }
        Event::Loaded { owner, rows, more } => {
            if domain.pending.is_some() {
                loaded(domain, env, owner, rows, more, out);
            } else {
                read::loaded(domain, &env.limits, owner, rows, more, out);
            }
        }
        Event::Restore { record } => match record {
            Record::Entry(_) | Record::Line(_) => {}
        },
        Event::Restored => {}
    }
}

fn begin(domain: &mut Domain, owner: Token, action: Action, out: &mut Queue<Request>) {
    let name = match &action {
        Action::Write { entry, recalled: _ } => entry.name,
        Action::Edit { party: _, name, scope: _, change: _ } => *name,
    };
    let load_owner = domain.next_load_token();
    domain.pending = Some(Pending { owner, load_owner, action, phase: Phase::Entry });
    push(out, Request::Load { owner: load_owner, range: Range::Entry { name } });
}

fn loaded(domain: &mut Domain, env: &Env<Limits>, owner: Token, rows: Rows, more: bool, out: &mut Queue<Request>) {
    let mut pending = domain.pending.take().expect("a store page answers an outstanding load");
    assert!(pending.load_owner == owner, "a store page echoes its load token");
    assert!(rows.records.capacity() <= env.limits.load_rows, "the store page fits its bound");
    match pending.phase {
        Phase::Entry => {
            assert!(!more, "an entry lookup fits one page");
            let mut found = None;
            for record in rows.records.as_slice() {
                match record {
                    Record::Entry(entry) => {
                        assert!(found.is_none(), "one globally named entry");
                        found = Some(entry.clone());
                    }
                    Record::Line(_) => unreachable!("entry lookup returns entries"),
                }
            }
            entry_loaded(domain, pending, found, out);
        }
        Phase::Count { scope, mut count, mut after } => {
            assert!(!rows.records.is_empty() || !more, "a continued page advances");
            for record in rows.records.as_slice() {
                match record {
                    Record::Line(line) => {
                        assert!(line.scope == scope, "scope load returns its own lines");
                        match after {
                            None => {}
                            Some(last) => assert!(line.name > last, "scope lines are ordered"),
                        }
                        count = count.checked_add(1).expect("scope count fits u32");
                        after = Some(line.name);
                    }
                    Record::Entry(_) => unreachable!("scope load returns lines"),
                }
            }
            if more {
                pending.phase = Phase::Count { scope: scope.clone(), count, after };
                pending.load_owner = domain.next_load_token();
                push(out, Request::Load { owner: pending.load_owner, range: Range::Lines { scope, after } });
                domain.pending = Some(pending);
            } else if count >= env.limits.entries_per_scope {
                push(out, Request::Refused { owner: pending.owner, why: Refusal::Full });
            } else {
                match pending.action {
                    Action::Write { entry, recalled: None } => {
                        domain.invalidate(&entry.scope);
                        save_new(pending.owner, entry, 1, out);
                    }
                    Action::Write { entry: _, recalled: Some(_) } | Action::Edit { .. } => {
                        unreachable!("only a new entry counts a scope")
                    }
                }
            }
        }
    }
}

fn entry_loaded(domain: &mut Domain, mut pending: Pending, found: Option<Entry>, out: &mut Queue<Request>) {
    match pending.action {
        Action::Write { entry, recalled } => match found {
            None => match recalled {
                None => {
                    let scope = entry.scope.clone();
                    pending.action = Action::Write { entry, recalled: None };
                    pending.phase = Phase::Count { scope: scope.clone(), count: 0, after: None };
                    pending.load_owner = domain.next_load_token();
                    push(out, Request::Load { owner: pending.load_owner, range: Range::Lines { scope, after: None } });
                    domain.pending = Some(pending);
                }
                Some(_) => push(out, Request::Refused { owner: pending.owner, why: Refusal::Missing }),
            },
            Some(old) => match recalled {
                None => push(out, Request::Refused { owner: pending.owner, why: Refusal::Exists }),
                Some(recalled) => {
                    assert!(old.name == entry.name, "entry lookup returns the named entry");
                    if old.revision != recalled || old.scope != entry.scope {
                        push(out, Request::Refused { owner: pending.owner, why: Refusal::Moved });
                    } else if let Some(revision) = old.revision.checked_add(1) {
                        domain.invalidate(&entry.scope);
                        save_new(pending.owner, entry, revision, out);
                    } else {
                        push(out, Request::Refused { owner: pending.owner, why: Refusal::RevisionExhausted });
                    }
                }
            },
        },
        Action::Edit { party, name, scope, change } => match found {
            None => push(out, Request::Refused { owner: pending.owner, why: Refusal::Missing }),
            Some(mut old) => match change {
                Change::Correct { description, body, references, recalled } => {
                    assert!(old.name == name, "entry lookup returns the named entry");
                    if old.revision != recalled || old.scope != scope {
                        push(out, Request::Refused { owner: pending.owner, why: Refusal::Moved });
                    } else if let Some(revision) = old.revision.checked_add(1) {
                        old.description = description;
                        old.body = body;
                        old.references = references;
                        old.author = Author::Party { party };
                        old.revision = revision;
                        domain.invalidate(&old.scope);
                        save_entry(pending.owner, old, out);
                    } else {
                        push(out, Request::Refused { owner: pending.owner, why: Refusal::RevisionExhausted });
                    }
                }
                Change::Delete { recalled } => {
                    assert!(old.name == name, "entry lookup returns the named entry");
                    if old.revision == recalled && old.scope == scope {
                        domain.invalidate(&old.scope);
                        push(out, Request::Erase { key: Key::Entry { name } });
                        push(out, Request::Erase { key: Key::Line { scope: old.scope, name } });
                        push(out, Request::Deleted { owner: pending.owner, name });
                    } else {
                        push(out, Request::Refused { owner: pending.owner, why: Refusal::Moved });
                    }
                }
            },
        },
    }
}

fn save_new(owner: Token, new: New, revision: u32, out: &mut Queue<Request>) {
    save_entry(
        owner,
        Entry {
            name: new.name,
            scope: new.scope,
            description: new.description,
            body: new.body,
            references: new.references,
            author: new.author,
            revision,
        },
        out,
    );
}

fn save_entry(owner: Token, entry: Entry, out: &mut Queue<Request>) {
    let line = Line {
        scope: entry.scope.clone(),
        name: entry.name,
        description: entry.description.clone(),
        revision: entry.revision,
    };
    let name = entry.name;
    let revision = entry.revision;
    push(out, Request::Save { record: Record::Entry(entry) });
    push(out, Request::Save { record: Record::Line(line) });
    push(out, Request::Written { owner, name, revision });
}

/// Check a write's bounded shape before a parent retains its payload.
#[must_use]
pub fn valid_new(entry: &New, limits: &Limits) -> bool {
    valid_scope(&entry.scope, limits)
        && valid_text(&entry.description, limits.description_bytes, true)
        && valid_text(&entry.body, limits.body_bytes, false)
        && valid_references(&entry.references, limits)
}

/// Check one durable entry before a parent admits it from a store page.
#[must_use]
pub fn valid_entry(entry: &Entry, limits: &Limits) -> bool {
    let author_valid = match entry.author {
        Author::Party { party } => party != 0,
        Author::Task { task, attempt } => task != 0 && attempt != 0,
    };
    entry.name != 0
        && entry.revision != 0
        && author_valid
        && valid_scope(&entry.scope, limits)
        && valid_text(&entry.description, limits.description_bytes, true)
        && valid_text(&entry.body, limits.body_bytes, false)
        && valid_references(&entry.references, limits)
}

fn valid_references(references: &skein_lib::List<u64>, limits: &Limits) -> bool {
    if references.capacity() > limits.references {
        return false;
    }
    for reference in references {
        if *reference == 0 {
            return false;
        }
    }
    true
}

/// Check a durable scope index line before a parent admits it from a store page.
#[must_use]
pub fn valid_line(line: &Line, limits: &Limits) -> bool {
    line.name != 0
        && line.revision != 0
        && valid_scope(&line.scope, limits)
        && valid_text(&line.description, limits.description_bytes, true)
}

fn valid_change(change: &Change, limits: &Limits) -> bool {
    match change {
        Change::Correct { description, body, references, recalled: _ } => {
            valid_text(description, limits.description_bytes, true)
                && valid_text(body, limits.body_bytes, false)
                && references.capacity() <= limits.references
        }
        Change::Delete { recalled: _ } => true,
    }
}

pub(crate) fn valid_scope(scope: &Scope, limits: &Limits) -> bool {
    match scope {
        Scope::Deployment | Scope::Project { .. } | Scope::Goal { .. } => true,
        Scope::Resources { project: _, connector: _, pattern } => {
            let segments_fit = match u32::try_from(pattern.segments.len()) {
                Ok(segments) => segments <= limits.pattern_bytes,
                Err(_) => false,
            };
            let bytes_fit = match u32::try_from(pattern_bytes(pattern)) {
                Ok(bytes) => bytes <= limits.pattern_bytes,
                Err(_) => false,
            };
            segments_fit && bytes_fit
        }
    }
}

fn pattern_bytes(pattern: &crate::Pattern) -> usize {
    let mut total = 0_usize;
    for segment in &pattern.segments {
        total = total.saturating_add(segment.len());
    }
    let last = match &pattern.last {
        crate::Last::Exact(bytes) | crate::Last::Open(bytes) => bytes.len(),
    };
    total.saturating_add(last)
}

fn valid_text(bytes: &[u8], bound: u32, one_line: bool) -> bool {
    let fits = match u32::try_from(bytes.len()) {
        Ok(len) => len <= bound,
        Err(_) => false,
    };
    fits && (!one_line || (!bytes.contains(&b'\n') && !bytes.contains(&b'\r')))
}

pub(crate) fn push(out: &mut Queue<Request>, request: Request) {
    out.try_push(request).expect("parent reserved max_out before stepping notes");
}
