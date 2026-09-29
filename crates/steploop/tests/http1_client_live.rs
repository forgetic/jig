//! The client planner over real loopback sockets, driven by `run`: a plain
//! upstream streaming a chunked SSE body slowly, a TLS upstream with an
//! `rcgen` CA, an unknown CA, and the recorder's shape in miniature, the
//! server and client planners in one I/O step relaying to a slow reader.
//! Scripted tests (`http1_client.rs`) cover the planner's logic; these check
//! it against real kernel semantics (§4.2).
//!
//! Upstreams and downstream clients are blocking std sockets on their own
//! threads. No test sleeps: waiting is a blocking read, a channel or a join.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use steploop::http1::client::{Client, ClientCmd, ClientError, ClientEvent, Target};
use steploop::http1::server::{Config, Server, ServerCmd, ServerEvent};
use steploop::http1::{FetchId, ReqId};
use steploop::reactor::Reactor;
use steploop::run::{Core, Host, IoStep, NoHost, NoTap, Observe, Tap, earliest, replay, run};
use steploop::sys::{Action, Event, Ids};
use steploop::tcp::READ_CHUNK;
use steploop::time::Time;

#[cfg(feature = "tls")]
mod support;

const TIMEOUT: Duration = Duration::from_secs(30);
const F: FetchId = FetchId(1);
const SSE_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";

// ---------------------------------------------------------------------------
// Upstreams
// ---------------------------------------------------------------------------

/// Reads a request: the head, then as many body bytes as its
/// `Content-Length` says.
fn read_request(s: &mut impl Read) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse::<usize>().unwrap());
            if got.len() >= end + 4 + len {
                return got;
            }
        }
        let n = s.read(&mut buf).expect("read the request");
        assert!(n > 0, "the request ended early: {got:?}");
        got.extend_from_slice(&buf[..n]);
    }
}

fn chunk(data: &[u8]) -> Vec<u8> {
    [format!("{:x}\r\n", data.len()).as_bytes(), data, b"\r\n"].concat()
}

fn listen() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

fn accept(l: &TcpListener) -> TcpStream {
    let (s, _) = l.accept().unwrap();
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    s.set_write_timeout(Some(TIMEOUT)).unwrap();
    s
}

fn get(addr: SocketAddr, tls: bool) -> ClientCmd {
    ClientCmd::Fetch {
        fetch: F,
        target: Target {
            host: "localhost".into(),
            port: addr.port(),
            tls,
            addr: Some(addr),
        },
        head: b"GET /v1/stream HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_vec(),
        body: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// One fetch under `run`
// ---------------------------------------------------------------------------

/// Starts one fetch, acks every `Body` at once and tells the host how many
/// body bytes it got; done at the fetch's terminal event.
struct OneFetch {
    fetch: Option<ClientCmd>,
    events: Vec<ClientEvent>,
    done: bool,
}

impl Core for OneFetch {
    type Comp = ClientEvent;
    type IoReq = ClientCmd;
    type HostReq = usize;

    fn step(
        &mut self,
        _now: Time,
        comps: &mut Vec<ClientEvent>,
        io: &mut Vec<ClientCmd>,
        host: &mut Vec<usize>,
    ) {
        io.extend(self.fetch.take());
        for event in comps.drain(..) {
            if let ClientEvent::Body { fetch, bytes } = &event {
                io.push(ClientCmd::Ack { fetch: *fetch });
                host.push(bytes.len());
            }
            self.done |= event.is_terminal();
            self.events.push(event);
        }
    }

    fn deadline(&self) -> Option<Time> {
        None
    }

    fn done(&self) -> bool {
        self.done
    }
}

/// Runs `cmd` on its own loop thread and returns the client's events.
fn fetch<H>(client: Client, cmd: ClientCmd, mut host: H) -> Vec<ClientEvent>
where
    H: Host<OneFetch> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reactor = Reactor::new().unwrap();
        let mut core = OneFetch {
            fetch: Some(cmd),
            events: Vec::new(),
            done: false,
        };
        let mut client = client;
        let result = run(&mut core, &mut client, &mut host, &mut reactor, &mut NoTap);
        let _ = tx.send((
            result.map_err(|e| e.to_string()),
            core.events,
            client.idle(),
        ));
    });
    let (result, events, idle) = rx.recv_timeout(TIMEOUT).expect("the fetch ends");
    result.unwrap();
    assert!(idle);
    events
}

