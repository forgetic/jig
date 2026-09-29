//! Scripted-event tests for `steploop::http1::client`: no sockets, threads or
//! clocks.
//!
//! Most tests script every event by hand and check the exact actions and
//! client events that follow. The split-point tests and the TLS tests use a
//! driver that answers each planned action from a scripted peer instead, so
//! a whole fetch runs at every split, or through an in-memory rustls server.
//!
//! Every test's harness checks, when it is dropped, that each fetch got
//! exactly one terminal event and nothing after it.

use std::collections::{BTreeSet, VecDeque};
use std::io::ErrorKind;
use std::net::SocketAddr;

use steploop::http1::FetchId;
use steploop::http1::client::{Client, ClientCmd, ClientError, ClientEvent, Target};
use steploop::http1::codec::HeadError;
use steploop::run::IoStep;
use steploop::sys::{Action, Event, Ids, Interest, IoError, Progress, Readiness, SignalId, SockId};
use steploop::tcp::READ_CHUNK;
use steploop::time::Time;

#[cfg(feature = "tls")]
mod support;

const F: FetchId = FetchId(100);
const G: FetchId = FetchId(101);
const T0: Time = Time(1_000);
const MAX_HEAD: usize = 64 * 1024;
/// A fetch given an address connects on the first id.
const C: SockId = SockId(1);
/// A fetch that resolves takes the first id for its query, then sockets.
const Q: u64 = 1;
const S1: SockId = SockId(2);
const S2: SockId = SockId(3);

const REQ_HEAD: &[u8] =
    b"POST /v1/messages HTTP/1.1\r\nHost: up.example\r\nConnection: close\r\nContent-Length: 4\r\n\r\n";
const REQ_BODY: &[u8] = b"ping";
const RESP_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
const RESP_BODY: &[u8] = b"e\r\ndata: {\"a\":1}\n\n\r\ne\r\ndata: {\"b\":2}\n\n\r\n0\r\n\r\n";
const EOF_IN_HEAD: HeadError =
    HeadError::Malformed("the connection closed before the response head ended");

// ---------------------------------------------------------------------------
// Commands, events and actions
// ---------------------------------------------------------------------------

fn addr(n: u8) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, n], 80))
}

fn request() -> Vec<u8> {
    [REQ_HEAD, REQ_BODY].concat()
}

fn target(addr: Option<SocketAddr>) -> Target {
    Target {
        host: "up.example".into(),
        port: 80,
        tls: false,
        addr,
    }
}

fn fetch_to(fetch: FetchId, target: Target) -> ClientCmd {
    ClientCmd::Fetch {
        fetch,
        target,
        head: REQ_HEAD.to_vec(),
        body: REQ_BODY.to_vec(),
    }
}

fn ack(fetch: FetchId) -> ClientCmd {
    ClientCmd::Ack { fetch }
}

fn cancel(fetch: FetchId) -> ClientCmd {
    ClientCmd::Cancel { fetch }
}

fn err(kind: ErrorKind) -> IoError {
    IoError::from(kind)
}

fn resolved(query: u64, result: Result<Vec<SocketAddr>, IoError>) -> Event {
    Event::Resolved { query, result }
}

fn connected(sock: SockId, result: Result<Progress, IoError>) -> Event {
    Event::Connected { sock, result }
}

fn read(sock: SockId, data: &[u8]) -> Event {
    Event::Read {
        sock,
        result: Ok(data.to_vec()),
    }
}

fn read_err(sock: SockId, kind: ErrorKind) -> Event {
    Event::Read {
        sock,
        result: Err(err(kind)),
    }
}

fn wrote(sock: SockId, data: &[u8]) -> Event {
    Event::Wrote {
        sock,
        data: data.to_vec(),
        result: Ok(data.len()),
    }
}

fn ready(sock: SockId) -> Event {
    Event::Ready {
        sock,
        result: Ok(Readiness {
            readable: true,
            writable: true,
        }),
    }
}

fn closed(sock: SockId) -> Event {
    Event::Closed {
        sock,
        result: Ok(()),
    }
}

fn resolve_act(query: u64) -> Action {
    Action::Resolve {
        query,
        host: "up.example".into(),
        port: 80,
    }
}

fn connect_act(sock: SockId, addr: SocketAddr) -> Action {
    Action::Connect { sock, addr }
}

fn write_act(sock: SockId, data: &[u8]) -> Action {
    Action::Write {
        sock,
        data: data.to_vec(),
    }
}

fn read_act(sock: SockId, max: usize) -> Action {
    Action::Read { sock, max }
}

fn close_act(sock: SockId) -> Action {
    Action::Close { sock }
}

fn head_event(fetch: FetchId) -> ClientEvent {
    ClientEvent::Head {
        fetch,
        status: 200,
        headers: vec![
            ("Content-Type".into(), "text/event-stream".into()),
            ("Transfer-Encoding".into(), "chunked".into()),
        ],
        raw: RESP_HEAD.to_vec(),
    }
}

fn body(fetch: FetchId, bytes: &[u8]) -> ClientEvent {
    ClientEvent::Body {
        fetch,
        bytes: bytes.to_vec(),
    }
}

