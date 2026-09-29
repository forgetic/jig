//! The HTTP/1.1 server planner: a listener and its connections, one request
//! each, turned into [`ServerEvent`]s for a core and driven by its
//! [`ServerCmd`]s. See `docs/explanation/sans-io-shell.md` §4.12 and §5.5.
//!
//! Each connection is a [`tcp::Conn`](crate::tcp::Conn) and a `State` enum
//! whose variants hold that state's data, so every transition is one `match`
//! arm that moves owned data from one state to the next. After each batch of
//! events and each batch of commands the planner sweeps all connections
//! through that `match`. There is no bookkeeping of which ones changed: at a
//! cap of a few hundred connections, the sweep costs less than that
//! bookkeeping would take to get right.
//!
//! The vocabulary is generic, with no jig types: a core names an exchange by
//! its [`ReqId`] (the connection's [`SockId`] value), answers with a
//! structured [`Response`] or relays raw bytes, and every connection closes
//! after its one response (`Connection: close`). Bad requests never reach the
//! core: the planner answers them itself with the codec's status.

use std::collections::BTreeMap;
use std::mem;
use std::time::Duration;

use super::codec::{self, HeadError, Limits, RequestHead};
use super::{Body, ReqId, Request, Response};
use crate::run::IoStep;
use crate::sys::{Action, Event, Ids, SignalId, SockId};
use crate::tcp::{Conn, Listener};
use crate::time::Time;

/// Server settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub limits: Limits,
    /// Open connections at most; the kernel's backlog holds the rest.
    pub max_conns: usize,
}

impl Default for Config {
    /// The codec's default limits and 256 connections.
    fn default() -> Self {
        Config {
            limits: Limits::default(),
            max_conns: 256,
        }
    }
}

/// What the server tells its core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerEvent {
    /// A complete request (or one whose body the client cut short with EOF).
    Request { req: ReqId, request: Request },
    /// The exchange ended before its response was completely written: the
    /// client disconnected or failed, or shutdown's grace ran out. Commands
    /// for `req` are ignored from now on.
    Gone { req: ReqId },
    /// Every raw byte queued for `req` so far has been written.
    Flushed { req: ReqId },
    /// A reactor signal, passed through.
    Signal { id: SignalId },
}

/// What the core tells the server. Commands for unknown or gone requests,
/// and commands out of order, are ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerCmd {
    /// Write `response` (with [`codec::write_response`]), then close.
    Respond { req: ReqId, response: Response },
    /// Start a raw response: bytes relayed verbatim, head included.
    RawStart { req: ReqId },
    /// More raw bytes. [`ServerEvent::Flushed`] follows once they are written.
    RawBytes { req: ReqId, bytes: Vec<u8> },
    /// The raw response is complete: close once it is written.
    RawEnd { req: ReqId },
    /// Close the listener and the connections with no request yet; give
    /// responses `grace` to finish, then close whatever is left.
    Shutdown { grace: Duration },
}

/// The HTTP/1.1 server planner. See the module docs.
#[derive(Debug)]
pub struct Server {
    ids: Ids,
    listener: Listener,
    conns: BTreeMap<SockId, Exchange>,
    limits: Limits,
    stop: Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Running,
    /// Shutting down: responses still being written are cut off at this time.
    Grace(Time),
    /// Shut down, and the grace period is over.
    Stopped,
}

/// One connection and where its exchange stands.
#[derive(Debug)]
struct Exchange {
    tcp: Conn,
    state: State,
}

#[derive(Debug)]
enum State {
    /// Reading the head. `inbound[..scanned]` has no newline the parser
    /// hasn't seen, so it only runs again once one arrives (see the codec's
    /// note on reparsing).
    Head {
        scanned: usize,
    },
    /// Reading the `Content-Length` body `head` declared.
    Body {
        head: RequestHead,
    },
    /// The core has the request; its response hasn't started.
    Waiting,
    /// Writing the response: a structured one (`ended` from the start) or a
    /// raw relay (`ended` at `RawEnd`). `flush_owed`: raw bytes were queued
    /// since the last `Flushed`.
    Sending {
        ended: bool,
        flush_owed: bool,
    },
    /// Writing an error response the core never hears of.
    Refusing,
    Closing,
}

impl State {
    /// The core holds this request and hasn't seen its response through, so
    /// it is owed a `Gone` if the exchange ends here.
    fn owed(&self) -> bool {
        matches!(self, State::Waiting | State::Sending { .. })
    }
}