/// The head, then the body bytes and how many `Body` events carried them,
/// checking that `End` came last.
fn split(events: &[ClientEvent]) -> (&ClientEvent, Vec<u8>, usize) {
    let (head, rest) = events.split_first().expect("events");
    let (end, bodies) = rest.split_last().expect("an end");
    assert_eq!(*end, ClientEvent::End { fetch: F }, "{events:?}");
    let mut body = Vec::new();
    for event in bodies {
        let ClientEvent::Body { bytes, .. } = event else {
            panic!("{event:?}");
        };
        body.extend_from_slice(bytes);
    }
    (head, body, bodies.len())
}

/// Lets the upstream send its next chunk once the client has everything up
/// to the end of the last one.
struct Pace {
    go: mpsc::Sender<()>,
    /// Body bytes through the end of each chunk still to be confirmed.
    ends: VecDeque<usize>,
    got: usize,
}

impl Host<OneFetch> for Pace {
    fn handle(&mut self, _now: Time, reqs: &mut Vec<usize>, _comps: &mut Vec<ClientEvent>) {
        for n in reqs.drain(..) {
            self.got += n;
            while self.ends.front().is_some_and(|&end| end <= self.got) {
                self.ends.pop_front();
                let _ = self.go.send(());
            }
        }
    }
}

#[test]
fn streams_a_chunked_sse_body_from_a_slow_upstream() {
    let chunks: Vec<Vec<u8>> = (0..6)
        .map(|i| chunk(format!("event: delta\ndata: {{\"n\":{i}}}\n\n").as_bytes()))
        .chain([b"0\r\n\r\n".to_vec()])
        .collect();
    let (listener, addr) = listen();
    let (go, next) = mpsc::channel();
    let sent = chunks.clone();
    let upstream = thread::spawn(move || {
        let mut s = accept(&listener);
        let request = read_request(&mut s);
        s.write_all(SSE_HEAD).unwrap();
        for (i, c) in sent.iter().enumerate() {
            // Each chunk goes only once the client has the one before.
            if i > 0 {
                next.recv_timeout(TIMEOUT).expect("the client got it");
            }
            s.write_all(c).unwrap();
        }
        request
    });
    let ends = chunks
        .iter()
        .scan(0, |sum, c| {
            *sum += c.len();
            Some(*sum)
        })
        .collect();
    let pace = Pace { go, ends, got: 0 };

    let events = fetch(Client::new(Ids::new()), get(addr, false), pace);
    let (head, body, bodies) = split(&events);
    let ClientEvent::Head {
        status,
        headers,
        raw,
        ..
    } = head
    else {
        panic!("{head:?}");
    };
    assert_eq!((*status, raw.as_slice()), (200, SSE_HEAD));
    assert_eq!(headers[1], ("Transfer-Encoding".into(), "chunked".into()));
    assert_eq!(body, chunks.concat(), "the chunk framing is kept");
    assert!(bodies >= chunks.len(), "streamed: no body spans two chunks");
    let request = upstream.join().unwrap();
    let ClientCmd::Fetch { head, .. } = get(addr, false) else {
        unreachable!()
    };
    assert_eq!(request, head, "the request went out verbatim");
}