fn failed(fetch: FetchId, error: ClientError) -> ClientEvent {
    ClientEvent::Failed { fetch, error }
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

struct T {
    c: Client,
    started: BTreeSet<FetchId>,
    ended: BTreeSet<FetchId>,
}

impl T {
    fn new() -> T {
        T::with(Client::new(Ids::new()))
    }

    fn with(c: Client) -> T {
        T {
            c,
            started: BTreeSet::new(),
            ended: BTreeSet::new(),
        }
    }

    /// Reap events the client must own all of.
    fn reap(&mut self, events: impl IntoIterator<Item = Event>) -> Vec<ClientEvent> {
        let (out, rest) = self.reap_some(events);
        assert_eq!(rest, [], "the client owns these");
        out
    }

    /// Reap, returning the events the client handed back.
    fn reap_some(
        &mut self,
        events: impl IntoIterator<Item = Event>,
    ) -> (Vec<ClientEvent>, Vec<Event>) {
        let mut events: Vec<Event> = events.into_iter().collect();
        let mut out = Vec::new();
        self.c.reap(T0, &mut events, &mut out);
        self.check(&out);
        (out, events)
    }

    fn step(
        &mut self,
        cmds: impl IntoIterator<Item = ClientCmd>,
    ) -> (Vec<ClientEvent>, Vec<Action>) {
        let mut cmds: Vec<ClientCmd> = cmds.into_iter().collect();
        for cmd in &cmds {
            if let ClientCmd::Fetch { fetch, .. } = cmd {
                self.started.insert(*fetch);
            }
        }
        let (mut out, mut actions) = (Vec::new(), Vec::new());
        IoStep::step(&mut self.c, T0, &mut cmds, &mut out, &mut actions);
        assert!(cmds.is_empty(), "step drains its commands");
        self.check(&out);
        (out, actions)
    }

    /// Step with no commands, expecting actions only.
    fn actions(&mut self) -> Vec<Action> {
        let (out, actions) = self.step([]);
        assert_eq!(out, [], "no client events expected");
        actions
    }

    /// Nothing may follow a fetch's terminal event, not even another one.
    fn check(&mut self, out: &[ClientEvent]) {
        for event in out {
            let fetch = event.fetch();
            assert!(!self.ended.contains(&fetch), "{event:?} after the end");
            if event.is_terminal() {
                self.ended.insert(fetch);
            }
        }
    }
}

impl Drop for T {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let open: Vec<_> = self.started.difference(&self.ended).collect();
            assert!(open.is_empty(), "fetches with no terminal event: {open:?}");
        }
    }
}

/// A fetch to `addr(1)` (no resolve), connected, its request written, and
/// its first read in flight.
fn connected_fetch() -> T {
    let mut t = T::new();
    let (out, actions) = t.step([fetch_to(F, target(Some(addr(1))))]);
    assert_eq!(out, []);
    assert_eq!(actions, [connect_act(C, addr(1))]);
    assert_eq!(t.reap([connected(C, Ok(Progress::Done))]), []);
    let req = request();
    assert_eq!(t.actions(), [write_act(C, &req), read_act(C, MAX_HEAD)]);
    assert_eq!(t.reap([wrote(C, &req)]), []);
    t
}

/// Deliver the head and `first` body bytes on `connected_fetch`'s socket.
fn with_body(first: &[u8]) -> T {
    let mut t = connected_fetch();
    let out = t.reap([read(C, &[RESP_HEAD, first].concat())]);
    assert_eq!(out, [head_event(F), body(F, first)]);
    t
}