impl Server {
    /// A server whose listener takes the first id from `ids`; the embedder
    /// adopts the bound listener under [`Server::listener`] before running.
    pub fn new(mut ids: Ids, config: Config) -> Server {
        let listener = Listener::new(ids.next_sock(), config.max_conns);
        Server {
            ids,
            listener,
            conns: BTreeMap::new(),
            limits: config.limits,
            stop: Stop::Running,
        }
    }

    /// The listener's id, for `Reactor::adopt_listener`.
    pub fn listener(&self) -> SockId {
        self.listener.sock()
    }

    /// Take the reactor's events. Events for sockets the server doesn't know
    /// are dropped.
    pub fn reap(&mut self, now: Time, events: &mut Vec<Event>, out: &mut Vec<ServerEvent>) {
        for event in events.drain(..) {
            if let Event::Signal { signal } = event {
                out.push(ServerEvent::Signal { id: signal });
                continue;
            }
            let Some(sock) = event.sock() else {
                continue;
            };
            if sock == self.listener.sock() {
                if let Some(new) = self.listener.on_event(now, event) {
                    let running = self.stop == Stop::Running;
                    self.conns.insert(new, Exchange::new(new, running));
                }
            } else if let Some(ex) = self.conns.get_mut(&sock) {
                ex.tcp.on_event(event);
            }
        }
        self.sweep(out);
    }

    /// Apply one command from the core.
    pub fn command(&mut self, now: Time, cmd: ServerCmd) {
        let (req, cmd) = match cmd {
            ServerCmd::Shutdown { grace } => return self.shutdown(now, grace),
            ServerCmd::Respond { req, .. }
            | ServerCmd::RawStart { req }
            | ServerCmd::RawBytes { req, .. }
            | ServerCmd::RawEnd { req } => (req, cmd),
        };
        if let Some(ex) = self.conns.get_mut(&SockId(req.0)) {
            ex.command(cmd);
        }
    }

    /// Expire the grace period, move every exchange along and plan the
    /// actions they need.
    pub fn plan(&mut self, now: Time, out: &mut Vec<ServerEvent>, actions: &mut Vec<Action>) {
        if matches!(self.stop, Stop::Grace(until) if until <= now) {
            self.stop = Stop::Stopped;
            for (&sock, ex) in &mut self.conns {
                ex.abandon(ReqId(sock.0), out);
            }
        }
        self.sweep(out);
        for ex in self.conns.values_mut() {
            ex.tcp.plan(actions);
        }
        let open = self.conns.len();
        self.listener.plan(now, open, &mut self.ids, actions);
    }

    /// The end of the grace period or of an accept backoff, whichever is
    /// first.
    pub fn deadline(&self) -> Option<Time> {
        let grace = match self.stop {
            Stop::Grace(until) => Some(until),
            Stop::Running | Stop::Stopped => None,
        };
        match (grace, self.listener.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The listener and every connection are closed, and nothing is in
    /// flight (a connection is forgotten only once its `Closed` arrives).
    pub fn idle(&self) -> bool {
        self.listener.is_closed() && self.conns.is_empty()
    }

    fn shutdown(&mut self, now: Time, grace: Duration) {
        if self.stop != Stop::Running {
            return;
        }
        self.stop = Stop::Grace(now.after(grace));
        self.listener.close();
        for ex in self.conns.values_mut() {
            if !ex.state.owed() {
                ex.close();
            }
        }
    }

    /// Move every exchange along, and forget the closed ones.
    fn sweep(&mut self, out: &mut Vec<ServerEvent>) {
        for (&sock, ex) in &mut self.conns {
            ex.advance(ReqId(sock.0), &self.limits, out);
        }
        self.conns.retain(|_, ex| !ex.tcp.is_closed());
    }
}

impl IoStep for Server {
    type Comp = ServerEvent;
    type Req = ServerCmd;

    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<ServerEvent>) {
        Server::reap(self, now, events, comps);
    }

    fn step(
        &mut self,
        now: Time,
        reqs: &mut Vec<ServerCmd>,
        comps: &mut Vec<ServerEvent>,
        actions: &mut Vec<Action>,
    ) {
        for cmd in reqs.drain(..) {
            self.command(now, cmd);
        }
        self.plan(now, comps, actions);
    }

    fn deadline(&self) -> Option<Time> {
        Server::deadline(self)
    }

    fn idle(&self) -> bool {
        Server::idle(self)
    }
}

impl Exchange {
    /// A freshly accepted connection; closed at once if the server is
    /// shutting down.
    fn new(sock: SockId, running: bool) -> Exchange {
        let mut ex = Exchange {
            tcp: Conn::new(sock),
            state: State::Head { scanned: 0 },
        };
        if !running {
            ex.close();
        }
        ex
    }

