//! Scripted-completion tests for the recorder core: no threads, sockets or
//! clocks. Each feeds completions to a step and checks the requests it makes.

use std::net::SocketAddr;

use steploop::http1::client::ClientError;
use steploop::http1::codec::write_response;

use super::*;

const REQ: ReqId = ReqId(7);
const FETCH: FetchId = FetchId(7);
const HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";

fn chat() -> Request {
    Request {
        method: "POST".to_owned(),
        target: "/chat/completions?stream=true".to_owned(),
        headers: vec![
            ("Host".to_owned(), "127.0.0.1:5050".to_owned()),
            ("Authorization".to_owned(), "Bearer sk-x".to_owned()),
            ("Accept-Encoding".to_owned(), "gzip".to_owned()),
            ("Content-Length".to_owned(), "2".to_owned()),
        ],
        body: b"{}".to_vec(),
    }
}

fn arrive(req: ReqId, request: Request) -> Comp {
    Comp::Server(ServerEvent::Request { req, request })
}

fn head(fetch: FetchId) -> Comp {
    Comp::Client(ClientEvent::Head {
        fetch,
        status: 200,
        headers: vec![
            ("Content-Type".to_owned(), "text/event-stream".to_owned()),
            ("Transfer-Encoding".to_owned(), "chunked".to_owned()),
        ],
        raw: HEAD.to_vec(),
    })
}

fn body(fetch: FetchId, bytes: &[u8]) -> Comp {
    Comp::Client(ClientEvent::Body {
        fetch,
        bytes: bytes.to_vec(),
    })
}

fn end(fetch: FetchId) -> Comp {
    Comp::Client(ClientEvent::End { fetch })
}

fn failed(fetch: FetchId) -> Comp {
    let error = ClientError::Tls("invalid peer certificate: UnknownIssuer".to_owned());
    Comp::Client(ClientEvent::Failed { fetch, error })
}

fn flushed(req: ReqId) -> Comp {
    Comp::Server(ServerEvent::Flushed { req })
}

fn gone(req: ReqId) -> Comp {
    Comp::Server(ServerEvent::Gone { req })
}

fn server(cmd: ServerCmd) -> IoReq {
    IoReq::Server(cmd)
}

fn client(cmd: ClientCmd) -> IoReq {
    IoReq::Client(cmd)
}

fn shutdown() -> IoReq {
    server(ServerCmd::Shutdown { grace: GRACE })
}

fn core(mode: Mode) -> RecorderCore {
    RecorderCore::new(mode, None, None)
}

/// One step with `comps`; returns what it asked for.
fn step(core: &mut RecorderCore, comps: Vec<Comp>) -> (Vec<IoReq>, Vec<HostReq>) {
    let mut comps = comps;
    let (mut io, mut host) = (Vec::new(), Vec::new());
    core.step(Time::ZERO, &mut comps, &mut io, &mut host);
    assert!(comps.is_empty(), "the step drains its input");
    (io, host)
}

/// A core with `chat()` fetched and its head relayed.
fn streaming(mode: Mode) -> RecorderCore {
    let mut core = core(mode);
    step(&mut core, vec![arrive(REQ, chat()), head(FETCH)]);
    core
}

fn captured(host: &[HostReq]) -> Vec<&Exchange> {
    host.iter()
        .filter_map(|h| match h {
            HostReq::Captured(exchange) => Some(exchange),
            HostReq::Log(_) => None,
        })
        .collect()
}

