//! Synchronous end-to-end test for the OpenAI Codex responses dialect (M4).
//!
//! Mirrors the M1/M3 acceptance tests: a plain `#[test]` (no async runtime of
//! its own) starts a `FakeLlm`, POSTs `/backend-api/codex/responses` with
//! blocking `reqwest`, and asserts the streamed SSE frames parse as a text reply
//! — `event:` lines present, the canonical frame sequence, reassembled text, and
//! the terminal `response.completed` usage. The request projection (top-level
//! `instructions`, prior tool results) is asserted via `fake.requests()`.

use jig_core::{Dialect, Reply, Script, StopReason, Turn, Usage};
use serde_json::Value;

/// One parsed SSE event: the `event:` name and its `data:` JSON payload.
#[derive(Debug)]
struct SseEvent {
    event: String,
    data: Value,
}

/// Parse a `text/event-stream` body into ordered `(event, data-json)` pairs.
///
/// Each event is an `event:` line immediately followed by a `data:` line (Codex
/// always pairs them); blank lines separate events.
fn parse_sse(body: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_event: Option<String> = None;

    for line in body.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            current_event = Some(name.to_string());
        } else if let Some(data) = line.strip_prefix("data: ") {
            let event = current_event
                .take()
                .expect("a Codex data: line must follow an event: line");
            let json: Value = serde_json::from_str(data).expect("data: payload is valid JSON");
            events.push(SseEvent { event, data: json });
        }
    }

    events
}

#[test]
fn codex_stream_parses_as_a_text_reply() {
    let fake = start_with(Script::Fixed(Reply::text("hello from jig")));

    let body = post_responses(
        &fake,
        serde_json::json!({
            "model": "gpt-fake",
            "stream": true,
            "instructions": "be terse",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hi" }],
                },
            ],
        }),
    );
    let events = parse_sse(&body);

    // Every frame carries an event: line, in the canonical order.
    let order: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        order,
        vec!["response.output_text.delta", "response.completed"],
        "unexpected frame order; body was:\n{body}"
    );

    // The output_text deltas reassemble to the scripted text.
    let text: String = events
        .iter()
        .filter(|e| e.event == "response.output_text.delta")
        .filter_map(|e| e.data["delta"].as_str())
        .collect();
    assert_eq!(text, "hello from jig");

    // The terminal response.completed carries token usage.
    let completed = events
        .iter()
        .find(|e| e.event == "response.completed")
        .expect("a response.completed frame");
    assert_eq!(completed.data["type"], "response.completed");
    assert!(completed.data["response"]["usage"]["input_tokens"].is_number());
    assert!(completed.data["response"]["usage"]["output_tokens"].is_number());

    // The request was projected as a Codex view, with the top-level instructions
    // surfaced as a leading system message.
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let recorded = &requests[0];
    assert_eq!(recorded.path, "/backend-api/codex/responses");
    assert_eq!(recorded.method, "POST");
    let view = recorded
        .view
        .as_ref()
        .expect("dialect route projects a view");
    assert_eq!(view.dialect, Dialect::Codex);
    assert_eq!(view.model.as_deref(), Some("gpt-fake"));
    assert_eq!(
        view.messages.first().map(|m| m.role.as_str()),
        Some("system")
    );
    assert_eq!(
        view.messages.first().map(|m| m.content.as_str()),
        Some("be terse")
    );
    assert_eq!(view.last_message().map(|m| m.content.as_str()), Some("hi"));
}

