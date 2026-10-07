//! Watchers: a party's live stream of a run, tree, goals, or inbox
//! (domain/engine.md, section 11).
//!
//! A watch begins with the snapshot the parent gave with it, delivered at
//! once. A watcher has at most one delivery in flight. A chunk for a watcher
//! that has none goes out at once; one for a watcher with a delivery in
//! flight waits in its backlog, and the whole backlog goes out in one
//! delivery once that one has ended. The backlog is bounded: a chunk that
//! finds it full drops what waits, counted as missed, and waits in its
//! place, so the watcher is told how much it missed right before the chunks
//! it catches up from. What a delivery held that its stream did not take in
//! time, and a report the watcher would have had that was dropped, are
//! counted as missed too, and told with the next delivery. Nothing a watcher
//! does holds anything else up, and nothing grows.
//!
//! A watch ends once the person stops watching, or once its run has finished
//! and the watcher has had what waited for it; it ends only once its
//! delivery in flight has, so the parent's token for it is free when it is
//! told so.
//!
//! The transition table. A miss counts a report the watcher would have had,
//! dropped; the other cells are unreachable, as a closed watcher is no
//! longer named.
//!
//! ```text
//! state     event                      next      requests
//! (none)    watch, admitted            Sending   watching; its snapshot
//!           watch, refused             (none)    refused
//! Idle      chunk                      Sending   deliver it, and the missed
//!           miss                       Idle      (one more missed)
//!           delivered                  Idle      (dropped: none in flight)
//!           unwatch                    Closed    ended, unwatched
//!           its run finished           Closed    ended, finished
//! Sending   chunk, room in the backlog Sending   (it waits)
//!           chunk, the backlog full    Sending   (what waits is missed)
//!           miss                       Sending   (one more missed)
//!           delivered, none waiting    Idle      (missed what it held, if
//!                                                 not taken)
//!           delivered, chunks waiting  Sending   deliver them, and the missed
//!           unwatch                    Closing   (what waits is dropped)
//!           its run finished           Draining
//! Draining  chunk, miss                Draining  (one more missed)
//!           delivered, chunks waiting  Draining  deliver them, and the missed
//!           delivered, none waiting    Closed    ended, finished
//!           unwatch                    Closing   (what waits is dropped)
//!           its run finished           Draining
//! Closing   chunk, miss, unwatch, or
//!             its run finished         Closing   (dropped)
//!           delivered                  Closed    ended, unwatched
//! ```

use alloc::boxed::Box;
use core::mem;

use skein_lib::bytes::copy_of;
use skein_lib::{Env, Id, List, Queue, Time, Token};

use crate::boundary::{Chunk, End, Kind, Refusal, Request, Subject};
use crate::domain::Domain;
use crate::facts::{Fact, Facts, Loss};
use crate::limits::Limits;

/// A person watching, under the parent's token for the watch.
#[derive(Debug)]
pub(crate) struct Watcher {
    pub(crate) token: Token,
    pub(crate) subject: Subject,
    /// What waits for the delivery in flight to end, oldest first: empty
    /// unless one is in flight.
    pub(crate) backlog: Queue<Chunk>,
    pub(crate) state: Stream,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Stream {
    /// No delivery in flight, and nothing waits; `missed` chunks were lost
    /// since the last delivery the stream took, to tell with the next.
    Idle { missed: u64 },
    /// A delivery is in flight, and `missed` chunks were lost since it went.
    Sending { missed: u64, flight: Flight },
    /// As `Sending`, and the watched run has finished: the watch ends once
    /// the backlog is out.
    Draining { missed: u64, flight: Flight },
    /// The person stopped watching while a delivery was in flight: the watch
    /// ends once it has.
    Closing,
    /// Terminal: holds nothing.
    Closed,
}

/// The delivery in flight: what it told was missed, and the chunks it
/// holds, all missed in turn if its stream does not take it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Flight {
    told: u64,
    chunks: u64,
}

/// A chunk less its content: what each watcher's copy is made from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Head {
    Report { task: Token, attempt: Token, kind: Kind, at: Time },
    Phase { task: Token, phase: u32, at: Time },
    Inbox { party: u64, at: Time },
}