#[test]
fn a_refused_connection_fails_the_fetch() {
    // Bind, then close, to find a port nothing listens on.
    let (_, addr) = listen();
    let events = fetch(Client::new(Ids::new()), get(addr, false), NoHost);
    let [
        ClientEvent::Failed {
            error: ClientError::Connect(e),
            ..
        },
    ] = &events[..]
    else {
        panic!("{events:?}");
    };
    assert_eq!(e.kind, io::ErrorKind::ConnectionRefused);
}

#[test]
fn an_early_response_arrives_though_the_upload_fails() {
    const TOO_LARGE: &[u8] = b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 17\r\nConnection: close\r\n\r\nrequest too large";
    let (listener, addr) = listen();
    let upstream = thread::spawn(move || {
        let mut s = accept(&listener);
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "the head ended early");
            got.extend_from_slice(&buf[..n]);
        }
        s.write_all(TOO_LARGE).unwrap();
        // FIN first, so the client reads the response and then EOF. The
        // close that follows resets, since the upload was never read, and
        // the client's next write fails.
        s.shutdown(Shutdown::Write).unwrap();
    });
    let cmd = ClientCmd::Fetch {
        fetch: F,
        target: Target {
            host: "localhost".into(),
            port: addr.port(),
            tls: false,
            addr: Some(addr),
        },
        head: format!(
            "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {BIG}\r\n\r\n"
        )
        .into_bytes(),
        body: vec![b'x'; BIG],
    };
    let events = fetch(Client::new(Ids::new()), cmd, NoHost);
    upstream.join().unwrap();
    let (head, body, _) = split(&events);
    assert!(
        matches!(head, ClientEvent::Head { status: 413, .. }),
        "{head:?}"
    );
    assert_eq!(body, b"request too large");
}

#[test]
fn a_failed_lookup_says_why() {
    let cmd = ClientCmd::Fetch {
        fetch: F,
        target: Target {
            host: "nonexistent.invalid".into(),
            port: 80,
            tls: false,
            addr: None,
        },
        head: b"GET / HTTP/1.1\r\nHost: nonexistent.invalid\r\n\r\n".to_vec(),
        body: Vec::new(),
    };
    let events = fetch(Client::new(Ids::new()), cmd, NoHost);
    let [ClientEvent::Failed { error, .. }] = &events[..] else {
        panic!("{events:?}");
    };
    assert!(matches!(error, ClientError::Resolve(Some(_))), "{error:?}");
    let text = error.to_string();
    assert!(text.contains("failed to lookup address"), "{text}");
}

// ---------------------------------------------------------------------------
// TLS
// ---------------------------------------------------------------------------

#[cfg(feature = "tls")]
mod tls {
    use rustls::{ServerConnection, StreamOwned};
    use steploop::tls::client_config;

    use super::support::{pki, server_config};
    use super::*;

    const LEN: usize = 1 << 20;

