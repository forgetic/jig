//! Scripted-completion tests for the provider core: no threads, sockets or
//! clocks. Behaviour tests compare against `render_action` for the action they
//! expect; the golden tests pin `render_action`'s bytes to the async server's.

use jig_core::{
    AbortStream, ErrorBody, Reply, Script, ScriptFile, StopReason, StreamError, Turn, Usage,
};
use serde_json::json;

use super::*;

fn post(target: &str, body: serde_json::Value) -> Request {
    Request {
        method: "POST".to_string(),
        target: target.to_string(),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body: serde_json::to_vec(&body).expect("serialize request body"),
    }
}

/// An OpenAI chat request whose only message is `content`.
fn chat(content: &str) -> Request {
    post(
        "/chat/completions",
        json!({ "model": "m", "messages": [{ "role": "user", "content": content }] }),
    )
}

fn arrive(req: u64, request: Request) -> Comp {
    Comp::Request {
        req: ReqId(req),
        request,
    }
}

fn provider_for(script: Script) -> Provider {
    let (plan, rule) = script.split();
    assert!(rule.is_none(), "data scripts need no rule");
    Provider::new(plan)
}

/// One step with `comps`; returns what it emitted.
fn step(provider: &mut Provider, comps: Vec<Comp>) -> (Vec<IoReq>, Vec<HostReq>) {
    let mut comps = comps;
    let (mut io, mut host) = (Vec::new(), Vec::new());
    provider.step(Time::ZERO, &mut comps, &mut io, &mut host);
    assert!(comps.is_empty(), "the step drains its input");
    (io, host)
}

/// Serve one request from a data plan: exactly one record and one response.
fn serve(provider: &mut Provider, req: u64, request: Request) -> Response {
    let (io, host) = step(provider, vec![arrive(req, request)]);
    assert!(
        matches!(host.as_slice(), [HostReq::Record(_)]),
        "expected one record, got {host:?}"
    );
    match io.as_slice() {
        [IoReq::Respond { req: r, response }] if *r == ReqId(req) => response.clone(),
        other => panic!("expected one response for {req}, got {other:?}"),
    }
}

fn reply(dialect: Dialect, text: &str) -> Response {
    render_action(dialect, ScriptAction::Reply(Reply::text(text)))
}

fn records(host: &[HostReq]) -> Vec<&RecordedRequest> {
    host.iter()
        .filter_map(|request| match request {
            HostReq::Record(recorded) => Some(recorded),
            HostReq::Decide { .. } => None,
        })
        .collect()
}

fn tool_call_reply() -> Reply {
    Reply {
        turns: vec![Turn::ToolCall {
            id: "call_1".to_string(),
            name: "write".to_string(),
            args: json!({ "path": "out.txt" }),
        }],
        usage: Usage::default(),
        stop: StopReason::ToolCalls,
    }
}

// ---------------------------------------------------------------- script kinds

#[test]
fn fixed_scripts_serve_the_same_action_every_time() {
    let mut provider = provider_for(Script::Fixed(Reply::text("same")));
    for req in 0..3 {
        assert_eq!(
            serve(&mut provider, req, chat("hi")),
            reply(Dialect::OpenAi, "same")
        );
    }

    let error = HttpError::provider(500, "server_error", "down");
    let mut provider = provider_for(Script::fixed_action(error.clone()));
    for req in 0..2 {
        assert_eq!(
            serve(&mut provider, req, chat("hi")),
            render_action(Dialect::OpenAi, ScriptAction::HttpError(error.clone()))
        );
    }
}

#[test]
fn sequences_serve_in_order_then_repeat_the_last() {
    let mut provider = provider_for(Script::sequence(vec![
        Reply::text("first"),
        Reply::text("second"),
    ]));
    let served: Vec<Response> = (0..4)
        .map(|req| serve(&mut provider, req, chat("hi")))
        .collect();
    assert_eq!(
        served,
        vec![
            reply(Dialect::OpenAi, "first"),
            reply(Dialect::OpenAi, "second"),
            reply(Dialect::OpenAi, "second"),
            reply(Dialect::OpenAi, "second"),
        ]
    );
}

