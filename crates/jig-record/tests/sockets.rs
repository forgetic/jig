//! `Recorder` over real loopback sockets, in front of a local TLS upstream
//! with its own CA (design `docs/explanation/sans-io-shell.md` §7): the
//! bytes it relays and captures, a certificate it must refuse, a preflight,
//! an idle pooled connection, an upstream cut mid-stream, and several
//! exchanges at once. The scripted tests in `src/relay/tests.rs` cover the
//! core's logic; these check it against the kernel and rustls.
//!
//! Upstreams and clients are blocking std sockets on their own threads.
//! Waiting is a blocking read, a channel or a join, except in the one test
//! that sleeps through a stalled relay, counting the loop's iterations: no
//! other check notices a loop that spins.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use jig_record::relay::{Comp, HostReq};
use jig_record::{
    ClientRequest, Header, Mode, Recorder, RecorderConfig, RecorderCore, RecorderHost, RecorderIo,
    Route, UpstreamOverride, UpstreamResponse,
};
use rustls::ServerConfig;
use steploop::reactor::Reactor;
use steploop::run::{Count, Observe, run};
use steploop::sys::{Action, Event, Ids};
use steploop::time::Time;
use steploop::tls::client_config;

mod support;
use support::*;

const OPENAI: &str = "/chat/completions";
const SSE_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";

fn chunk(data: &str) -> Vec<u8> {
    format!("{:x}\r\n{data}\r\n", data.len()).into_bytes()
}

/// An SSE body in several chunks, `tag` in each, then the last chunk.
fn sse_chunks(tag: &str) -> Vec<Vec<u8>> {
    (0..3)
        .map(|i| chunk(&format!("data: {{\"{tag}\":{i}}}\n\n")))
        .chain([b"0\r\n\r\n".to_vec()])
        .collect()
}

/// Writes the head, then each chunk on its own, then closes properly.
fn respond(mut tls: Tls, chunks: &[Vec<u8>]) {
    tls.write_all(SSE_HEAD).unwrap();
    for c in chunks {
        tls.write_all(c).unwrap();
        tls.flush().unwrap();
    }
    close(tls);
}

/// A recorder whose fetches go to `upstream` over TLS, trusting `pki`.
fn recorder(mode: Mode, upstream: SocketAddr, pki: &Pki) -> Recorder {
    Recorder::start(RecorderConfig {
        mode,
        upstream_host: None,
        upstream: Some(UpstreamOverride {
            addr: upstream,
            tls: true,
        }),
        roots: Some(pki.roots.clone()),
    })
    .unwrap()
}

/// An upstream serving one exchange with `chunks`; returns what it was sent.
fn upstream_once(pki: &Pki, chunks: Vec<Vec<u8>>) -> (SocketAddr, thread::JoinHandle<Vec<u8>>) {
    let (listener, addr) = listen();
    let config = server_config(pki);
    let upstream = thread::spawn(move || {
        let mut tls = accept_tls(&listener, &config);
        let request = read_request(&mut tls);
        respond(tls, &chunks);
        request
    });
    (addr, upstream)
}

fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
    pairs.iter().map(|(n, v)| Header::new(*n, *v)).collect()
}

