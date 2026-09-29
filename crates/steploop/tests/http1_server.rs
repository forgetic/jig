//! Scripted-event tests for `steploop::http1::server`: no sockets, threads or
//! clocks.
//!
//! Most tests script every event by hand and check the exact actions and
//! server events that follow. The split-point tests instead use `Wire`, a
//! scripted client that answers each planned action (one piece per read, with
//! a `WouldBlock` and a readiness between pieces; short writes, each followed
//! by a `WouldBlock`), so a whole exchange can run at every split.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::mem;
use std::net::SocketAddr;
use std::time::Duration;

use steploop::http1::codec::{Limits, write_response};
use steploop::http1::server::{Config, Server, ServerCmd, ServerEvent};
use steploop::http1::{Body, ReqId, Request, Response};
use steploop::run::IoStep;
use steploop::sys::{Action, Event, Ids, Interest, IoError, Readiness, SignalId, SockId};
use steploop::tcp::ACCEPT_BACKOFF;
use steploop::time::Time;

const L: SockId = SockId(1);
/// The first connection, and its exchange.
const C: SockId = SockId(2);
const REQ: ReqId = ReqId(2);
const T0: Time = Time(1_000_000);
const GRACE: Duration = Duration::from_secs(1);
const MAX_HEAD: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Events and actions
// ---------------------------------------------------------------------------

fn peer() -> SocketAddr {
    "127.0.0.1:40000".parse().unwrap()
}

fn wb() -> IoError {
    IoError::from(ErrorKind::WouldBlock)
}

fn accepted(new: u64, result: Result<SocketAddr, IoError>) -> Event {
    Event::Accepted {
        listener: L,
        new: SockId(new),
        result,
    }
}

fn read(sock: SockId, data: &[u8]) -> Event {
    Event::Read {
        sock,
        result: Ok(data.to_vec()),
    }
}

fn read_err(sock: SockId, e: IoError) -> Event {
    Event::Read {
        sock,
        result: Err(e),
    }
}