/// Ack, read EOF, and close: the end of every happy fetch on `C`.
fn finish(t: &mut T) {
    let (out, actions) = t.step([ack(F)]);
    assert_eq!(out, []);
    assert_eq!(actions, [read_act(C, READ_CHUNK)]);
    assert_eq!(t.reap([read(C, b"")]), [ClientEvent::End { fetch: F }]);
    assert_eq!(t.actions(), [close_act(C)]);
    assert!(!t.c.idle(), "until the socket is closed");
    assert_eq!(t.reap([closed(C)]), []);
    assert!(t.c.idle());
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

#[test]
fn resolves_connects_and_streams_until_eof() {
    let mut t = T::new();
    let (out, actions) = t.step([fetch_to(F, target(None))]);
    assert_eq!(out, []);
    assert_eq!(actions, [resolve_act(Q)]);
    assert_eq!(t.actions(), [], "nothing until it resolves");
    assert_eq!(t.reap([resolved(Q, Ok(vec![addr(1)]))]), []);
    assert_eq!(t.actions(), [connect_act(S1, addr(1))]);
    t.reap([connected(S1, Ok(Progress::InProgress))]);
    let arm = Action::Arm {
        sock: S1,
        interest: Interest::WRITE,
    };
    assert_eq!(t.actions(), [arm]);
    t.reap([ready(S1)]);
    assert_eq!(t.actions(), [Action::FinishConnect { sock: S1 }]);
    t.reap([connected(S1, Ok(Progress::Done))]);
    let req = request();
    assert_eq!(t.actions(), [write_act(S1, &req), read_act(S1, MAX_HEAD)]);

    // The bytes after the head are the first body.
    let out = t.reap([wrote(S1, &req), read(S1, &[RESP_HEAD, b"first"].concat())]);
    assert_eq!(out, [head_event(F), body(F, b"first")]);
    assert_eq!(t.actions(), [], "no read until the Ack");
    let (out, actions) = t.step([ack(F)]);
    assert_eq!((out, actions), (vec![], vec![read_act(S1, READ_CHUNK)]));
    assert_eq!(t.reap([read(S1, b"second")]), [body(F, b"second")]);
    t.step([ack(F)]);
    assert_eq!(t.reap([read(S1, b"")]), [ClientEvent::End { fetch: F }]);
    assert_eq!(t.actions(), [close_act(S1)]);
    assert_eq!(t.reap([closed(S1)]), []);
    assert!(t.c.idle());
}

#[test]
fn a_resolve_failure_fails_the_fetch() {
    let mut t = T::new();
    t.step([fetch_to(F, target(None))]);
    let e = err(ErrorKind::Other);
    let out = t.reap([resolved(Q, Err(e))]);
    assert_eq!(out, [failed(F, ClientError::Resolve(Some(e)))]);
    assert_eq!(t.actions(), []);
    assert!(t.c.idle());

    // No addresses at all fails the same way.
    let (_, actions) = t.step([fetch_to(G, target(None))]);
    assert_eq!(actions, [resolve_act(2)]);
    assert_eq!(
        t.reap([resolved(2, Ok(vec![]))]),
        [failed(G, ClientError::Resolve(None))]
    );
    assert!(t.c.idle());
}

#[test]
fn a_refused_address_moves_on_to_the_next() {
    let mut t = T::new();
    t.step([fetch_to(F, target(None))]);
    t.reap([resolved(Q, Ok(vec![addr(1), addr(2)]))]);
    assert_eq!(t.actions(), [connect_act(S1, addr(1))]);
    // A refused `Connect` leaves no socket behind to close.
    let refused = err(ErrorKind::ConnectionRefused);
    assert_eq!(t.reap([connected(S1, Err(refused))]), []);
    assert_eq!(t.actions(), [connect_act(S2, addr(2))]);
    t.reap([connected(S2, Ok(Progress::Done))]);
    let req = request();
    assert_eq!(t.actions(), [write_act(S2, &req), read_act(S2, MAX_HEAD)]);
    t.reap([wrote(S2, &req)]);
    let out = t.reap([read(S2, RESP_HEAD)]);
    assert_eq!(out, [head_event(F)]);
    assert_eq!(t.actions(), [read_act(S2, READ_CHUNK)], "no body owed");
    assert_eq!(t.reap([read(S2, b"")]), [ClientEvent::End { fetch: F }]);
    assert_eq!(t.actions(), [close_act(S2)]);
    t.reap([closed(S2)]);
    assert!(t.c.idle());
}

#[test]
fn a_failed_finish_closes_its_socket_and_moves_on() {
    let mut t = T::new();
    t.step([fetch_to(F, target(None))]);
    t.reap([resolved(Q, Ok(vec![addr(1), addr(2)]))]);
    t.actions();
    t.reap([connected(S1, Ok(Progress::InProgress))]);
    t.actions();
    t.reap([ready(S1)]);
    assert_eq!(t.actions(), [Action::FinishConnect { sock: S1 }]);
    let refused = err(ErrorKind::ConnectionRefused);
    assert_eq!(t.reap([connected(S1, Err(refused))]), []);
    assert_eq!(
        t.actions(),
        [connect_act(S2, addr(2)), close_act(S1)],
        "the next attempt, and the failed socket closed"
    );
    // The old socket's events still reach it.
    assert!(t.c.owns(&closed(S1)));
    t.reap([closed(S1)]);
    assert!(!t.c.owns(&closed(S1)));
    t.step([cancel(F)]);
    t.reap([connected(S2, Err(refused))]);
    assert!(t.c.idle());
}

#[test]
fn every_address_failing_reports_the_last_error() {
    let mut t = T::new();
    t.step([fetch_to(F, target(None))]);
    t.reap([resolved(Q, Ok(vec![addr(1), addr(2)]))]);
    t.actions();
    t.reap([connected(S1, Err(err(ErrorKind::ConnectionRefused)))]);
    assert_eq!(t.actions(), [connect_act(S2, addr(2))]);
    let last = err(ErrorKind::HostUnreachable);
    let out = t.reap([connected(S2, Err(last))]);
    assert_eq!(out, [failed(F, ClientError::Connect(last))]);
    assert_eq!(t.actions(), []);
    assert!(t.c.idle());
}

// ---------------------------------------------------------------------------
// Credit
// ---------------------------------------------------------------------------

#[test]
fn no_read_while_a_body_is_unacknowledged() {
    let mut t = with_body(b"part");
    for _ in 0..3 {
        assert_eq!(t.actions(), [], "the body is owed");
    }
    // Unasked readiness or an Ack for another fetch is no credit.
    t.reap([ready(C)]);
    let (out, actions) = t.step([ack(G)]);
    assert_eq!((out, actions), (vec![], vec![]));

    let (_, actions) = t.step([ack(F)]);
    assert_eq!(actions, [read_act(C, READ_CHUNK)]);
    // A second Ack is not credit for a Body not yet sent.
    let (_, actions) = t.step([ack(F)]);
    assert_eq!(actions, [], "one read in flight");
    assert_eq!(t.reap([read(C, b"more")]), [body(F, b"more")]);
    assert_eq!(t.actions(), []);
    finish(&mut t);
}

#[test]
fn a_blocked_read_arms_and_resumes() {
    let mut t = connected_fetch();
    t.reap([read(C, RESP_HEAD)]);
    assert_eq!(t.actions(), [read_act(C, READ_CHUNK)]);
    t.reap([read_err(C, ErrorKind::WouldBlock)]);
    let arm = Action::Arm {
        sock: C,
        interest: Interest::READ,
    };
    assert_eq!(t.actions(), [arm]);
    t.reap([ready(C)]);
    assert_eq!(t.actions(), [read_act(C, READ_CHUNK)]);
    assert_eq!(t.reap([read(C, b"x")]), [body(F, b"x")]);
    assert_eq!(t.actions(), []);
    finish(&mut t);
}

// ---------------------------------------------------------------------------
// Splits
// ---------------------------------------------------------------------------

/// Plays the peer for one fetch to `addr(1)`: each read gets the next piece
/// (or as much of it as `max` allows), with a `WouldBlock` and a readiness
/// before each piece after the first, then EOF. Every `Body` is acked at
/// once. Returns the client's events.
fn exchange(pieces: &[&[u8]]) -> Vec<ClientEvent> {
    let mut t = T::new();
    let mut pieces: VecDeque<Vec<u8>> = pieces.iter().map(|p| p.to_vec()).collect();
    let mut cmds = vec![fetch_to(F, target(Some(addr(1))))];
    let mut events = Vec::new();
    let mut block = false;
    for _ in 0..100_000 {
        let (out, actions) = t.step(cmds.drain(..));
        let mut all = out;
        if actions.is_empty() {
            assert!(t.c.idle(), "stuck with nothing planned");
            events.extend(all);
            return events;
        }
        let replies: Vec<Event> = actions
            .into_iter()
            .map(|action| match action {
                Action::Connect { sock, .. } => connected(sock, Ok(Progress::Done)),
                Action::Write { sock, data } => wrote(sock, &data),
                Action::Read { sock, .. } if block => {
                    block = false;
                    read_err(sock, ErrorKind::WouldBlock)
                }
                Action::Read { sock, max } => match pieces.pop_front() {
                    Some(mut piece) => {
                        if piece.len() > max {
                            pieces.push_front(piece.split_off(max));
                        }
                        block = true;
                        read(sock, &piece)
                    }
                    None => read(sock, b""),
                },
                Action::Arm { sock, .. } => ready(sock),
                Action::Close { sock } => closed(sock),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        all.extend(t.reap(replies));
        cmds.extend(all.iter().filter_map(|e| match e {
            ClientEvent::Body { fetch, .. } => Some(ack(*fetch)),
            _ => None,
        }));
        events.extend(all);
    }
    panic!("the exchange never ended");
}

/// The head as one event, the body bytes in order, and `End`.
fn assert_whole(events: &[ClientEvent], why: &str) {
    let (first, rest) = events.split_first().expect("events");
    assert_eq!(*first, head_event(F), "{why}");
    let (last, bodies) = rest.split_last().expect("an end");
    assert_eq!(*last, ClientEvent::End { fetch: F }, "{why}");
    let mut got = Vec::new();
    for event in bodies {
        let ClientEvent::Body { bytes, .. } = event else {
            panic!("{why}: {event:?}");
        };
        assert!(!bytes.is_empty(), "{why}: an empty body");
        got.extend_from_slice(bytes);
    }
    assert_eq!(got, RESP_BODY, "{why}");
}

#[test]
fn a_response_split_at_every_offset() {
    let response = [RESP_HEAD, RESP_BODY].concat();
    for i in 1..response.len() {
        let events = exchange(&[&response[..i], &response[i..]]);
        assert_whole(&events, &format!("split at {i}"));
    }
}

#[test]
fn a_response_one_byte_at_a_time() {
    let response = [RESP_HEAD, RESP_BODY].concat();
    let pieces: Vec<&[u8]> = response.chunks(1).collect();
    let events = exchange(&pieces);
    assert_whole(&events, "one byte at a time");
    assert_eq!(events.len(), 2 + RESP_BODY.len(), "a body per read");
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

#[test]
fn eof_mid_head_fails_the_fetch() {
    let mut t = connected_fetch();
    let partial = b"HTTP/1.1 200 OK\r\nContent-";
    assert_eq!(t.reap([read(C, partial)]), []);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD - partial.len())]);
    let out = t.reap([read(C, b"")]);
    assert_eq!(out, [failed(F, ClientError::Protocol(EOF_IN_HEAD))]);
    assert_eq!(t.actions(), [close_act(C)]);
    t.reap([closed(C)]);
    assert!(t.c.idle());
}

#[test]
fn eof_before_any_response_fails_the_fetch() {
    let mut t = connected_fetch();
    let out = t.reap([read(C, b"")]);
    assert_eq!(out, [failed(F, ClientError::Protocol(EOF_IN_HEAD))]);
    assert_eq!(t.actions(), [close_act(C)]);
}

#[test]
fn a_malformed_head_fails_the_fetch() {
    let mut t = connected_fetch();
    let out = t.reap([read(C, b"HTTP/1.1 2x0 OK\r\n\r\n")]);
    let malformed = HeadError::Malformed("invalid status line");
    assert_eq!(out, [failed(F, ClientError::Protocol(malformed))]);
    assert_eq!(t.actions(), [close_act(C)]);
}

#[test]
fn an_oversized_head_fails_the_fetch() {
    let limits = steploop::http1::codec::Limits {
        max_head: 64,
        ..Default::default()
    };
    let mut t = T::with(Client::new(Ids::new()).with_limits(limits));
    t.step([fetch_to(F, target(Some(addr(1))))]);
    t.reap([connected(C, Ok(Progress::Done))]);
    let req = request();
    assert_eq!(t.actions(), [write_act(C, &req), read_act(C, 64)]);
    let mut big = b"HTTP/1.1 200 OK\r\nX-Pad: ".to_vec();
    big.resize(64, b'a');
    let out = t.reap([wrote(C, &req), read(C, &big)]);
    assert_eq!(
        out,
        [failed(F, ClientError::Protocol(HeadError::HeadTooLarge))]
    );
}

#[test]
fn a_reset_mid_body_fails_the_fetch() {
    let mut t = with_body(b"part");
    t.step([ack(F)]);
    let out = t.reap([read_err(C, ErrorKind::ConnectionReset)]);
    let reset = err(ErrorKind::ConnectionReset);
    assert_eq!(out, [failed(F, ClientError::Io(reset))]);
    assert_eq!(t.actions(), [close_act(C)]);
    t.reap([closed(C)]);
    assert!(t.c.idle());
}

#[test]
fn a_write_error_fails_the_fetch_once_the_peer_ends_without_a_head() {
    let mut t = T::new();
    t.step([fetch_to(F, target(Some(addr(1))))]);
    t.reap([connected(C, Ok(Progress::Done))]);
    t.actions();
    let pipe = err(ErrorKind::BrokenPipe);
    let out = t.reap([
        write_failed(C, &request()),
        read_err(C, ErrorKind::WouldBlock),
    ]);
    assert_eq!(out, [], "a response may still come");
    let arm = Action::Arm {
        sock: C,
        interest: Interest::READ,
    };
    assert_eq!(t.actions(), [arm], "reads go on, writes don't");
    t.reap([ready(C)]);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD)]);
    let partial = b"HTTP/1.1 413 Pay";
    assert_eq!(t.reap([read(C, partial)]), []);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD - partial.len())]);
    // Not `Protocol`: the write error is why no head came.
    let out = t.reap([read(C, b"")]);
    assert_eq!(out, [failed(F, ClientError::Io(pipe))]);
    assert_eq!(t.actions(), [close_act(C)]);
}

