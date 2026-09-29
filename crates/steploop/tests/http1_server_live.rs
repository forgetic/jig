//! The server planner over real loopback sockets, driven by `run` with a
//! trivial core: concurrent clients, an idle client, a slow reader, shutdown
//! cutting a stalled response at its grace, and the replay of a tapped
//! exchange. Scripted tests (`http1_server.rs`) cover the planner's logic;
//! these check it against real kernel semantics (§4.2).
//!
//! Clients are blocking std sockets with timeouts on their own threads.
//! Waiting is a blocking read or a join, except where a test sleeps through
//! a stretch in which the loop should do nothing, and counts its iterations
//! to show that it does (`Count`): no other check notices a loop that spins.

use std::convert::Infallible;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use steploop::http1::codec::write_response;
use steploop::http1::server::{Config, Server, ServerCmd, ServerEvent};
use steploop::http1::{Body, ReqId, Response};
use steploop::reactor::{Reactor, SignalSender};
use steploop::run::{Core, Count, NoHost, Observe, Tap, replay, run};
use steploop::sys::{Action, Event, Ids};
use steploop::time::Time;

const TIMEOUT: Duration = Duration::from_secs(30);
const GRACE: Duration = Duration::from_secs(5);
/// Four times the most a loopback send buffer holds here (`tcp_wmem`), so a
/// reader with a small window makes the server's writes block.
const BIG: usize = 16 << 20;
/// How long a test watches a loop with nothing to do.
const IDLE: Duration = Duration::from_millis(500);
/// The most iterations such a stretch may take: a few to finish what came
/// before it. A spinning loop takes thousands a millisecond, and even one
/// polling every millisecond would take hundreds.
const IDLE_ITERATIONS: u64 = 20;

/// Answers each request with `hello {target}` (or `BIG` bytes for `/big`),
/// and shuts down on the signal.
#[derive(Default)]
struct Hello {
    grace: Duration,
    stopping: bool,
    gone: Vec<ReqId>,
}

impl Core for Hello {
    type Comp = ServerEvent;
    type IoReq = ServerCmd;
    type HostReq = Infallible;