#[test]
fn codex_rule_branches_on_prior_tool_results() {
    // Drive a one-step tool-use loop over the Codex route: with no
    // function_call_output items yet, hand off; once one comes back, return text.
    let script = Script::rule(|view| {
        if view.prior_tool_results == 0 {
            Reply {
                turns: vec![Turn::Text("calling tool".to_string())],
                usage: Usage::default(),
                stop: StopReason::ToolCalls,
            }
        } else {
            Reply::text("all done")
        }
    });
    let fake = start_with(script);

    // Turn 1: no tool results in the transcript yet.
    post_responses(
        &fake,
        serde_json::json!({
            "model": "gpt-fake",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "please write" }],
                },
            ],
        }),
    );

    // Turn 2: the transcript now includes a function_call_output item.
    let second = post_responses(
        &fake,
        serde_json::json!({
            "model": "gpt-fake",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "please write" }],
                },
                { "type": "function_call", "call_id": "c1", "name": "write", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c1", "output": "ok" },
            ],
        }),
    );
    let second_events = parse_sse(&second);
    let text: String = second_events
        .iter()
        .filter(|e| e.event == "response.output_text.delta")
        .filter_map(|e| e.data["delta"].as_str())
        .collect();
    assert_eq!(text, "all done");

    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].view.as_ref().unwrap().prior_tool_results, 0);
    assert_eq!(requests[1].view.as_ref().unwrap().prior_tool_results, 1);
}

#[test]
fn codex_tool_call_renders_function_call_item_and_arguments() {
    // M5 acceptance: a ToolCall turn renders a function_call output item
    // (response.output_item.added) carrying the call_id and name, with
    // response.function_call_arguments.delta fragments for the arguments and a
    // response.output_item.done before the terminal response.completed.
    let reply = Reply {
        turns: vec![Turn::ToolCall {
            id: "call_1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({ "path": "out.txt" }),
        }],
        usage: Usage::default(),
        stop: StopReason::ToolCalls,
    };
    let fake = start_with(Script::Fixed(reply));

    let body = post_responses(
        &fake,
        serde_json::json!({
            "model": "gpt-fake",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "go" }],
                },
            ],
        }),
    );
    let events = parse_sse(&body);

    // The function_call output item is added, carrying call_id and name.
    let added = events
        .iter()
        .find(|e| e.event == "response.output_item.added")
        .expect("a response.output_item.added frame");
    assert_eq!(added.data["item"]["type"], "function_call");
    assert_eq!(added.data["item"]["call_id"], "call_1");
    assert_eq!(added.data["item"]["name"], "write");

    // The arguments stream as function_call_arguments.delta fragments that
    // reassemble into the original JSON.
    let args: String = events
        .iter()
        .filter(|e| e.event == "response.function_call_arguments.delta")
        .filter_map(|e| e.data["delta"].as_str())
        .collect();
    let parsed: Value = serde_json::from_str(&args).expect("arguments are valid JSON");
    assert_eq!(parsed, serde_json::json!({ "path": "out.txt" }));

    // The item is finalized with output_item.done, and the stream still
    // terminates with response.completed.
    assert!(
        events
            .iter()
            .any(|e| e.event == "response.output_item.done"),
        "expected a response.output_item.done frame"
    );
    assert_eq!(
        events.last().map(|e| e.event.as_str()),
        Some("response.completed"),
        "stream must terminate with response.completed; body was:\n{body}"
    );
}

#[test]
fn unknown_path_is_404() {
    let fake = start_with(Script::Fixed(Reply::text("unused")));
    let client = reqwest::blocking::Client::new();
    let status = client
        .get(format!("{}/nope", fake.base_url()))
        .send()
        .expect("request succeeds")
        .status()
        .as_u16();
    assert_eq!(status, 404);
}

/// Start a `FakeLlm` serving `script`.
fn start_with(script: Script) -> jig_server::FakeLlm {
    jig_server::FakeLlm::start(script).expect("FakeLlm starts")
}

/// POST a Codex responses request body, returning the SSE response body.
fn post_responses(fake: &jig_server::FakeLlm, body: Value) -> String {
    let client = reqwest::blocking::Client::new();
    client
        .post(format!("{}/backend-api/codex/responses", fake.base_url()))
        // Auth is irrelevant to jig, but real Codex clients send a bearer token;
        // prove we accept it.
        .header("authorization", "Bearer test-key")
        .json(&body)
        .send()
        .expect("request succeeds")
        .text()
        .expect("body is readable")
}