#[test]
fn a_read_error_after_a_write_error_fails_with_the_read_error() {
    let mut t = uploading();
    let out = t.reap([
        write_failed(C, &LONG_REQUEST[10..]),
        read_err(C, ErrorKind::ConnectionReset),
    ]);
    let reset = err(ErrorKind::ConnectionReset);
    assert_eq!(out, [failed(F, ClientError::Io(reset))]);
    assert_eq!(t.actions(), [close_act(C)]);
}

// ---------------------------------------------------------------------------
// Early responses: the upstream answers and closes mid-upload
// ---------------------------------------------------------------------------

const EARLY_HEAD: &[u8] =
    b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 9\r\nConnection: close\r\n\r\n";
const EARLY_BODY: &[u8] = b"too large";
/// A request long enough to be written in two parts.
const LONG_REQUEST: &[u8] = b"POST /v1/messages HTTP/1.1\r\nHost: up.example\r\nConnection: close\r\nContent-Length: 20\r\n\r\n01234567890123456789";

fn write_failed(sock: SockId, data: &[u8]) -> Event {
    Event::Wrote {
        sock,
        data: data.to_vec(),
        result: Err(err(ErrorKind::BrokenPipe)),
    }
}

fn early_response() -> Event {
    read(C, &[EARLY_HEAD, EARLY_BODY].concat())
}