    fn body() -> Vec<u8> {
        (0..LEN).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn fetches_over_tls() {
        let pki = pki("client live test CA");
        let config = server_config(&pki);
        let (listener, addr) = listen();
        let upstream = thread::spawn(move || {
            let conn = ServerConnection::new(config).unwrap();
            let mut tls = StreamOwned::new(conn, accept(&listener));
            let request = read_request(&mut tls);
            tls.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .unwrap();
            tls.write_all(&body()).unwrap();
            tls.conn.send_close_notify();
            tls.flush().unwrap();
            request
        });
        let client = Client::new(Ids::new()).with_tls(client_config(Some(pki.roots)).unwrap());
        let events = fetch(client, get(addr, true), NoHost);
        let (head, got, bodies) = split(&events);
        assert!(
            matches!(head, ClientEvent::Head { status: 200, .. }),
            "{head:?}"
        );
        assert!(got == body(), "the body arrived whole");
        assert!(bodies > 1);
        let request = upstream.join().unwrap();
        assert!(request.starts_with(b"GET /v1/stream HTTP/1.1\r\n"));
    }

    #[test]
    fn an_unknown_ca_fails_the_fetch_and_tells_the_server() {
        let theirs = pki("the upstream's CA");
        let ours = pki("a stranger CA");
        let config = server_config(&theirs);
        let (listener, addr) = listen();
        let upstream = thread::spawn(move || {
            let conn = ServerConnection::new(config).unwrap();
            let mut tls = StreamOwned::new(conn, accept(&listener));
            let mut buf = [0u8; 16];
            tls.read(&mut buf).expect_err("the handshake fails")
        });
        let client = Client::new(Ids::new()).with_tls(client_config(Some(ours.roots)).unwrap());
        let events = fetch(client, get(addr, true), NoHost);
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
        let seen = upstream.join().unwrap().to_string();
        assert!(seen.contains("alert"), "the server heard why: {seen}");
    }
}

// ---------------------------------------------------------------------------
// The recorder in miniature: a server and a client in one I/O step
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Comp {
    Server(ServerEvent),
    Client(ClientEvent),
}

enum Req {
    Server(ServerCmd),
    Client(ClientCmd),
}

/// The composition: disjoint ids, the client taking its events out of each
/// batch first, and the server's completions ahead of the client's.
struct Relay {
    server: Server,
    client: Client,
    server_out: Vec<ServerEvent>,
    client_out: Vec<ClientEvent>,
}

impl Relay {
    fn new() -> Relay {
        let mut ids = Ids::new();
        let client = Client::new(ids.split());
        Relay {
            server: Server::new(ids, Config::default()),
            client,
            server_out: Vec::new(),
            client_out: Vec::new(),
        }
    }

    fn hand_over(&mut self, comps: &mut Vec<Comp>) {
        comps.extend(self.server_out.drain(..).map(Comp::Server));
        comps.extend(self.client_out.drain(..).map(Comp::Client));
    }
}

impl IoStep for Relay {
    type Comp = Comp;
    type Req = Req;

    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<Comp>) {
        self.client.reap(now, events, &mut self.client_out);
        self.server.reap(now, events, &mut self.server_out);
        self.hand_over(comps);
    }

    fn step(
        &mut self,
        now: Time,
        reqs: &mut Vec<Req>,
        comps: &mut Vec<Comp>,
        actions: &mut Vec<Action>,
    ) {
        for req in reqs.drain(..) {
            match req {
                Req::Server(cmd) => self.server.command(now, cmd),
                Req::Client(cmd) => self.client.command(now, cmd, &mut self.client_out, actions),
            }
        }
        self.server.plan(now, &mut self.server_out, actions);
        self.client.plan(now, &mut self.client_out, actions);
        self.hand_over(comps);
    }

    fn deadline(&self) -> Option<Time> {
        earliest(self.server.deadline(), self.client.deadline())
    }

    fn idle(&self) -> bool {
        self.server.idle() && self.client.idle()
    }
}

/// Relays each request upstream and the response back verbatim, one `Body`
/// at a time: `Ack` once the server has flushed it. Shuts down after the
/// first exchange.
struct RelayCore {
    upstream: SocketAddr,
    next_fetch: u64,
    /// Each fetch's exchange, and whether a relayed `Body` awaits its flush.
    fetches: BTreeMap<FetchId, (ReqId, bool)>,
    by_req: BTreeMap<ReqId, FetchId>,
    bodies: usize,
    largest_body: usize,
    /// A `Body` arrived while the last one was still unacknowledged.
    overdrawn: usize,
    failures: Vec<ClientError>,
    stopping: bool,
}

impl RelayCore {
    fn new(upstream: SocketAddr) -> RelayCore {
        RelayCore {
            upstream,
            next_fetch: 1,
            fetches: BTreeMap::new(),
            by_req: BTreeMap::new(),
            bodies: 0,
            largest_body: 0,
            overdrawn: 0,
            failures: Vec::new(),
            stopping: false,
        }
    }

