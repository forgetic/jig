//! Live views of runs, task trees, goals, and inboxes (domain/engine.md, 11).
//! A watch begins with its parent's snapshot. Each delivery is answered before
//! the next leaves; a full backlog loses old chunks and tells the watcher.
//! Views keep no durable state.

use alloc::boxed::Box;
use skein_lib::{Time, Token};

/// The kind of live report a host sent.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Kind {
    /// Text as it arrives.
    Text,
    /// Progress through a run.
    Progress,
    /// A model call.
    Call,
    /// A tool call.
    Tool,
    /// A turn's usage.
    Usage,
}

/// One of the four live subjects (domain/engine.md, 11).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Subject {
    /// One attempt's stream.
    Run { task: Token, attempt: Token },
    /// A task and its delegate tree.
    Tree { task: Token },
    /// A project's goals.
    Goals { project: u32 },
    /// A party's inbox.
    Inbox { party: u64 },
}

/// A piece of a watcher's stream.
#[derive(PartialEq, Eq, Hash, Debug)]
pub enum Chunk {
    /// The parent's view when the watch began.
    Snapshot { at: Time, content: Box<[u8]> },
    /// A host's report or committed turn.
    Report { task: Token, attempt: Token, kind: Kind, at: Time, content: Box<[u8]> },
    /// A task's phase changed.
    Phase { task: Token, phase: u32, at: Time },
    /// A party's inbox changed.
    Inbox { party: u64, at: Time, content: Box<[u8]> },
}

/// Why admission refused a watch.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Refusal {
    /// All watch slots are in use.
    Busy,
    /// The run or attempt is no longer live.
    Unknown,
    /// The run could not be followed when it started.
    Unfollowed,
    /// The parent's snapshot exceeds its bound.
    Oversized,
}

/// Why a watch ended.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum End {
    /// Its watcher left.
    Unwatched,
    /// Its run finished.
    Finished,
}

/// Events from the parent.
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// One attempt started, for a task.
    Started { task: Token, attempt: Token },
    /// A host reported content during the live attempt.
    Reported { task: Token, attempt: Token, kind: Kind, content: Box<[u8]> },
    /// A turn became durable.
    Turn { task: Token, attempt: Token, number: u32 },
    /// The live attempt ended.
    Finished { task: Token },
    /// A committed task phase, and the trees containing it.
    TaskPhase { task: Token, trees: Box<[Token]>, project: u32, phase: u32, priority: Option<u32> },
    /// A party's inbox changed.
    Inbox { party: u64, content: Box<[u8]> },
    /// Start watching from the supplied snapshot.
    Watch { watcher: Token, subject: Subject, snapshot: Box<[u8]> },
    /// Stop watching.
    Unwatch { watcher: Token },
    /// The last delivery was consumed or dropped.
    Delivered { watcher: Token, done: bool },
}

/// Requests to the parent.
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// The watch was admitted.
    Watching { watcher: Token },
    /// The watch was refused.
    Refused { watcher: Token, refusal: Refusal },
    /// Deliver ordered chunks, with the count lost since the last delivery.
    Deliver { watcher: Token, missed: u64, chunks: Box<[Chunk]> },
    /// The watch has ended.
    Ended { watcher: Token, end: End },
}