fn early_head() -> ClientEvent {
    ClientEvent::Head {
        fetch: F,
        status: 413,
        headers: vec![
            ("Content-Length".into(), "9".into()),
            ("Connection".into(), "close".into()),
        ],
        raw: EARLY_HEAD.to_vec(),
    }
}

/// `LONG_REQUEST` fetched from `addr(1)`: its first write was short, and
/// the rest is in flight with the first read.
fn uploading() -> T {
    let mut t = T::new();
    let (head, body) = LONG_REQUEST.split_at(LONG_REQUEST.len() - 20);
    t.step([ClientCmd::Fetch {
        fetch: F,
        target: target(Some(addr(1))),
        head: head.to_vec(),
        body: body.to_vec(),
    }]);
    t.reap([connected(C, Ok(Progress::Done))]);
    assert_eq!(
        t.actions(),
        [write_act(C, LONG_REQUEST), read_act(C, MAX_HEAD)]
    );
    t.reap([Event::Wrote {
        sock: C,
        data: LONG_REQUEST.to_vec(),
        result: Ok(10),
    }]);
    assert_eq!(t.actions(), [write_act(C, &LONG_REQUEST[10..])]);
    t
}

/// Ack the early body, read EOF, and close.
fn finish_early(t: &mut T) {
    let (out, actions) = t.step([ack(F)]);
    assert_eq!(out, []);
    assert_eq!(
        actions,
        [read_act(C, READ_CHUNK)],
        "no write after the error"
    );
    assert_eq!(t.reap([read(C, b"")]), [ClientEvent::End { fetch: F }]);
    assert_eq!(t.actions(), [close_act(C)]);
    t.reap([closed(C)]);
    assert!(t.c.idle());
}