fn wrote(sock: SockId, data: &[u8], result: Result<usize, IoError>) -> Event {
    Event::Wrote {
        sock,
        data: data.to_vec(),
        result,
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

fn accept(new: u64) -> Action {
    Action::Accept {
        listener: L,
        new: SockId(new),
    }
}

fn read_act(sock: SockId, max: usize) -> Action {
    Action::Read { sock, max }
}

fn write_act(sock: SockId, data: &[u8]) -> Action {
    Action::Write {
        sock,
        data: data.to_vec(),
    }
}

fn arm(sock: SockId, interest: Interest) -> Action {
    Action::Arm { sock, interest }
}

fn close(sock: SockId) -> Action {
    Action::Close { sock }
}

fn get(target: &str) -> Request {
    Request {
        method: "GET".into(),
        target: target.into(),
        headers: vec![("Host".into(), "h".into())],
        body: Vec::new(),
    }
}

const GET: &[u8] = b"GET /a?b HTTP/1.1\r\nHost: h\r\n\r\n";

fn ok(body: &[u8]) -> Response {
    Response {
        status: 200,
        reason: "OK".into(),
        headers: vec![("Content-Length".into(), body.len().to_string())],
        body: Body::Full(body.to_vec()),
    }
}

fn respond(req: ReqId, body: &[u8]) -> ServerCmd {
    ServerCmd::Respond {
        req,
        response: ok(body),
    }
}

fn raw_bytes(bytes: &[u8]) -> ServerCmd {
    ServerCmd::RawBytes {
        req: REQ,
        bytes: bytes.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// The harness: a server, the time, and the steps
// ---------------------------------------------------------------------------

struct T {
    s: Server,
    now: Time,
}

impl T {
    fn new(config: Config) -> T {
        let s = Server::new(Ids::new(), config);
        assert_eq!(s.listener(), L);
        T { s, now: T0 }
    }

    /// A server with one connection (`C`) whose first read is in flight, and
    /// the listener armed.
    fn with_conn(config: Config) -> T {
        let mut t = T::new(config);
        assert_eq!(t.actions(), [accept(2)]);
        assert_eq!(t.reap([accepted(2, Ok(peer()))]), []);
        let max = config.limits.max_head.min(steploop::tcp::READ_CHUNK);
        assert_eq!(t.actions(), [read_act(C, max), accept(3)]);
        t.reap([accepted(3, Err(wb()))]);
        assert_eq!(t.actions(), [arm(L, Interest::READ)]);
        t
    }

    /// `with_conn`, and `GET` delivered as `REQ`.
    fn with_request() -> T {
        let mut t = T::with_conn(Config::default());
        let request = get("/a?b");
        assert_eq!(
            t.reap([read(C, GET)]),
            [ServerEvent::Request { req: REQ, request }]
        );
        assert_eq!(t.actions(), [], "no reads while the core decides");
        t
    }

    fn reap(&mut self, events: impl IntoIterator<Item = Event>) -> Vec<ServerEvent> {
        let mut events: Vec<Event> = events.into_iter().collect();
        let mut out = Vec::new();
        IoStep::reap(&mut self.s, self.now, &mut events, &mut out);
        assert!(events.is_empty(), "reap drains its events");
        out
    }

    fn step(
        &mut self,
        cmds: impl IntoIterator<Item = ServerCmd>,
    ) -> (Vec<ServerEvent>, Vec<Action>) {
        let mut cmds: Vec<ServerCmd> = cmds.into_iter().collect();
        let (mut out, mut actions) = (Vec::new(), Vec::new());
        IoStep::step(&mut self.s, self.now, &mut cmds, &mut out, &mut actions);
        assert!(cmds.is_empty(), "step drains its commands");
        (out, actions)
    }

    /// Step with no commands, expecting actions only.
    fn actions(&mut self) -> Vec<Action> {
        let (out, actions) = self.step([]);
        assert_eq!(out, [], "no server events expected");
        actions
    }
}

// ---------------------------------------------------------------------------
// Requests and responses
// ---------------------------------------------------------------------------

#[test]
fn serves_one_request_and_closes() {
    let mut t = T::with_request();
    let response = ok(b"hello");
    let bytes = write_response(&response);
    let (out, actions) = t.step([ServerCmd::Respond { req: REQ, response }]);
    assert_eq!(out, []);
    assert_eq!(actions, [write_act(C, &bytes)]);
    assert_eq!(t.reap([wrote(C, &bytes, Ok(bytes.len()))]), []);
    assert_eq!(t.actions(), [close(C)]);
    assert!(!t.s.idle());
    assert_eq!(t.reap([closed(C)]), []);
    assert_eq!(t.actions(), []);
}

#[test]
fn a_body_is_read_to_its_content_length_and_no_further() {
    let mut t = T::with_conn(Config::default());
    let head = b"POST /p HTTP/1.1\r\nContent-Length: 10\r\n\r\n";
    assert_eq!(t.reap([read(C, head)]), []);
    assert_eq!(t.actions(), [read_act(C, 10)], "exactly the body");
    assert_eq!(t.reap([read(C, b"0123")]), []);
    assert_eq!(t.actions(), [read_act(C, 6)]);
    let out = t.reap([read(C, b"456789")]);
    let [ServerEvent::Request { req, request }] = &out[..] else {
        panic!("{out:?}");
    };
    assert_eq!(*req, REQ);
    assert_eq!(request.body, b"0123456789");
    assert_eq!(request.header("content-length"), Some("10"));
}

#[test]
fn bytes_after_the_body_are_ignored() {
    let mut t = T::with_conn(Config::default());
    let out = t.reap([read(
        C,
        b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\nhiGET / HTTP/1.1\r\n\r\n",
    )]);
    let [ServerEvent::Request { request, .. }] = &out[..] else {
        panic!("{out:?}");
    };
    assert_eq!(request.body, b"hi");
    assert_eq!(t.actions(), []);
}

#[test]
fn eof_mid_head_closes_without_a_response() {
    let mut t = T::with_conn(Config::default());
    assert_eq!(t.reap([read(C, b"GET / HT")]), []);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD - 8)]);
    assert_eq!(t.reap([read(C, b"")]), []);
    assert_eq!(t.actions(), [close(C)]);
}

#[test]
fn eof_mid_body_serves_the_partial_body() {
    let mut t = T::with_conn(Config::default());
    t.reap([read(C, b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\npart")]);
    assert_eq!(t.actions(), [read_act(C, 5)]);
    let out = t.reap([read(C, b"")]);
    let [ServerEvent::Request { request, .. }] = &out[..] else {
        panic!("{out:?}");
    };
    assert_eq!(request.body, b"part");
    // The half-closed client still gets its response.
    let (_, actions) = t.step([respond(REQ, b"ok")]);
    assert_eq!(actions, [write_act(C, &write_response(&ok(b"ok")))]);
}

#[test]
fn a_reset_before_the_request_closes_silently() {
    let mut t = T::with_conn(Config::default());
    t.reap([read(C, b"GET")]);
    t.actions();
    let reset = IoError::from(ErrorKind::ConnectionReset);
    assert_eq!(t.reap([read_err(C, reset)]), [], "the core never knew");
    assert_eq!(t.actions(), [close(C)]);
}

#[test]
fn would_block_arms_once_and_readiness_resumes_reading() {
    let mut t = T::with_conn(Config::default());
    t.reap([read(C, b"GET / HTTP/1.1\r\n")]);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD - 16)]);
    t.reap([read_err(C, wb())]);
    assert_eq!(t.actions(), [arm(C, Interest::READ)]);
    for _ in 0..3 {
        assert_eq!(t.actions(), [], "exactly one arm per socket");
    }
    t.reap([ready(C)]);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD - 16)]);
    let out = t.reap([read(C, b"\r\n")]);
    assert!(matches!(&out[..], [ServerEvent::Request { .. }]), "{out:?}");
}