    fn fetch(&mut self, req: ReqId, request: steploop::http1::Request) -> ClientCmd {
        let fetch = FetchId(self.next_fetch);
        self.next_fetch += 1;
        self.fetches.insert(fetch, (req, false));
        self.by_req.insert(req, fetch);
        let head = format!(
            "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            request.method,
            request.target,
            request.body.len()
        );
        ClientCmd::Fetch {
            fetch,
            target: Target {
                host: "localhost".into(),
                port: self.upstream.port(),
                tls: false,
                addr: Some(self.upstream),
            },
            head: head.into_bytes(),
            body: request.body,
        }
    }
}

impl Core for RelayCore {
    type Comp = Comp;
    type IoReq = Req;
    type HostReq = Infallible;

    fn step(
        &mut self,
        _now: Time,
        comps: &mut Vec<Comp>,
        io: &mut Vec<Req>,
        _host: &mut Vec<Infallible>,
    ) {
        for comp in comps.drain(..) {
            match comp {
                Comp::Server(ServerEvent::Request { req, request }) => {
                    let cmd = self.fetch(req, request);
                    io.push(Req::Client(cmd));
                }
                Comp::Server(ServerEvent::Flushed { req }) => {
                    let Some(&fetch) = self.by_req.get(&req) else {
                        continue;
                    };
                    if let Some((_, owed @ true)) = self.fetches.get_mut(&fetch) {
                        *owed = false;
                        io.push(Req::Client(ClientCmd::Ack { fetch }));
                    }
                }
                Comp::Server(ServerEvent::Gone { req }) => {
                    if let Some(&fetch) = self.by_req.get(&req) {
                        io.push(Req::Client(ClientCmd::Cancel { fetch }));
                    }
                }
                Comp::Server(ServerEvent::Signal { .. }) => {}
                Comp::Client(event) => {
                    let fetch = event.fetch();
                    let Some((req, owed)) = self.fetches.get_mut(&fetch) else {
                        continue;
                    };
                    let req = *req;
                    match event {
                        ClientEvent::Head { raw, .. } => {
                            io.push(Req::Server(ServerCmd::RawStart { req }));
                            io.push(Req::Server(ServerCmd::RawBytes { req, bytes: raw }));
                        }
                        ClientEvent::Body { bytes, .. } => {
                            self.overdrawn += usize::from(*owed);
                            *owed = true;
                            self.bodies += 1;
                            self.largest_body = self.largest_body.max(bytes.len());
                            io.push(Req::Server(ServerCmd::RawBytes { req, bytes }));
                        }
                        ClientEvent::End { .. } | ClientEvent::Failed { .. } => {
                            if let ClientEvent::Failed { error, .. } = event {
                                self.failures.push(error);
                            }
                            io.push(Req::Server(ServerCmd::RawEnd { req }));
                            io.push(Req::Server(ServerCmd::Shutdown { grace: TIMEOUT }));
                            self.stopping = true;
                        }
                    }
                }
            }
        }
    }

    fn deadline(&self) -> Option<Time> {
        None
    }

    fn done(&self) -> bool {
        self.stopping
    }
}

/// Records the run, and says so once the relay's write to its slow reader
/// has blocked.
struct Watch {
    tap: Tap<Comp, Infallible>,
    blocked: Option<mpsc::Sender<()>>,
}

/// The client's first id: the server's sockets are the ones below it.
fn client_ids_start() -> u64 {
    Ids::new().split().next_id()
}

impl Observe<Comp, Infallible> for Watch {
    fn polled(&mut self, now: Time, events: &[Event]) {
        let downstream = client_ids_start();
        let blocked = events.iter().any(|e| match e {
            Event::Wrote {
                sock,
                result: Err(e),
                ..
            } => e.is_would_block() && sock.0 < downstream,
            _ => false,
        });
        if blocked {
            if let Some(tx) = self.blocked.take() {
                let _ = tx.send(());
            }
        }
        self.tap.polled(now, events);
    }