/// A watch: refused at the entrance, or taken, and delivered its snapshot.
pub(crate) fn watch(
    domain: &mut Domain,
    env: &Env<Limits>,
    token: Token,
    subject: Subject,
    snapshot: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    assert!(!domain.names.contains_key(&token), "the parent names each watch it has open once");
    if let Some(refusal) = refusal(domain, &env.limits, subject, &snapshot) {
        domain.facts.push(Fact::Refused { refusal });
        out.push(Request::Refused { watcher: token, refusal });
        return;
    }
    let backlog = Queue::with_capacity(env.limits.backlog);
    let watcher = Watcher { token, subject, backlog, state: Stream::Closed };
    let id = domain.watchers.insert(watcher).expect("room for a watcher, checked at the entrance");
    let named = domain.names.insert(token, id);
    assert!(named == Ok(None), "a name for each watcher, and a watcher for each name");
    domain.facts.push(Fact::Watching);
    out.push(Request::Watching { watcher: token });
    let first = Chunk::Snapshot { at: env.now, content: snapshot };
    let watcher = domain.watchers.get_mut(id).expect("inserted above");
    watcher.state = send(token, first, 0, &mut domain.facts, out);
}

/// Why a watch is refused at the entrance, if it is: of a run not followed,
/// with a snapshot past the limits, or with no room for one more watcher,
/// open or ended in this iteration.
fn refusal(domain: &Domain, limits: &Limits, subject: Subject, snapshot: &[u8]) -> Option<Refusal> {
    match subject {
        Subject::Run { task, attempt } => {
            if domain.unfollowed.get(&task) == Some(&attempt) {
                return Some(Refusal::Unfollowed);
            }
            if domain.runs.get(&task) != Some(&attempt) {
                return Some(Refusal::Unknown);
            }
        }
        Subject::Tree { .. } | Subject::Goals { .. } | Subject::Inbox { .. } => {}
    }
    let within = match u32::try_from(snapshot.len()) {
        Ok(len) => len <= limits.snapshot_bytes,
        Err(_) => false,
    };
    if !within {
        return Some(Refusal::Oversized);
    }
    if domain.names.len() >= limits.watchers || domain.watchers.is_full() { Some(Refusal::Busy) } else { None }
}

/// The person stopped watching. A watch that has ended already is no longer
/// named, and the event is dropped.
pub(crate) fn unwatch(domain: &mut Domain, token: Token, out: &mut Queue<Request>) {
    let Some(&id) = domain.names.get(&token) else {
        return;
    };
    let watcher = domain.watchers.get_mut(id).expect("a named watcher is live");
    let state = mem::replace(&mut watcher.state, Stream::Closed);
    watcher.state = match state {
        Stream::Idle { .. } => close(token, End::Unwatched, &mut domain.facts, out),
        Stream::Sending { .. } | Stream::Draining { .. } => abandon(&mut watcher.backlog),
        Stream::Closing => Stream::Closing,
        Stream::Closed => unreachable!("a closed watcher is no longer named"),
    };
    conclude(domain, id);
}

/// The delivery in flight to `token` has ended, taken by its stream if
/// `done`. One to a watch that is no longer named, or that has none in
/// flight, is dropped.
pub(crate) fn delivered(domain: &mut Domain, token: Token, done: bool, out: &mut Queue<Request>) {
    let Some(&id) = domain.names.get(&token) else {
        return;
    };
    let watcher = domain.watchers.get_mut(id).expect("a named watcher is live");
    let state = mem::replace(&mut watcher.state, Stream::Closed);
    watcher.state = match state {
        Stream::Idle { missed } => Stream::Idle { missed },
        Stream::Sending { missed, flight } => {
            let missed = landed(missed, flight, done, &mut domain.facts);
            caught_up(token, &mut watcher.backlog, missed, &mut domain.facts, out)
        }
        Stream::Draining { missed, flight } => {
            let missed = landed(missed, flight, done, &mut domain.facts);
            drained(token, &mut watcher.backlog, missed, &mut domain.facts, out)
        }
        Stream::Closing => close(token, End::Unwatched, &mut domain.facts, out),
        Stream::Closed => unreachable!("a closed watcher is no longer named"),
    };
    conclude(domain, id);
}