    fn step(
        &mut self,
        _now: Time,
        comps: &mut Vec<ServerEvent>,
        io: &mut Vec<ServerCmd>,
        _host: &mut Vec<Infallible>,
    ) {
        for comp in comps.drain(..) {
            match comp {
                ServerEvent::Request { req, request } => io.push(ServerCmd::Respond {
                    req,
                    response: hello(&request.target),
                }),
                ServerEvent::Gone { req } => self.gone.push(req),
                ServerEvent::Flushed { .. } => {}
                ServerEvent::Signal { .. } if !self.stopping => {
                    self.stopping = true;
                    io.push(ServerCmd::Shutdown { grace: self.grace });
                }
                ServerEvent::Signal { .. } => {}
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

fn hello(target: &str) -> Response {
    let body = if target == "/big" {
        (0..BIG).map(|i| b"0123456789abcdef"[i % 16]).collect()
    } else {
        format!("hello {target}").into_bytes()
    };
    Response {
        status: 200,
        reason: "OK".into(),
        headers: vec![("Content-Length".into(), body.len().to_string())],
        body: Body::Full(body),
    }
}

fn expected(target: &str) -> Vec<u8> {
    write_response(&hello(target))
}

/// Counts blocked writes and write arms: evidence of backpressure, without
/// keeping a `Tap`'s copies of every buffer.
#[derive(Default)]
struct Counts {
    blocked_writes: usize,
    write_arms: usize,
}

impl Observe<ServerEvent, Infallible> for Counts {
    fn polled(&mut self, _now: Time, events: &[Event]) {
        self.blocked_writes += events
            .iter()
            .filter(|e| matches!(e, Event::Wrote { result: Err(e), .. } if e.is_would_block()))
            .count();
    }

    fn host_reqs(&mut self, _reqs: &[Infallible]) {}

    fn host_answers(&mut self, _answers: &[ServerEvent]) {}

    fn actions(&mut self, actions: &[Action]) {
        self.write_arms += actions
            .iter()
            .filter(|a| matches!(a, Action::Arm { interest, .. } if interest.write))
            .count();
    }
}

type Outcome<T> = (std::io::Result<()>, Hello, T);

/// A server loop on its own thread.
struct Running<T> {
    addr: SocketAddr,
    stop: SignalSender,
    done: mpsc::Receiver<()>,
    thread: JoinHandle<Outcome<T>>,
}

fn start<T>(grace: Duration, mut tap: T) -> Running<T>
where
    T: Observe<ServerEvent, Infallible> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut server = Server::new(Ids::new(), Config::default());
    let mut reactor = Reactor::new().unwrap();
    reactor.adopt_listener(server.listener(), listener).unwrap();
    let (_, stop) = reactor.signal().unwrap();
    let (done_tx, done) = mpsc::channel();
    let thread = thread::spawn(move || {
        let mut core = Hello {
            grace,
            ..Hello::default()
        };
        let result = run(&mut core, &mut server, &mut NoHost, &mut reactor, &mut tap);
        let _ = done_tx.send(());
        (result, core, tap)
    });
    Running {
        addr,
        stop,
        done,
        thread,
    }
}

impl<T> Running<T> {
    /// Raise the stop signal and wait for the loop to end.
    fn stop(self) -> (Hello, T) {
        self.stop.raise().unwrap();
        self.done
            .recv_timeout(TIMEOUT)
            .expect("the loop ends after the stop signal");
        let (result, core, tap) = self.thread.join().unwrap();
        result.unwrap();
        (core, tap)
    }
}

fn connect(addr: SocketAddr) -> TcpStream {
    with_timeouts(TcpStream::connect(addr).unwrap())
}

/// A client with a small receive window (set before connecting, so the
/// window stays small).
fn connect_small_window(addr: SocketAddr) -> TcpStream {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    s.set_recv_buffer_size(4096).unwrap();
    s.connect(&addr.into()).unwrap();
    with_timeouts(s.into())
}

fn with_timeouts(c: TcpStream) -> TcpStream {
    c.set_read_timeout(Some(TIMEOUT)).unwrap();
    c.set_write_timeout(Some(TIMEOUT)).unwrap();
    c
}

fn send_get(c: &mut TcpStream, target: &str) {
    let head = format!("GET {target} HTTP/1.1\r\nHost: test\r\n\r\n");
    c.write_all(head.as_bytes()).unwrap();
}

/// One whole exchange: the response, read until the server closes.
fn exchange(addr: SocketAddr, target: &str) -> Vec<u8> {
    let mut c = connect(addr);
    send_get(&mut c, target);
    let mut got = Vec::new();
    c.read_to_end(&mut got).unwrap();
    got
}

/// What a client still gets from a connection the server closed without
/// answering: nothing, or a reset if its bytes were never read.
fn assert_closed_without_response(c: &mut TcpStream) {
    let mut got = Vec::new();
    match c.read_to_end(&mut got) {
        Ok(_) => assert_eq!(got, b""),
        Err(e) => assert_eq!(e.kind(), ErrorKind::ConnectionReset),
    }
}

#[test]
fn serves_concurrent_clients() {
    let server = start(GRACE, Counts::default());
    let addr = server.addr;
    let clients: Vec<_> = (0..16)
        .map(|i| thread::spawn(move || exchange(addr, &format!("/client/{i}"))))
        .collect();
    for (i, c) in clients.into_iter().enumerate() {
        assert_eq!(c.join().unwrap(), expected(&format!("/client/{i}")));
    }
    let (core, _) = server.stop();
    assert_eq!(core.gone, []);
}

#[test]
fn idle_clients_do_not_block_others_and_are_closed_at_shutdown() {
    let server = start(GRACE, Counts::default());
    let mut partial = connect(server.addr);
    partial.write_all(b"GET /never HT").unwrap();
    let mut silent = connect(server.addr);
    for i in 0..3 {
        let target = format!("/while-idle/{i}");
        assert_eq!(exchange(server.addr, &target), expected(&target));
    }
    // Neither had a request in, so shutdown closes them at once rather than
    // waiting out the (long) grace.
    let started = Instant::now();
    let (core, _) = server.stop();
    assert!(started.elapsed() < GRACE);
    assert_eq!(core.gone, []);
    assert_closed_without_response(&mut partial);
    assert_closed_without_response(&mut silent);
}

#[test]
fn an_idle_server_sleeps() {
    let count = Count::new();
    let server = start(GRACE, count.clone());
    assert_eq!(exchange(server.addr, "/warm"), expected("/warm"));
    let mut partial = connect(server.addr);
    partial.write_all(b"GET /never HT").unwrap();
    let _silent = connect(server.addr);
    // Accepted in order, so by this reply the idle ones are in and armed.
    assert_eq!(exchange(server.addr, "/witness"), expected("/witness"));

    let before = count.iterations();
    thread::sleep(IDLE);
    let idle = count.iterations() - before;
    assert!(idle <= IDLE_ITERATIONS, "{idle} iterations while idle");
    server.stop();
}

#[test]
fn a_slow_reader_does_not_block_others() {
    let server = start(GRACE, Counts::default());
    let mut slow = connect_small_window(server.addr);
    send_get(&mut slow, "/big");
    // Once the first byte is here the server is writing, and with a window
    // this small it can't get far.
    let mut first = [0u8; 1];
    slow.read_exact(&mut first).unwrap();
    for i in 0..3 {
        let target = format!("/while-slow/{i}");
        assert_eq!(exchange(server.addr, &target), expected(&target));
    }
    let mut rest = Vec::new();
    slow.read_to_end(&mut rest).unwrap();
    let whole = expected("/big");
    assert_eq!(first[0], whole[0]);
    assert!(rest == whole[1..], "the big response arrived whole");
    let (core, counts) = server.stop();
    assert_eq!(core.gone, []);
    assert!(counts.blocked_writes > 0, "writes blocked");
    assert!(counts.write_arms > 0, "and the server armed for writing");
}

#[test]
fn shutdown_cuts_a_stalled_response_at_its_grace() {
    let grace = Duration::from_millis(200);
    let server = start(grace, Count::new());
    let mut stalled = connect_small_window(server.addr);
    send_get(&mut stalled, "/big");
    let mut first = [0u8; 1];
    stalled.read_exact(&mut first).unwrap();
    // The stalled write, then the grace, both with nothing to do.
    thread::sleep(IDLE);
    let started = Instant::now();
    let (core, count) = server.stop();
    assert!(started.elapsed() >= grace, "the response had its grace");
    assert_eq!(core.gone.len(), 1, "and the core heard it was cut");
    let iterations = count.iterations();
    assert!(
        iterations <= 2 * IDLE_ITERATIONS,
        "{iterations} iterations for one stalled exchange"
    );
    let mut rest = Vec::new();
    let _ = stalled.read_to_end(&mut rest);
    assert!(1 + rest.len() < expected("/big").len(), "cut short");
}

#[test]
fn a_tapped_exchange_replays_exactly() {
    let server = start(GRACE, Tap::new());
    assert_eq!(exchange(server.addr, "/replay"), expected("/replay"));
    let (_, tap) = server.stop();
    let actions = || tap.iterations.iter().flat_map(|it| &it.outputs.actions);
    assert!(actions().any(|a| matches!(a, Action::Accept { .. })));
    assert!(actions().any(|a| matches!(a, Action::Write { .. })));

    let mut core = Hello {
        grace: GRACE,
        ..Hello::default()
    };
    let mut server = Server::new(Ids::new(), Config::default());
    let replayed = replay(&mut core, &mut server, &tap);
    assert!(replayed == tap.outputs(), "replay diverged");
    assert!(server.idle());
}