#[test]
fn an_early_response_after_the_write_error_is_read() {
    let mut t = uploading();
    let out = t.reap([
        write_failed(C, &LONG_REQUEST[10..]),
        read_err(C, ErrorKind::WouldBlock),
    ]);
    assert_eq!(out, []);
    let arm = Action::Arm {
        sock: C,
        interest: Interest::READ,
    };
    assert_eq!(t.actions(), [arm]);
    t.reap([ready(C)]);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD)]);
    assert_eq!(
        t.reap([early_response()]),
        [early_head(), body(F, EARLY_BODY)]
    );
    finish_early(&mut t);
}

#[test]
fn an_early_response_with_the_write_error_is_read() {
    // The write fails in the batch whose read carries the whole response.
    let mut t = uploading();
    let out = t.reap([write_failed(C, &LONG_REQUEST[10..]), early_response()]);
    assert_eq!(out, [early_head(), body(F, EARLY_BODY)]);
    finish_early(&mut t);
}

#[test]
fn an_early_response_before_the_write_error_is_read() {
    let mut t = uploading();
    assert_eq!(
        t.reap([early_response()]),
        [early_head(), body(F, EARLY_BODY)]
    );
    assert_eq!(t.actions(), [], "the upload is in flight, the body owed");
    assert_eq!(t.reap([write_failed(C, &LONG_REQUEST[10..])]), []);
    finish_early(&mut t);
}

// ---------------------------------------------------------------------------
// Cancelling
// ---------------------------------------------------------------------------

#[test]
fn cancel_mid_body_fails_and_closes() {
    let mut t = with_body(b"part");
    let (out, actions) = t.step([cancel(F)]);
    assert_eq!(out, [failed(F, ClientError::Cancelled)]);
    assert_eq!(actions, [close_act(C)]);
    // Later commands for it are ignored, and events for its socket still
    // reach the socket.
    assert_eq!(t.step([ack(F), cancel(F)]), (vec![], vec![]));
    assert!(t.c.owns(&closed(C)));
    assert_eq!(t.reap([read(C, b"late"), closed(C)]), []);
    assert!(t.c.idle());
}

#[test]
fn cancel_while_resolving() {
    let mut t = T::new();
    t.step([fetch_to(F, target(None))]);
    let (out, actions) = t.step([cancel(F)]);
    assert_eq!(out, [failed(F, ClientError::Cancelled)]);
    assert_eq!(actions, []);
    assert!(t.c.idle());
    // The late answer is nobody's here: handed back.
    let late = resolved(Q, Ok(vec![addr(1)]));
    assert_eq!(t.reap_some([late.clone()]), (vec![], vec![late]));
}

#[test]
fn cancel_while_connecting_closes_once_the_connect_returns() {
    let mut t = T::new();
    t.step([fetch_to(F, target(Some(addr(1))))]);
    let (out, actions) = t.step([cancel(F)]);
    assert_eq!(out, [failed(F, ClientError::Cancelled)]);
    assert_eq!(actions, [], "the connect is in flight");
    t.reap([connected(C, Ok(Progress::Done))]);
    assert_eq!(t.actions(), [close_act(C)]);
    t.reap([closed(C)]);
    assert!(t.c.idle());
}

// ---------------------------------------------------------------------------
// Unknown, stale and foreign
// ---------------------------------------------------------------------------

#[test]
fn foreign_events_are_handed_back_in_order_and_stale_ones_ignored() {
    let mut t = connected_fetch();
    let foreign = vec![
        read(SockId(99), b"not ours"),
        Event::Signal {
            signal: SignalId(7),
        },
        closed(SockId(98)),
        resolved(55, Ok(vec![])),
        Event::Accepted {
            listener: SockId(97),
            new: SockId(96),
            result: Err(err(ErrorKind::WouldBlock)),
        },
    ];
    for event in &foreign {
        assert!(!t.c.owns(event), "{event:?}");
    }
    let mut mixed = foreign.clone();
    mixed.insert(2, wrote(C, b"never written"));
    mixed.insert(4, ready(C));
    let (out, rest) = t.reap_some(mixed);
    assert_eq!(out, []);
    assert_eq!(rest, foreign, "only the client's events are taken");
    assert_eq!(t.actions(), [], "the read is still in flight");
    assert_eq!(
        t.reap([read(C, &[RESP_HEAD, b"x"].concat())]),
        [head_event(F), body(F, b"x")]
    );
    finish(&mut t);
}

