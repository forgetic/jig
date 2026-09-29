//! `FakeLlm` over real loopback sockets: what serving on one no-await loop
//! adds (concurrent connections, a bounded `Drop`, `FnMut` rules), the wire
//! bytes it must keep, and the replay of a tapped exchange (design
//! `docs/explanation/sans-io-shell.md` §4.12, §5.8, §6 and §7). The other
//! files here are the oracle for everything that stayed the same.
//!
//! Clients are blocking std sockets with timeouts. No test sleeps: waiting is
//! a blocking read or a join, and "the loop has read this" is established by
//! an exchange the loop can only finish after reading it.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use jig_core::render::frames_to_body;
use jig_core::{
    Dialect, RecordedRequest, Reply, RequestView, Script, ScriptAction, StopReason, Turn, Usage,
    render_anthropic, render_codex, render_openai,
};
use jig_server::provider::{Comp, HostReq};
use jig_server::{FakeLlm, FakeLlmHost, Provider, ProviderConfig, RequestLog, ServerIo};
use steploop::http1::server::{Config, Server};
use steploop::reactor::Reactor;
use steploop::run::{IoStep, Tap, replay, run};
use steploop::sys::Ids;

const OPENAI: &str = "/chat/completions";
const ANTHROPIC: &str = "/v1/messages";
const CODEX: &str = "/backend-api/codex/responses";

const TIMEOUT: Duration = Duration::from_secs(30);
/// Four times the most a loopback send buffer holds here (`tcp_wmem`), so a
/// client that reads slowly, or not at all, makes the server's writes block.
const BIG: usize = 16 << 20;
/// Slack for `Drop` beyond the grace, for a loaded machine.
const MARGIN: Duration = Duration::from_secs(2);

fn grace() -> Duration {
    ProviderConfig::default().grace
}

// ------------------------------------------------------------------- scripts

fn text(content: &str) -> Reply {
    Reply::text(content)
}

fn big_text() -> String {
    "0123456789abcdef".repeat(BIG / 16)
}

/// Echoes the last message, except that `"big"` gets a `BIG` text reply.
fn echo_or_big() -> Script {
    Script::rule(|view| match last_message(view).as_str() {
        "big" => text(&big_text()),
        other => text(other),
    })
}

