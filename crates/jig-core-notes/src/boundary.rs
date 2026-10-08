//! The notes child's store and caller vocabulary (jig's domain/engine.md,
//! sections 5.6 and 10). The parent commits writes and returns bounded loads.

use alloc::boxed::Box;

use skein_lib::{List, Token};

/// A connector's bounded resource pattern, preserving literal segment boundaries.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Pattern {
    pub segments: Box<[Box<[u8]>]>,
    pub last: Last,
}

/// The terminal matching rule of a resource pattern.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Last {
    Exact(Box<[u8]>),
    Open(Box<[u8]>),
}

/// Where a note applies.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Scope {
    /// The whole deployment.
    Deployment,
    /// One project.
    Project { project: u32 },
    /// One goal in a project.
    Goal { project: u32, goal: u64 },
    /// Resources matching a connector's pattern in a project.
    Resources { project: u32, connector: u16, pattern: Pattern },
}

/// Who last wrote an entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Author {
    /// A party corrected or made the entry.
    Party { party: u64 },
    /// A task's attempt wrote it.
    Task { task: u64, attempt: u64 },
}

/// A note kept in the store and loaded on demand.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Entry {
    pub name: u64,
    pub scope: Scope,
    pub description: Box<[u8]>,
    pub body: Box<[u8]>,
    pub references: List<u64>,
    pub author: Author,
    pub revision: u32,
}

/// The fields supplied for a new entry or correction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct New {
    pub name: u64,
    pub scope: Scope,
    pub description: Box<[u8]>,
    pub body: Box<[u8]>,
    pub references: List<u64>,
    pub author: Author,
}

/// The durable line used to load a scope's index without its bodies.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Line {
    pub scope: Scope,
    pub name: u64,
    pub description: Box<[u8]>,
    pub revision: u32,
}

/// A child's ordered store key.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Key {
    /// The entry, globally named.
    Entry { name: u64 },
    /// One line of a scope's index.
    Line { scope: Scope, name: u64 },
}

/// A durable record, encoded by the parent's store layer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Record {
    /// One entry.
    Entry(Entry),
    /// One scope index line.
    Line(Line),
}

/// Which records to load, starting after a previous page's last name.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Range {
    /// A globally named entry.
    Entry { name: u64 },
    /// The lines of one scope.
    Lines { scope: Scope, after: Option<u64> },
}

/// Bounded records returned for one load.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rows {
    pub records: List<Record>,
}

/// How entries are recalled from the store.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Recall {
    /// A globally named entry.
    Name { name: u64 },
    /// Entries whose descriptions contain `query`, in scope order.
    Search { scopes: List<Scope>, query: Box<[u8]> },
}

/// A party's change to an existing note.
#[derive(PartialEq, Eq, Debug)]
pub enum Change {
    /// Replace the contents seen at `recalled`.
    Correct { description: Box<[u8]>, body: Box<[u8]>, references: List<u64>, recalled: u32 },
    /// Remove the entry seen at `recalled`.
    Delete { recalled: u32 },
}

/// Why a write was refused without changing the store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Refusal {
    /// Another call is being served.
    Busy,
    /// A field exceeds its configured bound.
    Oversized,
    /// No more entries fit in this scope.
    Full,
    /// A new entry already has this name.
    Exists,
    /// The named entry is absent.
    Missing,
    /// The entry is newer than the revision the writer saw.
    Moved,
    /// The entry's revision has reached its numeric bound.
    RevisionExhausted,
}

/// Events from the parent and the store.
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// Lines in the requested scopes, capped to `most`, with a remainder count.
    Index { owner: Token, scopes: List<Scope>, most: u32 },
    /// One zero-based page of entries by name or description search.
    Recall { owner: Token, by: Recall, page: u32 },
    /// Write an entry, naming the revision this writer recalled, if any.
    Write { owner: Token, entry: New, recalled: Option<u32> },
    /// Correct or delete an entry on a party's request.
    Edit { owner: Token, party: u64, name: u64, scope: Scope, change: Change },
    /// The parent could not load the requested store page.
    LoadFailed { owner: Token },
    /// A bounded store page for the load with this owner.
    Loaded { owner: Token, rows: Rows, more: bool },
    /// One durable record supplied during restart.
    Restore { record: Record },
    /// All live records have been restored.
    Restored,
}

/// Requests to the parent, including writes in the current decision.
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// The requested index and the number of lines beyond it.
    Indexed { owner: Token, lines: List<Line>, more: u32 },
    /// One page of recalled entries; `more` says another page exists.
    Recalled { owner: Token, entries: List<Entry>, more: bool },
    /// Save a record under its own key in this decision.
    Save { record: Record },
    /// Erase a record under its own key in this decision.
    Erase { key: Key },
    /// Load one bounded page, echoing the owner in its answer.
    Load { owner: Token, range: Range },
    /// The write committed by this decision will put the entry at revision.
    Written { owner: Token, name: u64, revision: u32 },
    /// The deletion committed by this decision will remove the entry.
    Deleted { owner: Token, name: u64 },
    /// A call changed nothing.
    Refused { owner: Token, why: Refusal },
}