/// Offers a chunk made from `head` and `content` to each watcher of `first`
/// or of `second`, and says how many took it.
pub(crate) fn offer(
    domain: &mut Domain,
    first: Subject,
    second: Subject,
    head: Head,
    content: &[u8],
    out: &mut Queue<Request>,
) -> u32 {
    let mut took: u32 = 0;
    for (_, &id) in &domain.names {
        let watcher = domain.watchers.get_mut(id).expect("a named watcher is live");
        if watcher.subject != first && watcher.subject != second {
            continue;
        }
        let state = mem::replace(&mut watcher.state, Stream::Closed);
        watcher.state = match state {
            Stream::Idle { missed } => {
                took = took.saturating_add(1);
                send(watcher.token, make(head, content), missed, &mut domain.facts, out)
            }
            Stream::Sending { missed, flight } => {
                took = took.saturating_add(1);
                let missed = wait(&mut watcher.backlog, head, content, missed, &mut domain.facts);
                Stream::Sending { missed, flight }
            }
            Stream::Draining { missed, flight } => Stream::Draining { missed: lose(missed, &mut domain.facts), flight },
            Stream::Closing => Stream::Closing,
            Stream::Closed => unreachable!("a closed watcher is no longer named"),
        };
    }
    took
}

/// A report that each watcher of `first` or of `second` would have had was
/// dropped: they are told they missed it.
pub(crate) fn miss(domain: &mut Domain, first: Subject, second: Subject) {
    for (_, &id) in &domain.names {
        let watcher = domain.watchers.get_mut(id).expect("a named watcher is live");
        if watcher.subject != first && watcher.subject != second {
            continue;
        }
        watcher.state = match watcher.state {
            Stream::Idle { missed } => Stream::Idle { missed: lose(missed, &mut domain.facts) },
            Stream::Sending { missed, flight } => Stream::Sending { missed: lose(missed, &mut domain.facts), flight },
            Stream::Draining { missed, flight } => Stream::Draining { missed: lose(missed, &mut domain.facts), flight },
            Stream::Closing => Stream::Closing,
            Stream::Closed => unreachable!("a closed watcher is no longer named"),
        };
    }
}

/// The run `run` has finished: its watchers end, once they have had what
/// waits for them.
pub(crate) fn finish(domain: &mut Domain, subject: Subject, out: &mut Queue<Request>) {
    // Ending a watcher unnames it, so the watchers are found first.
    let mut found = List::with_capacity(domain.names.len());
    for (_, &id) in &domain.names {
        let watcher = domain.watchers.get(id).expect("a named watcher is live");
        if watcher.subject == subject {
            found.push(id).expect("room for every watcher");
        }
    }
    for &id in &found {
        let watcher = domain.watchers.get_mut(id).expect("a watcher found is live");
        let token = watcher.token;
        let state = mem::replace(&mut watcher.state, Stream::Closed);
        watcher.state = match state {
            Stream::Idle { .. } => close(token, End::Finished, &mut domain.facts, out),
            Stream::Sending { missed, flight } | Stream::Draining { missed, flight } => {
                Stream::Draining { missed, flight }
            }
            Stream::Closing => Stream::Closing,
            Stream::Closed => unreachable!("a closed watcher is no longer named"),
        };
        conclude(domain, id);
    }
}

/// A copy of the chunk `head` and `content` make, for one watcher.
fn make(head: Head, content: &[u8]) -> Chunk {
    match head {
        Head::Report { task, attempt, kind, at } => {
            Chunk::Report { task, attempt, kind, at, content: copy_of(content) }
        }
        Head::Phase { task, phase, at } => Chunk::Phase { task, phase, at },
        Head::Inbox { party, at } => Chunk::Inbox { party, at, content: copy_of(content) },
    }
}

/// One more chunk missed.
fn lose(missed: u64, facts: &mut Facts) -> u64 {
    facts.lose(Loss::Chunks, 1);
    missed.saturating_add(1)
}