fn last_message(view: &RequestView) -> String {
    view.last_message()
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

fn logged_message(record: &RecordedRequest) -> String {
    record.view.as_ref().map(last_message).unwrap_or_default()
}

// ------------------------------------------------------------------- clients

fn addr(fake: &FakeLlm) -> SocketAddr {
    let url = fake.base_url();
    url.strip_prefix("http://").unwrap().parse().unwrap()
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

fn post(path: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\n\
         Host: jig\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// A chat request whose last message is `content`. Every dialect's
/// projection reads `messages`.
fn chat(path: &str, content: &str) -> Vec<u8> {
    let body = serde_json::json!({
        "model": "m",
        "stream": true,
        "messages": [{ "role": "user", "content": content }],
    });
    post(path, &body.to_string())
}

/// One whole exchange: the raw response, read until the server closes.
fn exchange(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
    let mut c = connect(addr);
    c.write_all(request).unwrap();
    read_all(&mut c)
}

fn read_all(c: &mut TcpStream) -> Vec<u8> {
    let mut got = Vec::new();
    c.read_to_end(&mut got).unwrap();
    got
}

/// What a client gets from a connection closed without a response: nothing,
/// or a reset if some of its bytes were never read.
fn assert_closed_without_response(c: &mut TcpStream) {
    let mut got = Vec::new();
    match c.read_to_end(&mut got) {
        Ok(_) => assert_eq!(String::from_utf8_lossy(&got), ""),
        Err(e) => assert_eq!(e.kind(), ErrorKind::ConnectionReset),
    }
}

// -------------------------------------------------------------- golden bytes

/// What the async server at `ca1edfd` wrote for `reply` on `dialect`'s route:
/// these headers, then the rendered body as one chunk.
fn sse(dialect: Dialect, reply: &Reply) -> Vec<u8> {
    let frames = match dialect {
        Dialect::OpenAi => render_openai(reply),
        Dialect::Anthropic => render_anthropic(reply),
        Dialect::Codex => render_codex(reply),
    };
    let body = frames_to_body(&frames);
    let mut out = b"HTTP/1.1 200 OK\r\n\
                    Content-Type: text/event-stream\r\n\
                    Cache-Control: no-cache\r\n\
                    Transfer-Encoding: chunked\r\n\
                    Connection: close\r\n\
                    \r\n"
        .to_vec();
    out.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\r\n0\r\n\r\n");
    out
}

fn openai(content: &str) -> Vec<u8> {
    sse(Dialect::OpenAi, &text(content))
}

/// Compare as text first, for a readable failure.
fn assert_bytes(actual: &[u8], expected: &[u8]) {
    assert_eq!(
        String::from_utf8_lossy(actual),
        String::from_utf8_lossy(expected)
    );
    assert_eq!(actual, expected);
}

#[test]
fn each_dialect_gets_the_async_servers_bytes() {
    let reply = Reply {
        turns: vec![
            Turn::Thinking("hmm".to_string()),
            Turn::Text("héllo ✓".to_string()),
            Turn::ToolCall {
                id: "call_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt" }),
            },
        ],
        usage: Usage {
            prompt_tokens: 7,
            completion_tokens: 9,
        },
        stop: StopReason::ToolCalls,
    };
    let fake = FakeLlm::start(Script::Fixed(reply.clone())).unwrap();
    let addr = addr(&fake);
    for (dialect, path) in [
        (Dialect::OpenAi, OPENAI),
        (Dialect::Anthropic, ANTHROPIC),
        (Dialect::Codex, CODEX),
    ] {
        assert_bytes(&exchange(addr, &chat(path, "hi")), &sse(dialect, &reply));
    }
    assert_bytes(
        &exchange(addr, &post("/nope", "")),
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
}

// --------------------------------------------------------------- concurrency

#[test]
fn an_idle_client_does_not_block_others() {
    let fake = FakeLlm::start(echo_or_big()).unwrap();
    let addr = addr(&fake);
    let mut idle = connect(addr);
    for i in 0..3 {
        let content = format!("while idle {i}");
        assert_bytes(&exchange(addr, &chat(OPENAI, &content)), &openai(&content));
    }
    // It never sent a request, so shutdown closes it without the grace.
    let started = Instant::now();
    drop(fake);
    assert!(started.elapsed() < grace());
    assert_closed_without_response(&mut idle);
}

#[test]
fn a_slow_reader_gets_a_large_reply_whole() {
    let fake = FakeLlm::start(echo_or_big()).unwrap();
    let addr = addr(&fake);
    let mut slow = connect_small_window(addr);
    slow.write_all(&chat(OPENAI, "big")).unwrap();
    // Once the first byte is here the server is writing, and with a window
    // this small it can't get far.
    let mut got = vec![0; 1];
    slow.read_exact(&mut got).unwrap();
    for i in 0..3 {
        let content = format!("while slow {i}");
        assert_bytes(&exchange(addr, &chat(OPENAI, &content)), &openai(&content));
    }
    let mut buf = [0; 4096];
    loop {
        match slow.read(&mut buf).unwrap() {
            0 => break,
            n => got.extend_from_slice(&buf[..n]),
        }
    }
    let want = openai(&big_text());
    assert_eq!(got.len(), want.len());
    assert!(got == want, "the large reply arrived byte for byte");
}

#[test]
fn a_client_leaving_mid_reply_does_not_stop_the_server() {
    let fake = FakeLlm::start(echo_or_big()).unwrap();
    let addr = addr(&fake);
    let mut leaver = connect_small_window(addr);
    leaver.write_all(&chat(OPENAI, "big")).unwrap();
    let mut part = vec![0; 64 << 10];
    leaver.read_exact(&mut part).unwrap();
    assert!(openai(&big_text()).starts_with(&part));
    // With bytes left unread, closing resets the connection.
    drop(leaver);

    assert_bytes(&exchange(addr, &chat(OPENAI, "after")), &openai("after"));
    let log: Vec<_> = fake.requests().iter().map(logged_message).collect();
    assert_eq!(log, ["big", "after"]);
    // The cut reply was already given up on, not left to the grace.
    let started = Instant::now();
    drop(fake);
    assert!(started.elapsed() < grace());
}

#[test]
fn drop_cuts_a_stalled_reply_at_the_grace_and_releases_the_port() {
    let fake = FakeLlm::start(echo_or_big()).unwrap();
    let addr = addr(&fake);
    let mut stalled = connect_small_window(addr);
    stalled.write_all(&chat(OPENAI, "big")).unwrap();
    let mut first = [0u8; 1];
    stalled.read_exact(&mut first).unwrap();

    let started = Instant::now();
    drop(fake);
    let took = started.elapsed();
    assert!(took >= grace(), "the reply had its grace: {took:?}");
    assert!(took < grace() + MARGIN, "and no more: {took:?}");
    let refused = TcpStream::connect(addr).map(drop).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::ConnectionRefused);

    // What the kernel had already taken still arrives; the rest never does.
    let mut rest = Vec::new();
    let _ = stalled.read_to_end(&mut rest);
    assert!(1 + rest.len() < openai(&big_text()).len(), "cut short");
}

#[test]
fn drop_while_a_client_is_mid_request_returns_promptly() {
    let fake = FakeLlm::start(echo_or_big()).unwrap();
    let addr = addr(&fake);
    let mut midway = connect(addr);
    let request = chat(OPENAI, "never finished");
    midway.write_all(&request[..request.len() - 5]).unwrap();
    // `midway`'s bytes were sent first, and the loop reads a connection as
    // soon as it has accepted it, so they are read no later than this request
    // is accepted: by its reply, the loop holds the partial request.
    assert_bytes(
        &exchange(addr, &chat(OPENAI, "witness")),
        &openai("witness"),
    );
    let log: Vec<_> = fake.requests().iter().map(logged_message).collect();
    assert_eq!(log, ["witness"], "an incomplete request is not recorded");

    let started = Instant::now();
    drop(fake);
    assert!(
        started.elapsed() < grace(),
        "no grace for an unfinished request"
    );
    assert_closed_without_response(&mut midway);
}

#[test]
fn the_script_and_the_log_follow_the_order_requests_complete() {
    let script = Script::sequence(vec![text("first"), text("second"), text("third")]);
    let fake = FakeLlm::start(script).unwrap();
    let addr = addr(&fake);
    // Connected in the order a, b, c, each holding back its last byte.
    let mut clients: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|name| {
            let mut c = connect(addr);
            let request = chat(OPENAI, name);
            let (head, last) = request.split_at(request.len() - 1);
            c.write_all(head).unwrap();
            (name, c, last.to_vec())
        })
        .collect();
    for (name, reply) in [("c", "first"), ("a", "second"), ("b", "third")] {
        let (_, c, last) = clients.iter_mut().find(|(n, ..)| *n == name).unwrap();
        c.write_all(last).unwrap();
        assert_bytes(&read_all(c), &openai(reply));
    }
    let log: Vec<_> = fake.requests().iter().map(logged_message).collect();
    assert_eq!(log, ["c", "a", "b"]);
}