    fn host_reqs(&mut self, reqs: &[Infallible]) {
        self.tap.host_reqs(reqs);
    }

    fn host_answers(&mut self, answers: &[Comp]) {
        self.tap.host_answers(answers);
    }

    fn actions(&mut self, actions: &[Action]) {
        self.tap.actions(actions);
    }
}

/// A downstream client with a small receive window (set before connecting,
/// so the window stays small).
fn connect_small_window(addr: SocketAddr) -> TcpStream {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    s.set_recv_buffer_size(4096).unwrap();
    s.connect(&addr.into()).unwrap();
    let s: TcpStream = s.into();
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    s
}

/// Four times the most a loopback send buffer holds here, so a stalled
/// reader stalls the relay, and the relay the upstream.
const BIG: usize = 16 << 20;

#[test]
fn relays_a_response_to_a_slow_reader_one_body_at_a_time() {
    // The upstream: reads the request, streams a big chunked body, closes.
    let (up_listener, up_addr) = listen();
    let response: Vec<u8> = {
        let data: Vec<u8> = (0..BIG).map(|i| b"data: 0123456789\n\n"[i % 18]).collect();
        let chunks = data.chunks(50_000).map(chunk);
        [SSE_HEAD.to_vec()]
            .into_iter()
            .chain(chunks)
            .chain([b"0\r\n\r\n".to_vec()])
            .collect::<Vec<_>>()
            .concat()
    };
    let sent = response.clone();
    let upstream = thread::spawn(move || {
        let mut s = accept(&up_listener);
        let request = read_request(&mut s);
        s.write_all(&sent).unwrap();
        request
    });

    // The relay, on its own loop thread.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut io = Relay::new();
    let mut reactor = Reactor::new().unwrap();
    reactor
        .adopt_listener(io.server.listener(), listener)
        .unwrap();
    let (blocked_tx, blocked) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let relay = thread::spawn(move || {
        let mut core = RelayCore::new(up_addr);
        let mut watch = Watch {
            tap: Tap::new(),
            blocked: Some(blocked_tx),
        };
        let result = run(&mut core, &mut io, &mut NoHost, &mut reactor, &mut watch);
        let _ = done_tx.send(());
        (result.map_err(|e| e.to_string()), core, watch.tap)
    });

    // The slow reader: it sends a request with a body, reads nothing until
    // the relay's writes to it have blocked, then reads a little at a time.
    let mut down = connect_small_window(addr);
    let body = vec![b'q'; 100_000];
    let request = [
        format!(
            "POST /v1/messages HTTP/1.1\r\nHost: relay\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
        &body,
    ]
    .concat();
    down.write_all(&request).unwrap();
    blocked
        .recv_timeout(TIMEOUT)
        .expect("the relay's writes block");
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match down.read(&mut buf).unwrap() {
            0 => break,
            n => got.extend_from_slice(&buf[..n]),
        }
    }

    done.recv_timeout(TIMEOUT).expect("the relay stops");
    let (result, core, tap) = relay.join().unwrap();
    result.unwrap();
    assert!(got == response, "the response was relayed verbatim");
    let upstream_got = upstream.join().unwrap();
    assert!(
        upstream_got.ends_with(&body),
        "the request body went upstream"
    );
    assert!(upstream_got.starts_with(b"POST /v1/messages HTTP/1.1\r\nHost: localhost\r\n"));
    assert_eq!(core.failures, []);
    assert!(core.bodies > 16, "many bodies: {}", core.bodies);
    assert_eq!(core.overdrawn, 0, "never a second Body before the Ack");
    assert!(core.largest_body <= READ_CHUNK);

    // The composed step is deterministic: the tapped run replays exactly.
    let mut core = RelayCore::new(up_addr);
    let mut io = Relay::new();
    let replayed = replay(&mut core, &mut io, &tap);
    assert!(replayed == tap.outputs(), "replay diverged");
    assert!(io.idle());
}