#[test]
fn short_writes_and_blocked_writes_finish_the_response() {
    let mut t = T::with_request();
    let bytes = write_response(&ok(b"0123456789"));
    let (_, actions) = t.step([respond(REQ, b"0123456789")]);
    assert_eq!(actions, [write_act(C, &bytes)]);
    t.reap([wrote(C, &bytes, Ok(10))]);
    assert_eq!(t.actions(), [write_act(C, &bytes[10..])]);
    t.reap([wrote(C, &bytes[10..], Err(wb()))]);
    assert_eq!(t.actions(), [arm(C, Interest::WRITE)]);
    assert_eq!(t.actions(), []);
    t.reap([ready(C)]);
    assert_eq!(t.actions(), [write_act(C, &bytes[10..])]);
    t.reap([wrote(C, &bytes[10..], Ok(bytes.len() - 10))]);
    assert_eq!(t.actions(), [close(C)]);
}

#[test]
fn a_reset_mid_write_is_gone() {
    let mut t = T::with_request();
    let bytes = write_response(&ok(b"body"));
    t.step([respond(REQ, b"body")]);
    let reset = IoError::from(ErrorKind::ConnectionReset);
    assert_eq!(
        t.reap([wrote(C, &bytes, Err(reset))]),
        [ServerEvent::Gone { req: REQ }]
    );
    assert_eq!(t.actions(), [close(C)]);
    // Gone once, and later commands are ignored.
    let (out, actions) = t.step([respond(REQ, b"again"), ServerCmd::RawEnd { req: REQ }]);
    assert_eq!((out, actions), (vec![], vec![]));
    assert_eq!(t.reap([closed(C)]), []);
}