#[test]
fn commands_for_unknown_fetches_and_reused_ids_are_ignored() {
    let mut t = T::new();
    assert_eq!(t.step([ack(G), cancel(G)]), (vec![], vec![]));
    t.step([fetch_to(F, target(Some(addr(1))))]);
    let again = fetch_to(F, target(Some(addr(2))));
    assert_eq!(t.step([again]), (vec![], vec![]), "F is in use");
    t.step([cancel(F)]);
    t.reap([connected(C, Err(err(ErrorKind::ConnectionRefused)))]);
    assert!(t.c.idle());
}

#[test]
fn several_fetches_run_side_by_side() {
    let mut t = T::new();
    let (_, actions) = t.step([
        fetch_to(F, target(Some(addr(1)))),
        fetch_to(G, target(Some(addr(2)))),
    ]);
    let (cf, cg) = (SockId(1), SockId(2));
    assert_eq!(
        actions,
        [connect_act(cf, addr(1)), connect_act(cg, addr(2))]
    );
    let refused = err(ErrorKind::ConnectionRefused);
    let out = t.reap([
        connected(cg, Ok(Progress::Done)),
        connected(cf, Err(refused)),
    ]);
    assert_eq!(out, [failed(F, ClientError::Connect(refused))]);
    let req = request();
    assert_eq!(t.actions(), [write_act(cg, &req), read_act(cg, MAX_HEAD)]);
    assert_eq!(
        t.reap([wrote(cg, &req), read(cg, RESP_HEAD)]),
        [head_event(G)]
    );
    assert_eq!(t.actions(), [read_act(cg, READ_CHUNK)]);
    assert_eq!(t.reap([read(cg, b"")]), [ClientEvent::End { fetch: G }]);
    assert_eq!(t.actions(), [close_act(cg)]);
}

#[test]
fn tls_without_a_config_fails_at_once() {
    let mut t = T::new();
    let mut tls = target(Some(addr(1)));
    tls.tls = true;
    let (out, actions) = t.step([fetch_to(F, tls)]);
    assert!(
        matches!(
            &out[..],
            [ClientEvent::Failed {
                fetch: F,
                error: ClientError::Tls(_)
            }]
        ),
        "{out:?}"
    );
    assert_eq!(actions, []);
    assert!(t.c.idle());
}

#[test]
fn errors_read_well() {
    let refused = IoError::from(&std::io::Error::from_raw_os_error(111));
    let cases = [
        (
            ClientError::Resolve(None),
            "the upstream host resolved to no address",
        ),
        (
            ClientError::Connect(refused),
            "connecting upstream failed: Connection refused (os error 111)",
        ),
        (
            ClientError::Tls("invalid peer certificate: UnknownIssuer".into()),
            "upstream TLS failed: invalid peer certificate: UnknownIssuer",
        ),
        (
            ClientError::Protocol(EOF_IN_HEAD),
            "bad upstream response: malformed HTTP head: the connection closed before the response head ended",
        ),
        (ClientError::Cancelled, "the fetch was cancelled"),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
    }
    let reset = err(ErrorKind::ConnectionReset);
    assert!(
        ClientError::Io(reset)
            .to_string()
            .starts_with("the upstream connection failed: ")
    );
    assert!(
        ClientError::Resolve(Some(reset))
            .to_string()
            .starts_with("resolving the upstream host failed: ")
    );
}

// ---------------------------------------------------------------------------
// TLS, through an in-memory rustls server
// ---------------------------------------------------------------------------

#[cfg(feature = "tls")]
mod tls {
    use std::io::{Read, Write};
    use std::mem;

    use rustls::ServerConnection;
    use steploop::tls::client_config;

    use super::support::{Pki, pki, server_config};
    use super::*;

    thread_local! {
        static PKI: Pki = pki("client planner test CA");
    }

    const LEN: usize = 200_000;

    /// A rustls server behind a scripted socket: what the client writes goes
    /// straight in, and its answer waits in `to_client` for the client's
    /// reads.
    struct Peer {
        server: ServerConnection,
        to_client: Vec<u8>,
        request: Vec<u8>,
        /// Plaintext the server has yet to encrypt.
        outgoing: Vec<u8>,
        response: Vec<u8>,
        responded: bool,
        close_notify: bool,
        /// The server has sent everything and closes (FIN) once it is read.
        fin: bool,
        error: Option<rustls::Error>,
    }

    impl Peer {
        fn receive(&mut self, mut data: &[u8]) {
            while !data.is_empty() && self.error.is_none() {
                self.server.read_tls(&mut data).expect("read_tls");
                if let Err(e) = self.server.process_new_packets() {
                    self.error = Some(e);
                }
                let mut buf = [0u8; 4096];
                while let Ok(n) = self.server.reader().read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    self.request.extend_from_slice(&buf[..n]);
                }
            }
            if !self.responded && self.request.ends_with(REQ_BODY) {
                self.responded = true;
                self.outgoing = mem::take(&mut self.response);
            }
            self.flush();
        }