#[test]
fn phases_keep_a_cursor_per_phase() {
    let script = ScriptFile::from_json_str(
        r#"{ "phases": [
              { "name": "architect", "when": { "messages_contain": ["ROLE: architect"] },
                "sequence": [ { "text": "a1" }, { "text": "a2" } ] },
              { "name": "engineer", "when": { "messages_contain": ["ROLE: engineer"] },
                "sequence": [ { "text": "e1" }, { "text": "e2" } ] }
            ] }"#,
    )
    .expect("phases parse")
    .into_script();
    let mut provider = provider_for(script);

    let mut next = |req, content| serve(&mut provider, req, chat(content));
    assert_eq!(next(0, "ROLE: architect"), reply(Dialect::OpenAi, "a1"));
    assert_eq!(next(1, "ROLE: engineer"), reply(Dialect::OpenAi, "e1"));
    assert_eq!(next(2, "ROLE: engineer"), reply(Dialect::OpenAi, "e2"));
    // The engineer's turns did not move the architect's cursor.
    assert_eq!(next(3, "ROLE: architect"), reply(Dialect::OpenAi, "a2"));
    // No phase matches: an empty text reply.
    assert_eq!(next(4, "ROLE: reviewer"), reply(Dialect::OpenAi, ""));
}

#[test]
fn reference_delivery_is_served_from_the_plan() {
    let file =
        ScriptFile::from_json_str(r#"{ "reference_delivery": { "greeting_file": "G.md" } }"#)
            .expect("reference delivery parses");
    let spec = match &file {
        ScriptFile::ReferenceDelivery(spec) => spec.clone(),
        other => panic!("expected reference delivery, got {other:?}"),
    };
    let mut provider = provider_for(file.into_script());

    let request = chat(
        "ROLE: engineer\n- org/widget (dir: widget-dir/, access: writable, default branch: main)",
    );
    let view = parse_openai(&request.body);
    let expected = spec.reply(&view);
    assert_eq!(
        expected.stop,
        StopReason::ToolCalls,
        "the engineer writes first"
    );
    assert_eq!(
        serve(&mut provider, 0, request),
        render_action(Dialect::OpenAi, ScriptAction::Reply(expected))
    );
}

// ------------------------------------------------------------------- decisions

#[test]
fn rule_decisions_round_trip_through_the_host() {
    let (plan, rule) = Script::rule(|view| {
        if view.prior_tool_results == 0 {
            tool_call_reply()
        } else {
            Reply::text("done")
        }
    })
    .split();
    assert_eq!(plan, Plan::External);
    let mut rule = rule.expect("a rule script yields its rule");
    let mut provider = Provider::new(plan);

    // Two requests on different dialects are undecided at once.
    let anthropic = post(
        "/v1/messages",
        json!({ "model": "claude", "messages": [{ "role": "user", "content": "go" }] }),
    );
    let (io, host) = step(
        &mut provider,
        vec![arrive(1, chat("go")), arrive(2, anthropic)],
    );
    assert!(io.is_empty(), "nothing is answered before the host decides");
    let decisions: Vec<(ReqId, RequestView)> = host
        .into_iter()
        .filter_map(|request| match request {
            HostReq::Decide { req, view } => Some((req, view)),
            HostReq::Record(_) => None,
        })
        .collect();
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0].0, ReqId(1));
    assert_eq!(decisions[0].1.dialect, Dialect::OpenAi);
    assert_eq!(decisions[1].0, ReqId(2));
    assert_eq!(decisions[1].1.dialect, Dialect::Anthropic);

    // Answer out of order: each response uses its own request's dialect.
    let answers = decisions
        .iter()
        .rev()
        .map(|(req, view)| Comp::Decision {
            req: *req,
            action: rule.decide(view),
        })
        .collect();
    let (io, host) = step(&mut provider, answers);
    assert!(host.is_empty());
    let action = ScriptAction::Reply(tool_call_reply());
    assert_eq!(
        io,
        vec![
            IoReq::Respond {
                req: ReqId(2),
                response: render_action(Dialect::Anthropic, action.clone()),
            },
            IoReq::Respond {
                req: ReqId(1),
                response: render_action(Dialect::OpenAi, action),
            },
        ]
    );
}