#[test]
fn commands_for_unknown_or_undelivered_requests_are_ignored() {
    let mut t = T::with_conn(Config::default());
    // C exists but has delivered nothing; 99 never existed.
    let cmds = [
        respond(REQ, b"early"),
        ServerCmd::RawStart { req: ReqId(99) },
        respond(ReqId(99), b"x"),
    ];
    assert_eq!(t.step(cmds), (vec![], vec![]));
    let out = t.reap([read(C, GET)]);
    assert!(matches!(&out[..], [ServerEvent::Request { .. }]));
}

#[test]
fn out_of_order_commands_are_ignored() {
    let mut t = T::with_request();
    // Raw bytes need a RawStart; a structured response excludes a raw one.
    assert_eq!(t.step([raw_bytes(b"x")]), (vec![], vec![]));
    let bytes = write_response(&ok(b"s"));
    let (_, actions) = t.step([
        respond(REQ, b"s"),
        ServerCmd::RawStart { req: REQ },
        raw_bytes(b"x"),
    ]);
    assert_eq!(actions, [write_act(C, &bytes)]);
}

#[test]
fn stale_and_foreign_events_are_ignored() {
    let mut t = T::with_conn(Config::default());
    let stale = [
        read(SockId(99), b"GET / HTTP/1.1\r\n\r\n"),
        closed(SockId(99)),
        wrote(C, b"never written", Ok(5)),
        ready(C),
        accepted(7, Ok(peer())),
        Event::Resolved {
            query: 1,
            result: Ok(vec![]),
        },
    ];
    assert_eq!(t.reap(stale), []);
    assert_eq!(t.actions(), [], "C's read is still in flight");
    let out = t.reap([read(C, GET)]);
    assert!(matches!(&out[..], [ServerEvent::Request { .. }]));
}

#[test]
fn signals_pass_through() {
    let mut t = T::new(Config::default());
    let signal = Event::Signal {
        signal: SignalId(77),
    };
    assert_eq!(t.reap([signal]), [ServerEvent::Signal { id: SignalId(77) }]);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

fn refusal_bytes(status: u16, reason: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}",
        reason.len()
    )
    .into_bytes()
}

#[test]
fn a_refusal_is_written_then_closed_and_the_core_never_sees_it() {
    let mut t = T::with_conn(Config::default());
    let bytes = refusal_bytes(400, "Bad Request");
    assert_eq!(t.reap([read(C, b"GET / HTTP/1.1\r\nno colon\r\n\r\n")]), []);
    assert_eq!(t.actions(), [write_act(C, &bytes)], "and no more reads");
    t.reap([wrote(C, &bytes, Ok(bytes.len()))]);
    assert_eq!(t.actions(), [close(C)]);
}

#[test]
fn an_oversized_head_is_refused_at_the_limit() {
    let limits = Limits {
        max_head: 32,
        ..Limits::default()
    };
    let config = Config {
        limits,
        ..Config::default()
    };
    let mut t = T::with_conn(config);
    t.reap([read(C, b"GET /0123456789 HTTP/1.1\r\nX: ")]);
    assert_eq!(t.actions(), [read_act(C, 3)], "never past the limit");
    t.reap([read(C, b"abc")]);
    assert_eq!(
        t.actions(),
        [write_act(C, &refusal_bytes(413, "Content Too Large"))]
    );
}

/// Each refused request, at every split point: the refusal, and nothing for
/// the core.
#[test]
fn refusals_at_every_split() {
    let cases: [(&[u8], u16, &str); 5] = [
        (b"GET / HTTP/1.1\r\nno colon\r\n\r\n", 400, "Bad Request"),
        (
            b"POST / HTTP/1.1\r\nContent-Length: 1x\r\n\r\n",
            400,
            "Bad Request",
        ),
        (b"GET / HTTP/1.1\r\nX: y\n\r\n", 400, "Bad Request"),
        (
            b"POST / HTTP/1.1\r\nContent-Length: 99999999999\r\n\r\n",
            413,
            "Content Too Large",
        ),
        (
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            501,
            "Not Implemented",
        ),
    ];
    for (request, status, reason) in cases {
        for i in 0..=request.len() {
            let mut wire = Wire::new(vec![&request[..i], &request[i..]], false);
            let seen = wire.run(Config::default());
            assert_eq!(seen, [], "split at {i}");
            assert_eq!(wire.written, refusal_bytes(status, reason), "split at {i}");
            assert!(wire.closed);
        }
    }
}