#[test]
fn relays_the_upstream_bytes_and_captures_them_exactly() {
    let pki = pki("happy path CA");
    let chunks = sse_chunks("n");
    let (up_addr, upstream) = upstream_once(&pki, chunks.clone());
    let rec = recorder(Mode::Once, up_addr, &pki);
    let body = r#"{"model":"m","stream":true}"#;

    let got = exchange(addr_of(&rec.base_url()), &post(OPENAI, body));
    assert_eq!(got, [SSE_HEAD.to_vec(), chunks.concat()].concat());

    let forwarded = format!(
        "POST /chat/completions HTTP/1.1\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Host: api.openai.com\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let sent = upstream.join().unwrap();
    assert_eq!(String::from_utf8_lossy(&sent), forwarded);

    let (request, response, route) = rec.next_capture(TIMEOUT).unwrap();
    let client_headers = [
        ("Host", "jig"),
        ("Content-Type", "application/json"),
        ("Content-Length", "27"),
        ("Connection", "close"),
    ];
    let want = ClientRequest {
        method: "POST".into(),
        target: OPENAI.into(),
        headers: headers(&client_headers),
        body: body.as_bytes().to_vec(),
    };
    assert_eq!(request, want);
    let upstream_headers = [
        ("Content-Type", "text/event-stream"),
        ("Transfer-Encoding", "chunked"),
        ("Connection", "close"),
    ];
    let want = UpstreamResponse {
        status: 200,
        headers: headers(&upstream_headers),
        body: chunks.concat(),
    };
    assert_eq!(response, want, "the raw body, chunk framing and all");
    assert_eq!(route, Route::resolve(OPENAI).unwrap());
    // Once mode stopped by itself after the capture.
    assert_eq!(rec.stop().len(), 0);
}

#[test]
fn an_unknown_ca_closes_the_client_without_a_response_and_ends_once_mode() {
    let theirs = pki("the upstream's CA");
    let ours = pki("a stranger CA");
    let (listener, up_addr) = listen();
    let config = server_config(&theirs);
    let upstream = thread::spawn(move || {
        let mut tls = accept_tls(&listener, &config);
        tls.read(&mut [0u8; 16]).expect_err("the handshake fails")
    });
    let rec = recorder(Mode::Once, up_addr, &ours);

    let mut c = connect(addr_of(&rec.base_url()));
    c.write_all(&post(OPENAI, "{}")).unwrap();
    assert_closed_without_response(&mut c);
    let seen = upstream.join().unwrap().to_string();
    assert!(seen.contains("alert"), "the upstream heard why: {seen}");

    // The loop has ended, so this fails at once rather than timing out.
    let started = Instant::now();
    let err = rec.next_capture(TIMEOUT).unwrap_err();
    assert_ne!(err.kind(), ErrorKind::TimedOut, "{err}");
    assert!(started.elapsed() < TIMEOUT);
}

#[test]
fn a_preflight_gets_204_and_the_real_request_is_captured() {
    let pki = pki("preflight CA");
    let (up_addr, upstream) = upstream_once(&pki, sse_chunks("p"));
    let rec = recorder(Mode::Once, up_addr, &pki);
    let addr = addr_of(&rec.base_url());

    let preflight = exchange(addr, b"HEAD / HTTP/1.1\r\nHost: jig\r\n\r\n");
    assert_eq!(
        preflight,
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    exchange(addr, &post(OPENAI, "{}"));
    let (request, response, _) = rec.next_capture(TIMEOUT).unwrap();
    assert_eq!((request.method.as_str(), response.status), ("POST", 200));
    // Only the real request went upstream.
    assert!(
        upstream
            .join()
            .unwrap()
            .starts_with(b"POST /chat/completions ")
    );
}

#[test]
fn an_idle_pooled_connection_does_not_hold_up_the_capture() {
    let pki = pki("idle pool CA");
    let (up_addr, _upstream) = upstream_once(&pki, sse_chunks("i"));
    let rec = recorder(Mode::Once, up_addr, &pki);
    let addr = addr_of(&rec.base_url());

    // Opened first and never used, as a client's pool does.
    let mut idle = connect(addr);
    exchange(addr, &post(OPENAI, "{}"));
    let (request, _, _) = rec.next_capture(TIMEOUT).unwrap();
    assert_eq!(request.target, OPENAI);
    // Shutting down closed it without the grace: it never sent a request.
    let started = Instant::now();
    rec.stop();
    assert!(started.elapsed() < jig_record::relay::GRACE);
    assert_closed_without_response(&mut idle);
}

#[test]
fn an_upstream_cut_mid_stream_truncates_the_relay_without_a_capture() {
    let pki = pki("cut CA");
    let first = chunk("data: {\"before\":1}\n\n");
    let (listener, up_addr) = listen();
    let config = server_config(&pki);
    let (cut_tx, cut) = mpsc::channel();
    let sent = first.clone();
    let upstream = thread::spawn(move || {
        let mut tls = accept_tls(&listener, &config);
        read_request(&mut tls);
        tls.write_all(SSE_HEAD).unwrap();
        tls.write_all(&sent).unwrap();
        tls.flush().unwrap();
        cut.recv_timeout(TIMEOUT).expect("the client has it");
        // A reset, not a clean end: SO_LINGER 0, then close.
        socket2::SockRef::from(&tls.sock)
            .set_linger(Some(Duration::ZERO))
            .unwrap();
    });
    let rec = recorder(Mode::Once, up_addr, &pki);

    let mut c = connect(addr_of(&rec.base_url()));
    c.write_all(&post(OPENAI, "{}")).unwrap();
    let relayed = [SSE_HEAD, &first].concat();
    let mut got = vec![0; relayed.len()];
    c.read_exact(&mut got).unwrap();
    assert_eq!(got, relayed);
    cut_tx.send(()).unwrap();
    upstream.join().unwrap();

    let mut rest = Vec::new();
    c.read_to_end(&mut rest).unwrap();
    assert_eq!(
        rest, b"",
        "truncated: the relay ended where the upstream did"
    );
    let err = rec.next_capture(TIMEOUT).unwrap_err();
    assert_ne!(err.kind(), ErrorKind::TimedOut, "{err}");
}

/// Serves `n` connections, each answered with its request's body as the
/// data, reading all `n` requests before answering any, in reverse order:
/// the recorder must hold `n` exchanges open at once.
fn upstream_all_at_once(config: Arc<ServerConfig>, listener: TcpListener, n: usize) {
    let mut open: Vec<(Tls, Vec<u8>)> = (0..n)
        .map(|_| {
            let mut tls = accept_tls(&listener, &config);
            let request = read_request(&mut tls);
            (tls, request)
        })
        .collect();
    while let Some((tls, request)) = open.pop() {
        respond(tls, &echo(&request));
    }
}

/// The chunks an upstream answers `request` with: its body as the data.
fn echo(request: &[u8]) -> Vec<Vec<u8>> {
    let end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    echo_body(&String::from_utf8_lossy(&request[end + 4..]))
}

fn echo_body(body: &str) -> Vec<Vec<u8>> {
    vec![chunk(&format!("data: {body}\n\n")), b"0\r\n\r\n".to_vec()]
}

/// What a client posting `body` gets back through the recorder.
fn echoed(body: &str) -> Vec<u8> {
    [SSE_HEAD.to_vec(), echo_body(body).concat()].concat()
}

#[test]
fn pump_mode_captures_every_exchange_concurrent_ones_included() {
    const CONCURRENT: usize = 4;
    const SEQUENTIAL: usize = 2;
    let pki = pki("pump CA");
    let (listener, up_addr) = listen();
    let config = server_config(&pki);
    let upstream = thread::spawn(move || {
        upstream_all_at_once(
            Arc::clone(&config),
            listener.try_clone().unwrap(),
            CONCURRENT,
        );
        for _ in 0..SEQUENTIAL {
            upstream_all_at_once(Arc::clone(&config), listener.try_clone().unwrap(), 1);
        }
    });
    let rec = recorder(Mode::Pump, up_addr, &pki);
    let addr = addr_of(&rec.base_url());

    let mut bodies: Vec<String> = (0..CONCURRENT).map(|i| format!("{{\"c\":{i}}}")).collect();
    let clients: Vec<_> = bodies
        .iter()
        .cloned()
        .map(|body| thread::spawn(move || (exchange(addr, &post(OPENAI, &body)), body)))
        .collect();
    for client in clients {
        let (got, body) = client.join().unwrap();
        assert_eq!(got, echoed(&body));
    }
    for i in 0..SEQUENTIAL {
        let body = format!("{{\"s\":{i}}}");
        assert_eq!(exchange(addr, &post(OPENAI, &body)), echoed(&body));
        bodies.push(body);
    }
    upstream.join().unwrap();

    // Every client had its whole response, which the recorder sends only
    // after capturing it, so every capture is in.
    let captured = rec.stop();
    assert_eq!(captured.len(), CONCURRENT + SEQUENTIAL);
    let mut seen: Vec<String> = captured
        .iter()
        .map(|(request, response, _)| {
            let body = String::from_utf8(request.body.clone()).unwrap();
            let want = echo_body(&body).concat();
            assert_eq!(response.body, want, "each capture has its own response");
            body
        })
        .collect();
    seen.sort();
    bodies.sort();
    assert_eq!(seen, bodies);
    // The sequential ones completed in order, after the concurrent ones.
    let last: Vec<_> = captured[CONCURRENT..]
        .iter()
        .map(|(r, ..)| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    assert_eq!(last, [r#"{"s":0}"#, r#"{"s":1}"#]);
}

// ------------------------------------------------------------------ stalls

/// Counts the loop's iterations, and says when the relay's first write to
/// its client has blocked.
struct Stall {
    count: Count,
    blocked: Option<mpsc::Sender<()>>,
}

impl Observe<Comp, HostReq> for Stall {
    fn polled(&mut self, now: Time, events: &[Event]) {
        Observe::<Comp, HostReq>::polled(&mut self.count, now, events);
        // The server's sockets have the ids below the client's half.
        let upstream_ids = Ids::new().split().next_id();
        let blocked = events.iter().any(|e| {
            matches!(e, Event::Wrote { sock, result: Err(e), .. }
                if e.is_would_block() && sock.0 < upstream_ids)
        });
        if blocked {
            if let Some(tx) = self.blocked.take() {
                let _ = tx.send(());
            }
        }
    }

    fn host_reqs(&mut self, _reqs: &[HostReq]) {}

    fn host_answers(&mut self, _answers: &[Comp]) {}

    fn actions(&mut self, _actions: &[Action]) {}
}

/// A client with a small receive window (set before connecting, so the
/// window stays small).
fn connect_small_window(addr: SocketAddr) -> TcpStream {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    s.set_recv_buffer_size(4096).unwrap();
    s.connect(&addr.into()).unwrap();
    let s: TcpStream = s.into();
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    s
}

/// The recorder's loop, built by hand from its public parts: while a slow
/// reader holds the relay up, the loop sleeps, and the relay then finishes.
#[test]
fn a_relay_held_up_by_a_slow_reader_sleeps() {
    /// Four times the most a loopback send buffer holds, so the relay's
    /// writes to a reader that doesn't read are sure to block.
    const BIG: usize = 16 << 20;
    const IDLE: Duration = Duration::from_millis(500);
    // A few to finish what came before; a spinning loop takes thousands a
    // millisecond.
    const IDLE_ITERATIONS: u64 = 20;
    let pki = pki("stall CA");
    let (up_listener, up_addr) = listen();
    let config = server_config(&pki);
    let event = "data: 0123456789abcdef\n\n".repeat(2_000);
    let body: Vec<u8> = (0..BIG / event.len())
        .map(|_| chunk(&event))
        .chain([b"0\r\n\r\n".to_vec()])
        .collect::<Vec<_>>()
        .concat();
    let sent = body.clone();
    let upstream = thread::spawn(move || {
        let mut tls = accept_tls(&up_listener, &config);
        read_request(&mut tls);
        tls.write_all(SSE_HEAD).unwrap();
        tls.write_all(&sent).unwrap();
        close(tls);
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut reactor = Reactor::new().unwrap();
    // Kept to the end: dropping the last sender raises the signal.
    let (stop_id, _stop) = reactor.signal().unwrap();
    let tls = client_config(Some(pki.roots.clone())).unwrap();
    let mut io = RecorderIo::new(tls, stop_id);
    reactor.adopt_listener(io.listener(), listener).unwrap();
    let (captures_tx, captures) = mpsc::channel();
    let mut host = RecorderHost::new(captures_tx);
    let upstream_override = UpstreamOverride {
        addr: up_addr,
        tls: true,
    };
    let mut core = RecorderCore::new(Mode::Once, None, Some(upstream_override));
    let count = Count::new();
    let (blocked_tx, blocked) = mpsc::channel();
    let mut stall = Stall {
        count: count.clone(),
        blocked: Some(blocked_tx),
    };
    let looped =
        thread::spawn(move || run(&mut core, &mut io, &mut host, &mut reactor, &mut stall));

    let mut slow = connect_small_window(addr);
    slow.write_all(&post(OPENAI, "{}")).unwrap();
    blocked
        .recv_timeout(TIMEOUT)
        .expect("the relay's writes block");
    let before = count.iterations();
    thread::sleep(IDLE);
    let stalled = count.iterations() - before;
    assert!(
        stalled <= IDLE_ITERATIONS,
        "{stalled} iterations while stalled"
    );

    let mut got = Vec::new();
    slow.read_to_end(&mut got).unwrap();
    assert!(got == [SSE_HEAD, &body].concat(), "relayed whole");
    upstream.join().unwrap();
    let (_, response, _) = captures.recv_timeout(TIMEOUT).expect("a capture");
    assert!(response.body == body, "captured whole");
    looped.join().unwrap().unwrap();
}