#[test]
fn concurrent_clients_find_the_log_in_script_order() {
    const N: usize = 16;
    let script = Script::sequence((0..N).map(|k| text(&format!("reply {k}"))).collect());
    let fake = FakeLlm::start(script).unwrap();
    let addr = addr(&fake);
    let clients: Vec<_> = (0..N)
        .map(|i| thread::spawn(move || exchange(addr, &chat(OPENAI, &format!("client {i}")))))
        .collect();
    // Which step of the script each client was served.
    let served: Vec<usize> = clients
        .into_iter()
        .map(|c| {
            let got = c.join().unwrap();
            (0..N)
                .find(|k| got == openai(&format!("reply {k}")))
                .expect("a reply from the script")
        })
        .collect();
    let log = fake.requests();
    assert_eq!(log.len(), N);
    for (i, k) in served.into_iter().enumerate() {
        assert_eq!(logged_message(&log[k]), format!("client {i}"));
    }
}

// --------------------------------------------------------------------- rules

#[test]
fn a_rule_sees_each_view_and_keeps_its_state() {
    // `FnMut`: the state lives in the closure, with no lock or atomic.
    let mut seen = Vec::new();
    let script = Script::action_rule(move |view| {
        seen.push(format!("{:?}:{}", view.dialect, last_message(view)));
        ScriptAction::Reply(text(&seen.join(",")))
    });
    let fake = FakeLlm::start(script).unwrap();
    let addr = addr(&fake);
    assert_bytes(&exchange(addr, &chat(OPENAI, "one")), &openai("OpenAi:one"));
    assert_bytes(
        &exchange(addr, &chat(ANTHROPIC, "two")),
        &sse(Dialect::Anthropic, &text("OpenAi:one,Anthropic:two")),
    );
}

// -------------------------------------------------------------------- replay

/// The loop `FakeLlm` runs, built by hand from the public parts with a `Tap`:
/// a real exchange, decided by a rule, replays exactly into fresh steps.
#[test]
fn a_tapped_exchange_replays_exactly() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (plan, rule) = echo_or_big().split();
    let server = Server::new(Ids::new(), Config::default());
    let mut reactor = Reactor::new().unwrap();
    reactor.adopt_listener(server.listener(), listener).unwrap();
    let (stop_id, stop) = reactor.signal().unwrap();
    let log = RequestLog::default();
    let mut host = FakeLlmHost::new(Arc::clone(&log), rule);
    let mut io = ServerIo::new(server, stop_id);
    let mut provider = Provider::new(plan.clone());
    let looped = thread::spawn(move || {
        let mut tap = Tap::new();
        let result = run(&mut provider, &mut io, &mut host, &mut reactor, &mut tap);
        (result, tap)
    });

    assert_bytes(
        &exchange(addr, &chat(OPENAI, "replay me")),
        &openai("replay me"),
    );
    stop.raise().unwrap();
    let (result, tap) = looped.join().unwrap();
    result.unwrap();
    let answers = tap
        .iterations
        .iter()
        .flat_map(|it| it.answers.iter().flatten());
    let decisions = answers.filter(|c| matches!(c, Comp::Decision { .. }));
    assert_eq!(decisions.count(), 1, "the rule's answer is a tapped input");

    let mut provider = Provider::new(plan);
    let mut io = ServerIo::new(Server::new(Ids::new(), Config::default()), stop_id);
    let replayed = replay(&mut provider, &mut io, &tap);
    assert!(replayed == tap.outputs(), "replay diverged");
    assert!(io.idle());
    let records: Vec<_> = replayed
        .iter()
        .flat_map(|out| &out.host_reqs)
        .filter_map(|req| match req {
            HostReq::Record(record) => Some(record.clone()),
            HostReq::Decide { .. } => None,
        })
        .collect();
    assert_eq!(records, *log.lock().unwrap());
}