// ---------------------------------------------------------------------------
// Split points, with `Wire`
// ---------------------------------------------------------------------------

/// A scripted client and reactor: see the module docs.
struct Wire {
    pieces: VecDeque<Vec<u8>>,
    eof: bool,
    /// A read owes a `WouldBlock` before the next piece.
    read_gap: bool,
    /// A write owes a `WouldBlock` after a short one.
    write_gap: bool,
    written: Vec<u8>,
    accepted: bool,
    closed: bool,
}

/// What a write takes at most.
const WRITE_MAX: usize = 7;

impl Wire {
    fn new(pieces: Vec<&[u8]>, eof: bool) -> Wire {
        Wire {
            pieces: pieces
                .into_iter()
                .filter(|p| !p.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
            eof,
            read_gap: false,
            write_gap: false,
            written: Vec::new(),
            accepted: false,
            closed: false,
        }
    }

    /// Run a server against this client until nothing more can happen,
    /// answering each request with `answer`. Returns the server events.
    fn run(&mut self, config: Config) -> Vec<ServerEvent> {
        let mut s = Server::new(Ids::new(), config);
        let (mut events, mut comps, mut cmds, mut actions) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut seen = Vec::new();
        for _ in 0..100_000 {
            IoStep::reap(&mut s, T0, &mut events, &mut comps);
            loop {
                for comp in comps.drain(..) {
                    if let ServerEvent::Request { req, request } = &comp {
                        cmds.push(ServerCmd::Respond {
                            req: *req,
                            response: answer(request),
                        });
                    }
                    seen.push(comp);
                }
                IoStep::step(&mut s, T0, &mut cmds, &mut comps, &mut actions);
                if comps.is_empty() {
                    break;
                }
            }
            events.extend(actions.drain(..).filter_map(|a| self.answer(a)));
            if events.is_empty() {
                return seen;
            }
        }
        panic!("the exchange never settled");
    }

    fn answer(&mut self, action: Action) -> Option<Event> {
        Some(match action {
            Action::Accept { listener, new } => {
                let result = if mem::replace(&mut self.accepted, true) {
                    Err(wb())
                } else {
                    Ok(peer())
                };
                Event::Accepted {
                    listener,
                    new,
                    result,
                }
            }
            Action::Arm { sock, .. } if sock == L => return None,
            Action::Arm { sock, interest } => {
                let readable = interest.read && (!self.pieces.is_empty() || self.eof);
                if !readable && !interest.write {
                    return None; // the client has nothing more to send
                }
                Event::Ready {
                    sock,
                    result: Ok(Readiness {
                        readable,
                        writable: interest.write,
                    }),
                }
            }
            Action::Read { sock, max } => {
                let result = if mem::take(&mut self.read_gap) {
                    Err(wb())
                } else if let Some(mut piece) = self.pieces.pop_front() {
                    if piece.len() > max {
                        self.pieces.push_front(piece.split_off(max));
                    }
                    self.read_gap = true;
                    Ok(piece)
                } else if self.eof {
                    Ok(Vec::new())
                } else {
                    Err(wb())
                };
                Event::Read { sock, result }
            }
            Action::Write { sock, data } => {
                let result = if mem::take(&mut self.write_gap) {
                    Err(wb())
                } else {
                    let n = data.len().min(WRITE_MAX);
                    self.written.extend_from_slice(&data[..n]);
                    self.write_gap = n < data.len();
                    Ok(n)
                };
                Event::Wrote { sock, data, result }
            }
            Action::Close { sock } => {
                self.closed |= sock == C;
                Event::Closed {
                    sock,
                    result: Ok(()),
                }
            }
            other => panic!("unexpected action {other:?}"),
        })
    }
}

/// The core's answer in the `Wire` tests: the body echoed back, framed.
fn answer(request: &Request) -> Response {
    let mut body = format!("{} {} ", request.method, request.target).into_bytes();
    body.extend_from_slice(&request.body);
    ok(&body)
}

const POST: &[u8] =
    b"POST /v1/messages?beta=true HTTP/1.1\r\nHost: api\r\nContent-Length: 11\r\n\r\nhello world";
const POST_HEAD_LEN: usize = POST.len() - 11;

fn post(body: &[u8]) -> Request {
    Request {
        method: "POST".into(),
        target: "/v1/messages?beta=true".into(),
        headers: vec![
            ("Host".into(), "api".into()),
            ("Content-Length".into(), "11".into()),
        ],
        body: body.to_vec(),
    }
}

#[test]
fn a_request_split_at_every_pair_of_offsets() {
    let expected = post(b"hello world");
    let response = write_response(&answer(&expected));
    for i in 0..=POST.len() {
        for j in i..=POST.len() {
            let mut wire = Wire::new(vec![&POST[..i], &POST[i..j], &POST[j..]], false);
            let seen = wire.run(Config::default());
            let request = ServerEvent::Request {
                req: REQ,
                request: expected.clone(),
            };
            assert_eq!(seen, [request], "split at {i}, {j}");
            assert_eq!(wire.written, response, "split at {i}, {j}");
            assert!(wire.closed);
        }
    }
}

#[test]
fn eof_mid_head_at_every_offset() {
    for i in 0..POST_HEAD_LEN {
        let mut wire = Wire::new(vec![&POST[..i]], true);
        assert_eq!(wire.run(Config::default()), [], "EOF at {i}");
        assert_eq!(wire.written, b"", "EOF at {i}");
        assert!(wire.closed, "EOF at {i}");
    }
}

#[test]
fn eof_mid_body_at_every_offset() {
    for i in POST_HEAD_LEN..=POST.len() {
        let mut wire = Wire::new(vec![&POST[..i]], true);
        let expected = post(&POST[POST_HEAD_LEN..i]);
        let response = write_response(&answer(&expected));
        let request = ServerEvent::Request {
            req: REQ,
            request: expected,
        };
        assert_eq!(wire.run(Config::default()), [request], "EOF at {i}");
        assert_eq!(wire.written, response, "EOF at {i}");
    }
}

// ---------------------------------------------------------------------------
// The listener: cap and accept errors
// ---------------------------------------------------------------------------

#[test]
fn the_connection_cap_holds_accepts_until_one_closes() {
    let config = Config {
        max_conns: 2,
        ..Config::default()
    };
    let mut t = T::new(config);
    assert_eq!(t.actions(), [accept(2)]);
    t.reap([accepted(2, Ok(peer()))]);
    assert_eq!(t.actions(), [read_act(C, MAX_HEAD), accept(3)]);
    t.reap([accepted(3, Ok(peer())), read_err(C, wb())]);
    let d = SockId(3);
    assert_eq!(
        t.actions(),
        [arm(C, Interest::READ), read_act(d, MAX_HEAD)],
        "at the cap: no accept"
    );
    let out = t.reap([read(d, GET)]);
    assert!(matches!(&out[..], [ServerEvent::Request { .. }]));
    assert_eq!(t.actions(), [], "still at the cap");
    let bytes = write_response(&ok(b"x"));
    let (_, actions) = t.step([respond(ReqId(3), b"x")]);
    assert_eq!(actions, [write_act(d, &bytes)]);
    t.reap([wrote(d, &bytes, Ok(bytes.len()))]);
    assert_eq!(t.actions(), [close(d)], "closing still counts");
    t.reap([closed(d)]);
    assert_eq!(t.actions(), [accept(4)], "room again");
}

#[test]
fn a_failed_accept_backs_off_on_the_deadline() {
    let mut t = T::new(Config::default());
    assert_eq!(t.actions(), [accept(2)]);
    t.reap([accepted(2, Err(IoError::from(ErrorKind::Other)))]);
    let until = T0.after(ACCEPT_BACKOFF);
    assert_eq!(t.s.deadline(), Some(until));
    assert_eq!(t.actions(), []);
    t.now = until;
    assert_eq!(t.actions(), [accept(3)]);
    assert_eq!(t.s.deadline(), None);
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

fn shutdown() -> ServerCmd {
    ServerCmd::Shutdown { grace: GRACE }
}

/// `with_request`, plus a second connection `D` still reading its head,
/// its read armed.
fn with_request_and_idle_conn() -> (T, SockId) {
    let mut t = T::with_request();
    let d = SockId(4);
    t.reap([ready(L)]);
    assert_eq!(t.actions(), [accept(4)]);
    t.reap([accepted(4, Ok(peer()))]);
    assert_eq!(t.actions(), [read_act(d, MAX_HEAD), accept(5)]);
    t.reap([read_err(d, wb()), accepted(5, Err(wb()))]);
    assert_eq!(
        t.actions(),
        [arm(d, Interest::READ), arm(L, Interest::READ)]
    );
    (t, d)
}

#[test]
fn shutdown_closes_idle_conns_at_once_and_cuts_the_rest_at_the_deadline() {
    let (mut t, d) = with_request_and_idle_conn();
    let (out, actions) = t.step([shutdown()]);
    assert_eq!(out, []);
    assert_eq!(
        actions,
        [close(d), close(L)],
        "pending arms don't delay closing"
    );
    let until = T0.after(GRACE);
    assert_eq!(t.s.deadline(), Some(until));
    t.reap([closed(d), closed(L)]);
    assert!(!t.s.idle(), "C still waits for its response");
    t.now = Time(until.0 - 1);
    assert_eq!(t.actions(), []);
    t.now = until;
    let (out, actions) = t.step([]);
    assert_eq!(out, [ServerEvent::Gone { req: REQ }]);
    assert_eq!(actions, [close(C)]);
    assert_eq!(t.s.deadline(), None);
    t.reap([closed(C)]);
    assert!(t.s.idle());
}

#[test]
fn shutdown_cuts_a_stalled_write_at_the_deadline() {
    let mut t = T::with_request();
    let bytes = write_response(&ok(b"big"));
    t.step([respond(REQ, b"big")]);
    t.reap([wrote(C, &bytes, Err(wb()))]);
    assert_eq!(t.actions(), [arm(C, Interest::WRITE)]);
    let (_, actions) = t.step([shutdown()]);
    assert_eq!(actions, [close(L)], "the response gets its grace");
    t.now = T0.after(GRACE);
    let (out, actions) = t.step([]);
    assert_eq!(out, [ServerEvent::Gone { req: REQ }]);
    assert_eq!(actions, [close(C)]);
}

#[test]
fn shutdown_ends_early_when_the_responses_finish() {
    let mut t = T::with_request();
    let (_, actions) = t.step([shutdown()]);
    assert_eq!(actions, [close(L)]);
    t.reap([closed(L)]);
    let bytes = write_response(&ok(b"late"));
    let (_, actions) = t.step([respond(REQ, b"late")]);
    assert_eq!(actions, [write_act(C, &bytes)]);
    t.reap([wrote(C, &bytes, Ok(bytes.len()))]);
    assert_eq!(t.actions(), [close(C)]);
    assert_eq!(t.reap([closed(C)]), [], "no Gone: the response completed");
    assert!(t.s.idle(), "idle well before the deadline");
}

#[test]
fn a_socket_accepted_during_shutdown_is_closed() {
    let mut t = T::new(Config::default());
    assert_eq!(t.actions(), [accept(2)]);
    let (_, actions) = t.step([shutdown()]);
    assert_eq!(actions, [], "the accept in flight comes first");
    t.reap([accepted(2, Ok(peer()))]);
    assert_eq!(t.actions(), [close(C), close(L)]);
    t.reap([closed(C), closed(L)]);
    assert!(t.s.idle());
}

#[test]
fn a_second_shutdown_keeps_the_first_deadline() {
    let mut t = T::with_request();
    t.step([shutdown()]);
    t.now = Time(T0.0 + 5);
    t.step([ServerCmd::Shutdown {
        grace: Duration::from_secs(60),
    }]);
    assert_eq!(t.s.deadline(), Some(T0.after(GRACE)));
}

// ---------------------------------------------------------------------------
// Raw relay
// ---------------------------------------------------------------------------

#[test]
fn a_raw_relay_reports_each_flush_and_closes_after_its_end() {
    let mut t = T::with_request();
    assert_eq!(t.step([ServerCmd::RawStart { req: REQ }]), (vec![], vec![]));

    let head = b"HTTP/1.1 200 OK\r\n\r\n";
    let (out, actions) = t.step([raw_bytes(head)]);
    assert_eq!(out, []);
    assert_eq!(actions, [write_act(C, head)]);
    assert_eq!(t.reap([wrote(C, head, Ok(5))]), [], "not all written yet");
    assert_eq!(t.actions(), [write_act(C, &head[5..])]);
    assert_eq!(
        t.reap([wrote(C, &head[5..], Ok(head.len() - 5))]),
        [flushed()]
    );

    // Two batches before a write go out as one write and one flush.
    let (out, actions) = t.step([raw_bytes(b"ab"), raw_bytes(b"cd")]);
    assert_eq!(out, []);
    assert_eq!(actions, [write_act(C, b"abcd")]);
    assert_eq!(t.reap([wrote(C, b"abcd", Ok(4))]), [flushed()]);

    // Nothing to write is flushed at once, so credit keeps flowing.
    assert_eq!(t.step([raw_bytes(b"")]), (vec![flushed()], vec![]));

    let (out, actions) = t.step([raw_bytes(b"end"), ServerCmd::RawEnd { req: REQ }]);
    assert_eq!(out, []);
    assert_eq!(actions, [write_act(C, b"end")]);
    assert_eq!(t.reap([wrote(C, b"end", Ok(3))]), [flushed()]);
    assert_eq!(t.actions(), [close(C)]);
    assert_eq!(t.reap([closed(C)]), []);
}

#[test]
fn a_write_error_mid_relay_is_gone_and_reads_nothing() {
    // Writing stops at the error while reading could go on (the client
    // planner's early responses), but the server has nothing left to read.
    let mut t = T::with_request();
    t.step([ServerCmd::RawStart { req: REQ }]);
    let (_, actions) = t.step([raw_bytes(b"part")]);
    assert_eq!(actions, [write_act(C, b"part")]);
    let pipe = IoError::from(ErrorKind::BrokenPipe);
    assert_eq!(
        t.reap([wrote(C, b"part", Err(pipe))]),
        [ServerEvent::Gone { req: REQ }],
        "no Flushed for bytes never written"
    );
    assert_eq!(t.actions(), [close(C)]);
    let (out, actions) = t.step([raw_bytes(b"more"), ServerCmd::RawEnd { req: REQ }]);
    assert_eq!((out, actions), (vec![], vec![]));
    assert_eq!(t.reap([closed(C)]), []);
}

#[test]
fn a_raw_end_with_everything_written_closes_at_once() {
    let mut t = T::with_request();
    t.step([ServerCmd::RawStart { req: REQ }]);
    let (out, actions) = t.step([ServerCmd::RawEnd { req: REQ }]);
    assert_eq!(out, []);
    assert_eq!(actions, [close(C)]);
}

fn flushed() -> ServerEvent {
    ServerEvent::Flushed { req: REQ }
}