#[test]
fn a_preflight_gets_the_exact_204() {
    let mut core = core(Mode::Once);
    let preflight = Request {
        method: "HEAD".to_owned(),
        target: "/".to_owned(),
        headers: vec![("Host".to_owned(), "127.0.0.1".to_owned())],
        body: Vec::new(),
    };
    let (io, host) = step(&mut core, vec![arrive(REQ, preflight)]);
    let [IoReq::Server(ServerCmd::Respond { req, response })] = &io[..] else {
        panic!("{io:?}");
    };
    assert_eq!(*req, REQ);
    assert_eq!(
        write_response(response),
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert_eq!(host, []);
    assert!(!core.done(), "a preflight doesn't count as the recording");
}

#[test]
fn a_routable_request_is_fetched_with_the_rewritten_head() {
    let mut core = core(Mode::Once);
    let (io, host) = step(&mut core, vec![arrive(REQ, chat())]);
    let head = "POST /chat/completions?stream=true HTTP/1.1\r\n\
                Authorization: Bearer sk-x\r\n\
                Accept-Encoding: identity\r\n\
                Content-Length: 2\r\n\
                Host: api.openai.com\r\n\
                Connection: close\r\n\r\n";
    let target = Target {
        host: "api.openai.com".to_owned(),
        port: 443,
        tls: true,
        addr: None,
    };
    let fetch = client(ClientCmd::Fetch {
        fetch: FETCH,
        target,
        head: head.as_bytes().to_vec(),
        body: b"{}".to_vec(),
    });
    assert_eq!(io, [fetch]);
    assert_eq!(host, []);
}

#[test]
fn the_host_override_and_the_upstream_hook_shape_the_target() {
    let addr: SocketAddr = "127.0.0.1:4443".parse().unwrap();
    let upstream = UpstreamOverride { addr, tls: false };
    let mut core = RecorderCore::new(Mode::Once, Some("api.deepseek.com".into()), Some(upstream));
    let (io, _) = step(&mut core, vec![arrive(REQ, chat())]);
    let [IoReq::Client(ClientCmd::Fetch { target, head, .. })] = &io[..] else {
        panic!("{io:?}");
    };
    let want = Target {
        host: "api.deepseek.com".to_owned(),
        port: 443,
        tls: false,
        addr: Some(addr),
    };
    assert_eq!(*target, want);
    let head = String::from_utf8_lossy(head);
    assert!(head.contains("\r\nHost: api.deepseek.com\r\n"), "{head}");
}

#[test]
fn the_response_is_relayed_raw_and_each_body_acked_on_the_next_flush() {
    let mut core = core(Mode::Once);
    let (io, _) = step(&mut core, vec![arrive(REQ, chat()), head(FETCH)]);
    assert_eq!(
        io[1..],
        [
            server(ServerCmd::RawStart { req: REQ }),
            server(ServerCmd::RawBytes {
                req: REQ,
                bytes: HEAD.to_vec()
            }),
        ]
    );
    // The head's flush owes the upstream nothing.
    assert_eq!(step(&mut core, vec![flushed(REQ)]).0, []);

    for chunk in [&b"5\r\nhello\r\n"[..], b"0\r\n\r\n"] {
        let (io, _) = step(&mut core, vec![body(FETCH, chunk)]);
        let bytes = chunk.to_vec();
        assert_eq!(io, [server(ServerCmd::RawBytes { req: REQ, bytes })]);
        let (io, _) = step(&mut core, vec![flushed(REQ)]);
        assert_eq!(io, [client(ClientCmd::Ack { fetch: FETCH })]);
        assert_eq!(step(&mut core, vec![flushed(REQ)]).0, [], "one ack each");
    }
}

#[test]
fn the_end_captures_the_exchange_with_the_raw_body() {
    let mut core = streaming(Mode::Once);
    step(
        &mut core,
        vec![body(FETCH, b"5\r\nhello\r\n"), flushed(REQ)],
    );
    // `End` may come before the last body's ack.
    let (io, host) = step(&mut core, vec![body(FETCH, b"0\r\n\r\n"), end(FETCH)]);
    assert_eq!(
        io[1..],
        [server(ServerCmd::RawEnd { req: REQ }), shutdown()]
    );

    let request = ClientRequest {
        method: "POST".to_owned(),
        target: "/chat/completions?stream=true".to_owned(),
        headers: chat()
            .headers
            .into_iter()
            .map(|(n, v)| Header::new(n, v))
            .collect(),
        body: b"{}".to_vec(),
    };
    let response = UpstreamResponse {
        status: 200,
        headers: vec![
            Header::new("Content-Type", "text/event-stream"),
            Header::new("Transfer-Encoding", "chunked"),
        ],
        body: b"5\r\nhello\r\n0\r\n\r\n".to_vec(),
    };
    let route = Route::resolve("/chat/completions").unwrap();
    let line = "captured exchange #0 POST /chat/completions -> 200 (15 body bytes)";
    assert_eq!(
        host,
        [
            HostReq::Log(line.to_owned()),
            HostReq::Captured((request, response, route)),
        ]
    );
}

#[test]
fn a_failure_before_the_head_closes_the_client_without_a_response() {
    let mut core = core(Mode::Pump);
    step(&mut core, vec![arrive(REQ, chat())]);
    let (io, host) = step(&mut core, vec![failed(FETCH)]);
    assert_eq!(
        io,
        [
            server(ServerCmd::RawStart { req: REQ }),
            server(ServerCmd::RawEnd { req: REQ }),
        ]
    );
    let why = "connection error: upstream TLS failed: invalid peer certificate: UnknownIssuer";
    assert_eq!(host, [HostReq::Log(why.to_owned())]);
}

#[test]
fn a_failure_mid_body_ends_the_relay_truncated_without_a_capture() {
    let mut core = streaming(Mode::Pump);
    step(&mut core, vec![body(FETCH, b"5\r\nhel")]);
    let (io, host) = step(&mut core, vec![failed(FETCH)]);
    assert_eq!(io, [server(ServerCmd::RawEnd { req: REQ })]);
    assert_eq!(captured(&host), Vec::<&Exchange>::new());
}

#[test]
fn a_client_gone_mid_relay_cancels_the_fetch_without_a_capture() {
    let mut core = streaming(Mode::Pump);
    step(&mut core, vec![body(FETCH, b"5\r\nhello\r\n")]);
    let (io, host) = step(&mut core, vec![gone(REQ)]);
    assert_eq!(io, [client(ClientCmd::Cancel { fetch: FETCH })]);
    assert_eq!(captured(&host), Vec::<&Exchange>::new());
    // The cancellation's own `Failed`, and anything else late, is ignored.
    let cancelled = Comp::Client(ClientEvent::Failed {
        fetch: FETCH,
        error: ClientError::Cancelled,
    });
    assert_eq!(
        step(&mut core, vec![cancelled, end(FETCH)]),
        (vec![], vec![])
    );
}

#[test]
fn once_mode_shuts_down_after_its_capture_and_captures_no_more() {
    let mut core = core(Mode::Once);
    let other = ReqId(9);
    step(&mut core, vec![arrive(REQ, chat()), arrive(other, chat())]);
    step(&mut core, vec![head(FETCH), head(FetchId(9))]);
    let (io, host) = step(&mut core, vec![end(FETCH)]);
    assert_eq!(io.last(), Some(&shutdown()));
    assert_eq!(captured(&host).len(), 1);
    assert!(!core.done(), "the other relay may finish in the grace");
    // It does, and is relayed to its client, but not captured.
    let (io, host) = step(&mut core, vec![end(FetchId(9))]);
    assert_eq!(io, [server(ServerCmd::RawEnd { req: other })]);
    assert_eq!(host, []);
    assert!(core.done());
}

#[test]
fn once_mode_ends_without_a_capture_when_its_exchange_fails() {
    let mut core = core(Mode::Once);
    step(&mut core, vec![arrive(REQ, chat())]);
    let (io, host) = step(&mut core, vec![failed(FETCH)]);
    assert_eq!(io.last(), Some(&shutdown()));
    assert_eq!(captured(&host), Vec::<&Exchange>::new());
    assert!(core.done());
}

#[test]
fn pump_mode_captures_every_exchange_until_stopped() {
    let mut core = core(Mode::Pump);
    for (n, req) in [3, 5].into_iter().enumerate() {
        let fetch = FetchId(req);
        let req = ReqId(req);
        step(&mut core, vec![arrive(req, chat()), head(fetch)]);
        step(&mut core, vec![body(fetch, b"0\r\n\r\n")]);
        let (io, host) = step(&mut core, vec![end(fetch)]);
        assert_eq!(io, [server(ServerCmd::RawEnd { req })], "no shutdown");
        let line = format!("captured exchange #{n} POST /chat/completions -> 200 (5 body bytes)");
        assert_eq!(host[0], HostReq::Log(line));
        assert_eq!(captured(&host).len(), 1);
    }
    assert!(!core.done());
    let (io, _) = step(&mut core, vec![Comp::Stop]);
    assert_eq!(io, [shutdown()]);
    assert!(core.done());
}

#[test]
fn stop_shuts_down_once_and_lets_a_relay_in_flight_finish() {
    let mut core = streaming(Mode::Pump);
    let (io, _) = step(&mut core, vec![Comp::Stop, Comp::Stop]);
    assert_eq!(io, [shutdown()]);
    assert!(!core.done(), "a relay is in flight");
    let (io, host) = step(&mut core, vec![body(FETCH, b"0\r\n\r\n"), end(FETCH)]);
    assert_eq!(io[1..], [server(ServerCmd::RawEnd { req: REQ })]);
    assert_eq!(
        captured(&host).len(),
        1,
        "finished in the grace, so captured"
    );
    assert!(core.done());

    // Or the grace runs out, and the server says so.
    let mut core = streaming(Mode::Pump);
    step(&mut core, vec![Comp::Stop]);
    let (io, _) = step(&mut core, vec![gone(REQ)]);
    assert_eq!(io, [client(ClientCmd::Cancel { fetch: FETCH })]);
    assert!(core.done());
}

#[test]
fn stale_and_out_of_order_events_are_ignored() {
    let mut core = core(Mode::Once);
    // Nothing is in flight.
    let strays = vec![
        head(FETCH),
        body(FETCH, b"x"),
        end(FETCH),
        failed(FETCH),
        flushed(REQ),
        gone(REQ),
        Comp::Server(ServerEvent::Signal {
            id: steploop::sys::SignalId(1),
        }),
    ];
    assert_eq!(step(&mut core, strays), (vec![], vec![]));

    // A body or an end before the head: the planner never sends them, and
    // the relay is left as it was.
    step(&mut core, vec![arrive(REQ, chat())]);
    assert_eq!(
        step(&mut core, vec![body(FETCH, b"x"), end(FETCH)]),
        (vec![], vec![])
    );
    // A second body before the first one's ack, and a second head.
    step(&mut core, vec![head(FETCH), body(FETCH, b"a")]);
    assert_eq!(
        step(&mut core, vec![body(FETCH, b"b"), head(FETCH)]),
        (vec![], vec![])
    );
    let (_, host) = step(&mut core, vec![end(FETCH)]);
    let [(_, response, _)] = &captured(&host)[..] else {
        panic!("{host:?}");
    };
    assert_eq!(response.body, b"a", "only what was relayed is captured");
}