#[test]
fn a_decision_for_an_unknown_request_is_ignored() {
    let (plan, _rule) = Script::rule(|_| Reply::text("ruled")).split();
    let mut provider = Provider::new(plan);
    let decide = |req| Comp::Decision {
        req: ReqId(req),
        action: ScriptAction::Reply(Reply::text("late")),
    };

    // Never asked about.
    assert_eq!(step(&mut provider, vec![decide(7)]), (vec![], vec![]));

    // Answered once; the repeat is as unknown as a stranger.
    step(&mut provider, vec![arrive(1, chat("hi"))]);
    let (io, _) = step(&mut provider, vec![decide(1)]);
    assert_eq!(io.len(), 1);
    assert_eq!(step(&mut provider, vec![decide(1)]), (vec![], vec![]));
}

// ------------------------------------------------------------ routing and views

#[test]
fn unknown_paths_get_404_without_consuming_the_script() {
    let mut provider = provider_for(Script::sequence(vec![
        Reply::text("first"),
        Reply::text("second"),
    ]));
    let get = Request {
        method: "GET".to_string(),
        target: "/nope?x=1".to_string(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let (io, host) = step(&mut provider, vec![arrive(0, get)]);
    assert_eq!(
        io,
        vec![IoReq::Respond {
            req: ReqId(0),
            response: not_found(),
        }]
    );
    assert_eq!(
        host,
        vec![HostReq::Record(RecordedRequest {
            path: "/nope".to_string(),
            method: "GET".to_string(),
            body: Vec::new(),
            view: None,
        })]
    );
    assert_eq!(
        serve(&mut provider, 1, chat("hi")),
        reply(Dialect::OpenAi, "first")
    );
}

#[test]
fn each_dialect_route_projects_its_view_and_query_strings_are_stripped() {
    let body = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] });
    let codex = json!({
        "model": "m",
        "instructions": "be terse",
        "input": [{ "type": "message", "role": "user",
                    "content": [{ "type": "input_text", "text": "hi" }] }],
    });
    let cases = [
        (
            "/chat/completions?stream=1",
            "/chat/completions",
            Dialect::OpenAi,
            body.clone(),
        ),
        (
            "/v1/messages?beta=true",
            "/v1/messages",
            Dialect::Anthropic,
            body,
        ),
        (
            "/backend-api/codex/responses",
            "/backend-api/codex/responses",
            Dialect::Codex,
            codex,
        ),
    ];
    for (target, path, dialect, body) in cases {
        let mut provider = provider_for(Script::Fixed(Reply::text("ok")));
        let request = post(target, body);
        let raw = request.body.clone();
        let (io, host) = step(&mut provider, vec![arrive(0, request)]);

        assert_eq!(
            host,
            vec![HostReq::Record(RecordedRequest {
                path: path.to_string(),
                method: "POST".to_string(),
                body: raw.clone(),
                view: Some(project(dialect, &raw)),
            })],
            "{target}"
        );
        assert_eq!(
            io,
            vec![IoReq::Respond {
                req: ReqId(0),
                response: reply(dialect, "ok"),
            }],
            "{target}"
        );
    }
}

#[test]
fn an_unparseable_body_still_gets_a_sparse_view_and_a_reply() {
    let mut provider = provider_for(Script::Fixed(Reply::text("ok")));
    let request = Request {
        method: "POST".to_string(),
        target: "/v1/messages".to_string(),
        headers: Vec::new(),
        body: b"not json".to_vec(),
    };
    let (io, host) = step(&mut provider, vec![arrive(0, request)]);
    let recorded = records(&host);
    assert_eq!(
        recorded[0].view,
        Some(RequestView::new(Dialect::Anthropic, None, Vec::new(), 0))
    );
    assert_eq!(
        io,
        vec![IoReq::Respond {
            req: ReqId(0),
            response: reply(Dialect::Anthropic, "ok"),
        }]
    );
}

#[test]
fn the_record_comes_before_the_response() {
    // Host requests are handled before the I/O step runs (§5.3), so a record
    // emitted no later than its response is in the log first.
    let mut provider = provider_for(Script::Fixed(Reply::text("ok")));
    let (io, host) = step(&mut provider, vec![arrive(0, chat("hi"))]);
    assert_eq!(records(&host).len(), 1);
    assert_eq!(io.len(), 1);

    // With a rule, the record precedes the question, and the response waits
    // for the answer.
    let (plan, _rule) = Script::rule(|_| Reply::text("ok")).split();
    let mut provider = Provider::new(plan);
    let (io, host) = step(&mut provider, vec![arrive(0, chat("hi"))]);
    assert!(io.is_empty());
    assert!(matches!(
        host.as_slice(),
        [HostReq::Record(_), HostReq::Decide { .. }]
    ));
}