/// Nothing in flight, a chunk: it goes out at once, told what was missed.
fn send(token: Token, chunk: Chunk, missed: u64, facts: &mut Facts, out: &mut Queue<Request>) -> Stream {
    let chunks: Box<[Chunk]> = Box::new([chunk]);
    facts.push(Fact::Delivered { chunks: 1 });
    out.push(Request::Deliver { watcher: token, missed, chunks });
    Stream::Sending { missed: 0, flight: Flight { told: missed, chunks: 1 } }
}

/// A delivery in flight, a chunk: it waits, and if the backlog is full, what
/// waits is missed first. Returns what has been missed since the delivery
/// went.
fn wait(backlog: &mut Queue<Chunk>, head: Head, content: &[u8], missed: u64, facts: &mut Facts) -> u64 {
    let mut missed = missed;
    if backlog.room() == 0 {
        let dropped = backlog.len();
        clear(backlog);
        facts.push(Fact::Overflowed { missed: dropped });
        facts.lose(Loss::Chunks, u64::from(dropped));
        missed = missed.saturating_add(u64::from(dropped));
    }
    backlog.try_push(make(head, content)).expect("room in the backlog, made above");
    missed
}

/// A delivery ended: what has been missed since it went, and, if its stream
/// did not take it, all it held.
fn landed(missed: u64, flight: Flight, done: bool, facts: &mut Facts) -> u64 {
    if done {
        return missed;
    }
    facts.push(Fact::Undelivered { chunks: flight.chunks });
    facts.lose(Loss::Chunks, flight.chunks);
    missed.saturating_add(flight.told).saturating_add(flight.chunks)
}

/// Sending, delivered: what waits goes out, or the watcher is caught up.
fn caught_up(
    token: Token,
    backlog: &mut Queue<Chunk>,
    missed: u64,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> Stream {
    if backlog.is_empty() {
        return Stream::Idle { missed };
    }
    let flight = deliver(token, backlog, missed, facts, out);
    Stream::Sending { missed: 0, flight }
}

/// Draining, delivered: what waits goes out, or the watch is over.
fn drained(
    token: Token,
    backlog: &mut Queue<Chunk>,
    missed: u64,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> Stream {
    if backlog.is_empty() {
        return close(token, End::Finished, facts, out);
    }
    let flight = deliver(token, backlog, missed, facts, out);
    Stream::Draining { missed: 0, flight }
}

/// A delivery in flight, unwatched: what waits is dropped, and the watch
/// ends once the delivery has.
fn abandon(backlog: &mut Queue<Chunk>) -> Stream {
    clear(backlog);
    Stream::Closing
}

/// The watch ends, for `end`.
fn close(token: Token, end: End, facts: &mut Facts, out: &mut Queue<Request>) -> Stream {
    facts.push(Fact::Ended { end });
    out.push(Request::Ended { watcher: token, end });
    Stream::Closed
}

/// Sends the whole backlog in one delivery, told what was missed before it.
fn deliver(
    token: Token,
    backlog: &mut Queue<Chunk>,
    missed: u64,
    facts: &mut Facts,
    out: &mut Queue<Request>,
) -> Flight {
    let count = backlog.len();
    let mut chunks = List::with_capacity(count);
    for _ in 0..count {
        if let Some(chunk) = backlog.pop() {
            chunks.push(chunk).expect("room for the backlog");
        }
    }
    facts.push(Fact::Delivered { chunks: count });
    out.push(Request::Deliver { watcher: token, missed, chunks: chunks.into_boxed() });
    Flight { told: missed, chunks: u64::from(count) }
}

/// Drops what waits in `backlog`.
fn clear(backlog: &mut Queue<Chunk>) {
    for _ in 0..backlog.len() {
        if backlog.pop().is_none() {
            return;
        }
    }
}

/// What the watcher's new state implies: a closed watcher is unnamed and
/// retired, its token the parent's again.
fn conclude(domain: &mut Domain, id: Id<Watcher>) {
    let watcher = domain.watchers.get(id).expect("a watcher concluded is live");
    match watcher.state {
        Stream::Closed => {
            let token = watcher.token;
            let named = domain.names.remove(&token);
            assert!(named == Some(id), "a watcher's name is its own");
            domain.watchers.retire(id);
        }
        Stream::Idle { .. } | Stream::Sending { .. } | Stream::Draining { .. } | Stream::Closing => {}
    }
}