        /// Encrypt everything outgoing (rustls takes a bounded amount at a
        /// time), then close if the response is out.
        fn flush(&mut self) {
            loop {
                while self.server.wants_write() {
                    self.server.write_tls(&mut self.to_client).unwrap();
                }
                if self.outgoing.is_empty() {
                    break;
                }
                let n = self.server.writer().write(&self.outgoing).unwrap();
                self.outgoing.drain(..n);
            }
            if self.responded && !self.fin {
                if self.close_notify {
                    self.server.send_close_notify();
                    self.server.write_tls(&mut self.to_client).unwrap();
                }
                self.fin = true;
            }
        }
    }

    /// Runs a TLS fetch against `peer`, acking every `Body` at once. Returns
    /// the client's events and every action it planned.
    fn run_tls(peer: &mut Peer, roots: rustls::RootCertStore) -> (Vec<ClientEvent>, Vec<Action>) {
        let config = client_config(Some(roots)).unwrap();
        let mut t = T::with(Client::new(Ids::new()).with_tls(config));
        let mut tls = target(Some(addr(1)));
        tls.host = "localhost".into();
        tls.tls = true;
        let mut cmds = vec![fetch_to(F, tls)];
        let (mut events, mut planned) = (Vec::new(), Vec::new());
        for _ in 0..100_000 {
            let (mut out, actions) = t.step(cmds.drain(..));
            if actions.is_empty() {
                assert!(t.c.idle(), "stuck with nothing planned");
                events.extend(out);
                return (events, planned);
            }
            planned.extend(actions.iter().cloned());
            let replies: Vec<Event> = actions
                .into_iter()
                .map(|action| match action {
                    Action::Connect { sock, .. } => connected(sock, Ok(Progress::Done)),
                    Action::Write { sock, data } => {
                        peer.receive(&data);
                        wrote(sock, &data)
                    }
                    Action::Read { sock, max } if !peer.to_client.is_empty() => {
                        let n = max.min(peer.to_client.len());
                        read(sock, &peer.to_client.drain(..n).collect::<Vec<_>>())
                    }
                    Action::Read { sock, .. } if peer.fin => read(sock, b""),
                    Action::Read { sock, .. } => read_err(sock, ErrorKind::WouldBlock),
                    Action::Arm { sock, interest } => {
                        let can_read = !peer.to_client.is_empty() || peer.fin;
                        assert!(interest.write || can_read, "waiting on a silent server");
                        ready(sock)
                    }
                    Action::Close { sock } => closed(sock),
                    other => panic!("unexpected {other:?}"),
                })
                .collect();
            out.extend(t.reap(replies));
            cmds.extend(out.iter().filter_map(|e| match e {
                ClientEvent::Body { fetch, .. } => Some(ack(*fetch)),
                _ => None,
            }));
            events.extend(out);
        }
        panic!("the fetch never ended");
    }

    fn peer(close_notify: bool) -> Peer {
        let body: Vec<u8> = (0..LEN).map(|i| (i % 251) as u8).collect();
        Peer {
            server: ServerConnection::new(PKI.with(server_config)).unwrap(),
            to_client: Vec::new(),
            request: Vec::new(),
            outgoing: Vec::new(),
            response: [RESP_HEAD, &body].concat(),
            responded: false,
            close_notify,
            fin: false,
            error: None,
        }
    }

    fn check_response(events: &[ClientEvent]) {
        let (first, rest) = events.split_first().expect("events");
        assert_eq!(*first, head_event(F));
        let (last, bodies) = rest.split_last().expect("an end");
        assert_eq!(*last, ClientEvent::End { fetch: F });
        let got: Vec<u8> = bodies
            .iter()
            .flat_map(|e| match e {
                ClientEvent::Body { bytes, .. } => bytes.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        let want: Vec<u8> = (0..LEN).map(|i| (i % 251) as u8).collect();
        assert!(got == want, "the body arrived whole");
        assert!(bodies.len() > 1, "in several bodies");
    }

    #[test]
    fn a_tls_fetch_ends_at_close_notify() {
        let mut peer = peer(true);
        let (events, _) = run_tls(&mut peer, PKI.with(|p| p.roots.clone()));
        assert_eq!(peer.request, request(), "the request arrived verbatim");
        assert!(peer.error.is_none());
        check_response(&events);
    }

    #[test]
    fn a_tls_fetch_ends_at_a_bare_fin() {
        let mut peer = peer(false);
        let (events, _) = run_tls(&mut peer, PKI.with(|p| p.roots.clone()));
        check_response(&events);
    }

    #[test]
    fn an_unknown_ca_fails_and_tells_the_server() {
        let stranger = pki("a stranger CA");
        let mut peer = peer(true);
        let (events, planned) = run_tls(&mut peer, stranger.roots);
        let [
            ClientEvent::Failed {
                error: ClientError::Tls(why),
                ..
            },
        ] = &events[..]
        else {
            panic!("{events:?}");
        };
        assert!(why.contains("UnknownIssuer"), "{why}");
        assert!(
            matches!(peer.error, Some(rustls::Error::AlertReceived(_))),
            "the alert reached the server: {:?}",
            peer.error
        );
        assert!(matches!(planned.last(), Some(Action::Close { .. })));
    }
}