// -------------------------------------------------------------------- rendering

#[test]
fn http_errors_take_the_content_type_override_and_drop_framing_headers() {
    let error = HttpError {
        status: 429,
        body: ErrorBody::Raw {
            content_type: "text/plain".to_string(),
            body: "slow down".to_string(),
        },
        headers: [
            ("Retry-After", "7"),
            ("content-type", "application/problem+json"),
            ("Content-Type", "second/ignored"),
            ("CONTENT-LENGTH", "999"),
            ("connection", "keep-alive"),
            ("x-request-id", "req_9"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .to_vec(),
    };
    let response = render_action(Dialect::OpenAi, ScriptAction::HttpError(error));
    assert_eq!(response.status, 429);
    assert_eq!(response.reason, "Too Many Requests");
    assert_eq!(
        response.headers,
        header_list(&[
            ("Content-Type", "application/problem+json"),
            ("Content-Length", "9"),
            ("Connection", "close"),
            ("Retry-After", "7"),
            ("x-request-id", "req_9"),
        ])
    );
    assert_eq!(response.body, Body::Full(b"slow down".to_vec()));
}

#[test]
fn http_error_reason_phrases_follow_the_async_server() {
    let cases = [
        (400, "Bad Request"),
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (404, "Not Found"),
        (408, "Request Timeout"),
        (409, "Conflict"),
        (429, "Too Many Requests"),
        (500, "Internal Server Error"),
        (501, "Not Implemented"),
        (502, "Bad Gateway"),
        (503, "Service Unavailable"),
        (504, "Gateway Timeout"),
        (418, "Error"),
        (599, "Error"),
    ];
    for (status, reason) in cases {
        let error = HttpError::raw(status, "text/plain", "x");
        let response = render_action(Dialect::OpenAi, ScriptAction::HttpError(error));
        assert_eq!(
            (response.status, response.reason.as_str()),
            (status, reason)
        );
    }
}

#[test]
fn stream_actions_get_501_in_the_route_dialect() {
    let unsupported = HttpError::provider(
        501,
        "unsupported_script_action",
        "script action is not implemented by jig-server yet",
    );
    let actions = [
        ScriptAction::StreamError(StreamError {
            body: ErrorBody::provider("overloaded", "busy"),
        }),
        ScriptAction::AbortStream(AbortStream {
            reply: Reply::text("cut"),
            after_frames: Some(1),
            after_bytes: None,
        }),
    ];
    for action in actions {
        for dialect in [Dialect::OpenAi, Dialect::Anthropic, Dialect::Codex] {
            assert_eq!(
                render_action(dialect, action.clone()),
                render_action(dialect, ScriptAction::HttpError(unsupported.clone())),
            );
        }
    }
    let response = render_action(
        Dialect::Anthropic,
        ScriptAction::AbortStream(AbortStream {
            reply: Reply::text("cut"),
            after_frames: None,
            after_bytes: None,
        }),
    );
    assert_eq!(
        (response.status, response.reason.as_str()),
        (501, "Not Implemented")
    );
    let Body::Full(body) = response.body else {
        panic!("errors have a Content-Length body");
    };
    let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON error body");
    assert_eq!(body["error"]["type"], "unsupported_script_action");
}

// --------------------------------------------------------------------- shutdown

#[test]
fn stop_issues_one_shutdown_with_the_grace() {
    let mut provider = provider_for(Script::Fixed(Reply::text("ok")));
    assert!(!provider.done());
    let (io, host) = step(&mut provider, vec![Comp::Stop]);
    assert_eq!(
        io,
        vec![IoReq::Shutdown {
            grace: Duration::from_secs(1),
        }]
    );
    assert!(host.is_empty());
    assert!(provider.done());
    assert_eq!(provider.deadline(), None);

    // A second stop changes nothing.
    assert_eq!(step(&mut provider, vec![Comp::Stop]), (vec![], vec![]));

    let config = ProviderConfig {
        grace: Duration::from_millis(250),
    };
    let (plan, _) = Script::Fixed(Reply::text("ok")).split();
    let mut provider = Provider::with_config(plan, config);
    let (io, _) = step(&mut provider, vec![Comp::Stop]);
    assert_eq!(
        io,
        vec![IoReq::Shutdown {
            grace: config.grace
        }]
    );
}

#[test]
fn done_waits_for_undecided_requests() {
    let (plan, _rule) = Script::rule(|_| Reply::text("ok")).split();
    let mut provider = Provider::new(plan);
    step(&mut provider, vec![arrive(3, chat("hi")), Comp::Stop]);
    assert!(!provider.done(), "a decision is still owed");

    let (io, _) = step(
        &mut provider,
        vec![Comp::Decision {
            req: ReqId(3),
            action: ScriptAction::Reply(Reply::text("ok")),
        }],
    );
    assert_eq!(io.len(), 1);
    assert!(provider.done());
}

#[test]
fn a_gone_request_is_owed_no_decision() {
    let (plan, _rule) = Script::rule(|_| Reply::text("ok")).split();
    let mut provider = Provider::new(plan);
    step(&mut provider, vec![arrive(3, chat("hi")), Comp::Stop]);
    assert!(!provider.done());

    // The host never answered, and the grace ran out.
    let gone = Comp::Gone { req: ReqId(3) };
    assert_eq!(step(&mut provider, vec![gone.clone()]), (vec![], vec![]));
    assert!(provider.done(), "nothing is owed any more");
    // A late decision, or a second Gone, finds nothing.
    let late = Comp::Decision {
        req: ReqId(3),
        action: ScriptAction::Reply(Reply::text("late")),
    };
    assert_eq!(step(&mut provider, vec![late, gone]), (vec![], vec![]));
}

#[test]
fn requests_after_stop_are_still_answered() {
    let mut provider = provider_for(Script::Fixed(Reply::text("ok")));
    step(&mut provider, vec![Comp::Stop]);
    assert_eq!(
        serve(&mut provider, 0, chat("hi")),
        reply(Dialect::OpenAi, "ok")
    );
}

// ---------------------------------------------------------------- serve_request

#[test]
fn serve_request_answers_from_a_data_plan() {
    let mut provider = provider_for(Script::sequence(vec![
        Reply::text("one"),
        Reply::text("two"),
    ]));
    let request = chat("hi");
    let body = request.body.clone();
    let (response, recorded) = serve_request(&mut provider, None, request);
    assert_eq!(response, reply(Dialect::OpenAi, "one"));
    assert_eq!(recorded.path, "/chat/completions");
    assert_eq!(recorded.body, body);
    assert_eq!(recorded.view, Some(parse_openai(&body)));

    let (response, _) = serve_request(&mut provider, None, chat("hi"));
    assert_eq!(response, reply(Dialect::OpenAi, "two"));

    let get = Request {
        method: "GET".to_string(),
        target: "/nope".to_string(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let (response, recorded) = serve_request(&mut provider, None, get);
    assert_eq!(response, not_found());
    assert_eq!(recorded.view, None);
}

#[test]
fn serve_request_asks_the_rule() {
    let mut turns = 0;
    let (plan, mut rule) = Script::rule(move |_| {
        turns += 1;
        Reply::text(format!("turn {turns}"))
    })
    .split();
    let mut provider = Provider::new(plan);
    for turn in 1..=2 {
        let (response, _) = serve_request(&mut provider, rule.as_mut(), chat("hi"));
        assert_eq!(response, reply(Dialect::OpenAi, &format!("turn {turn}")));
    }
}

#[test]
fn serve_request_does_not_collide_with_an_undecided_request() {
    let (plan, mut rule) = Script::rule(|_| Reply::text("ok")).split();
    let mut provider = Provider::new(plan);
    step(&mut provider, vec![arrive(0, chat("pending"))]);

    let (response, _) = serve_request(&mut provider, rule.as_mut(), chat("now"));
    assert_eq!(response, reply(Dialect::OpenAi, "ok"));

    // The earlier request is still owed its answer, and still gets it.
    let (io, _) = step(
        &mut provider,
        vec![Comp::Decision {
            req: ReqId(0),
            action: ScriptAction::Reply(Reply::text("late")),
        }],
    );
    assert_eq!(
        io,
        vec![IoReq::Respond {
            req: ReqId(0),
            response: reply(Dialect::OpenAi, "late"),
        }]
    );
}

#[test]
#[should_panic(expected = "no rule was given")]
fn serve_request_without_a_rule_for_an_external_plan_panics() {
    let (plan, _rule) = Script::rule(|_| Reply::text("ok")).split();
    serve_request(&mut Provider::new(plan), None, chat("hi"));
}

// ------------------------------------------------------------------ golden bytes

/// Serialize per the §4.6 contract: the head verbatim, each `Chunked` part as
/// one chunk, then the terminator.
fn wire(response: &Response) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", response.status, response.reason).into_bytes();
    for (name, value) in &response.headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    match &response.body {
        Body::Empty => {}
        Body::Full(bytes) => out.extend_from_slice(bytes),
        Body::Chunked(parts) => {
            for part in parts {
                out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                out.extend_from_slice(part);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n\r\n");
        }
    }
    out
}

/// What `write_sse_response` at `ca1edfd` writes for `body`.
fn async_server_sse(body: &str) -> Vec<u8> {
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

/// What `write_http_error` at `ca1edfd` writes when no extra headers survive.
fn async_server_error(status_line: &str, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status_line}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

fn assert_bytes(actual: Vec<u8>, expected: Vec<u8>) {
    assert_eq!(
        String::from_utf8_lossy(&actual),
        String::from_utf8_lossy(&expected)
    );
    assert_eq!(actual, expected);
}

#[test]
fn sse_bytes_match_the_async_server() {
    let replies = [
        Reply::text("héllo ✓"),
        tool_call_reply(),
        Reply {
            turns: vec![
                Turn::Thinking("hmm".to_string()),
                Turn::Text("done".to_string()),
            ],
            usage: Usage {
                prompt_tokens: 7,
                completion_tokens: 9,
            },
            stop: StopReason::Stop,
        },
    ];
    for reply in replies {
        let cases = [
            (Dialect::OpenAi, frames_to_body(&render_openai(&reply))),
            (
                Dialect::Anthropic,
                frames_to_body(&render_anthropic(&reply)),
            ),
            (Dialect::Codex, frames_to_body(&render_codex(&reply))),
        ];
        for (dialect, body) in cases {
            let response = render_action(dialect, ScriptAction::Reply(reply.clone()));
            assert_bytes(wire(&response), async_server_sse(&body));
        }
    }
}

#[test]
fn http_error_bytes_match_the_async_server() {
    // Raw body, with the override, dropped framing headers and ordered extras.
    let error = HttpError {
        status: 502,
        body: ErrorBody::Raw {
            content_type: "text/plain".to_string(),
            body: "bad gateway".to_string(),
        },
        headers: [
            ("x-jig-test", "raw-error"),
            ("Content-Type", "text/x-override"),
            ("Connection", "keep-alive"),
            ("content-length", "1"),
            ("Retry-After", "3"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .to_vec(),
    };
    let response = render_action(Dialect::OpenAi, ScriptAction::HttpError(error));
    assert_bytes(
        wire(&response),
        b"HTTP/1.1 502 Bad Gateway\r\n\
          Content-Type: text/x-override\r\n\
          Content-Length: 11\r\n\
          Connection: close\r\n\
          x-jig-test: raw-error\r\n\
          Retry-After: 3\r\n\
          \r\n\
          bad gateway"
            .to_vec(),
    );

    // Provider-shaped bodies in each route dialect, and an unlisted status.
    for (dialect, status, status_line) in [
        (Dialect::OpenAi, 500, "500 Internal Server Error"),
        (Dialect::Anthropic, 529, "529 Error"),
        (Dialect::Codex, 429, "429 Too Many Requests"),
    ] {
        let error = HttpError::provider(status, "server_error", "temporary");
        let body = error.render_body(dialect).body;
        let response = render_action(dialect, ScriptAction::HttpError(error));
        assert_bytes(
            wire(&response),
            async_server_error(status_line, "application/json", &body),
        );
    }
}

#[test]
fn unsupported_action_bytes_match_the_async_server() {
    let response = render_action(
        Dialect::OpenAi,
        ScriptAction::StreamError(StreamError {
            body: ErrorBody::provider("overloaded", "busy"),
        }),
    );
    assert_bytes(
        wire(&response),
        async_server_error(
            "501 Not Implemented",
            "application/json",
            r#"{"error":{"code":"unsupported_script_action","message":"script action is not implemented by jig-server yet"}}"#,
        ),
    );
}

#[test]
fn not_found_bytes_match_the_async_server() {
    assert_bytes(
        wire(&not_found()),
        b"HTTP/1.1 404 Not Found\r\n\
          Content-Length: 0\r\n\
          Connection: close\r\n\
          \r\n"
            .to_vec(),
    );
}