    fn command(&mut self, cmd: ServerCmd) {
        match (&mut self.state, cmd) {
            (State::Waiting, ServerCmd::Respond { response, .. }) => {
                self.tcp.send(codec::write_response(&response));
                self.state = State::Sending {
                    ended: true,
                    flush_owed: false,
                };
            }
            (State::Waiting, ServerCmd::RawStart { .. }) => {
                self.state = State::Sending {
                    ended: false,
                    flush_owed: false,
                };
            }
            (
                State::Sending {
                    ended: false,
                    flush_owed,
                },
                ServerCmd::RawBytes { bytes, .. },
            ) => {
                *flush_owed = true;
                self.tcp.send(bytes);
            }
            (State::Sending { ended, .. }, ServerCmd::RawEnd { .. }) => *ended = true,
            _ => {}
        }
    }

    /// Move the exchange as far as its socket allows.
    fn advance(&mut self, req: ReqId, limits: &Limits, out: &mut Vec<ServerEvent>) {
        if self.tcp.error().is_some() {
            return self.abandon(req, out);
        }
        let state = mem::replace(&mut self.state, State::Closing);
        self.state = self.next(state, req, limits, out);
    }

    /// The state after `state`, given the socket.
    fn next(
        &mut self,
        state: State,
        req: ReqId,
        limits: &Limits,
        out: &mut Vec<ServerEvent>,
    ) -> State {
        let tcp = &mut self.tcp;
        match state {
            State::Head { mut scanned } => match read_head(&tcp.inbound, &mut scanned, limits) {
                Ok(Some(head)) => self.next(State::Body { head }, req, limits, out),
                // Nothing to answer: the client never sent a request.
                Ok(None) if tcp.eof() => {
                    tcp.close();
                    State::Closing
                }
                Ok(None) => {
                    tcp.set_read_limit(limits.max_head);
                    State::Head { scanned }
                }
                Err(e) => {
                    tcp.set_read_limit(0);
                    tcp.send(codec::write_response(&refusal(e)));
                    State::Refusing
                }
            },
            State::Body { head } => {
                let end = head.head_len.saturating_add(head.body_len());
                // EOF mid-body serves what arrived, as the async server did.
                if tcp.inbound.len() < end && !tcp.eof() {
                    tcp.set_read_limit(end);
                    return State::Body { head };
                }
                tcp.set_read_limit(0);
                let mut body = tcp.inbound.split_off(head.head_len.min(tcp.inbound.len()));
                // Bytes after the body are ignored: one request per connection.
                body.truncate(head.body_len());
                tcp.inbound.clear();
                let request = head.into_request(body);
                out.push(ServerEvent::Request { req, request });
                State::Waiting
            }
            State::Sending { ended, flush_owed } if tcp.unsent() == 0 => {
                if flush_owed {
                    out.push(ServerEvent::Flushed { req });
                }
                if ended {
                    tcp.close();
                    State::Closing
                } else {
                    State::Sending {
                        ended,
                        flush_owed: false,
                    }
                }
            }
            State::Refusing if tcp.unsent() == 0 => {
                tcp.close();
                State::Closing
            }
            other => other,
        }
    }

    /// End the exchange now, telling the core if it holds the request.
    fn abandon(&mut self, req: ReqId, out: &mut Vec<ServerEvent>) {
        if self.state.owed() {
            out.push(ServerEvent::Gone { req });
        }
        self.close();
    }

    fn close(&mut self) {
        self.state = State::Closing;
        self.tcp.close();
    }
}

/// Parse the head if the bytes since the last look could have completed it:
/// a head ends at a newline, or fails at the size limit.
fn read_head(
    inbound: &[u8],
    scanned: &mut usize,
    limits: &Limits,
) -> Result<Option<RequestHead>, HeadError> {
    let fresh = inbound.get(*scanned..).unwrap_or_default();
    let worth_parsing = fresh.contains(&b'\n') || inbound.len() >= limits.max_head;
    *scanned = inbound.len();
    if worth_parsing {
        codec::parse_request_head(inbound, limits)
    } else {
        Ok(None)
    }
}

/// The response to a request the planner refuses: the codec's status, with
/// the reason phrase as a plain-text body.
fn refusal(e: HeadError) -> Response {
    let (status, reason) = e.status();
    Response {
        status,
        reason: reason.to_owned(),
        headers: vec![
            ("Content-Type".to_owned(), "text/plain".to_owned()),
            ("Content-Length".to_owned(), reason.len().to_string()),
            ("Connection".to_owned(), "close".to_owned()),
        ],
        body: Body::Full(reason.as_bytes().to_vec()),
    }
}
