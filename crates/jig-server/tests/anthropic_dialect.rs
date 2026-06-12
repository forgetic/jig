//! Synchronous end-to-end test for the Anthropic messages dialect (M3).
//!
//! Mirrors the M1 OpenAI acceptance test: a plain `#[test]` (no async runtime of
//! its own) starts a `FakeLlm`, POSTs `/v1/messages` with a blocking HTTP client,
//! and asserts the streamed SSE frames parse as a text reply — `event:` lines
//! present, the canonical frame sequence, reassembled text, and the terminal
//! `stop_reason`. The request projection (top-level `system`, prior tool
//! results) is asserted via `fake.requests()`.

use jig_core::{Dialect, Reply, Script, StopReason, Turn, Usage};
use serde_json::Value;

mod support;

/// One parsed SSE event: the `event:` name and its `data:` JSON payload.
#[derive(Debug)]
struct SseEvent {
    event: String,
    data: Value,
}

/// Parse a `text/event-stream` body into ordered `(event, data-json)` pairs.
///
/// Each event is an `event:` line immediately followed by a `data:` line
/// (Anthropic always pairs them); blank lines separate events.
fn parse_sse(body: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_event: Option<String> = None;

    for line in body.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            current_event = Some(name.to_string());
        } else if let Some(data) = line.strip_prefix("data: ") {
            let event = current_event
                .take()
                .expect("an Anthropic data: line must follow an event: line");
            let json: Value = serde_json::from_str(data).expect("data: payload is valid JSON");
            events.push(SseEvent { event, data: json });
        }
    }

    events
}

#[test]
fn anthropic_stream_parses_as_a_text_reply() {
    let fake = start_with(Script::Fixed(Reply::text("hello from jig")));

    let body = post_messages(
        &fake,
        serde_json::json!({
            "model": "claude-fake",
            "stream": true,
            "system": "be terse",
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    );
    let events = parse_sse(&body);

    // Every frame carries an event: line, in the canonical order.
    let order: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        order,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ],
        "unexpected frame order; body was:\n{body}"
    );

    // message_start opens an assistant message.
    let start = &events[0].data;
    assert_eq!(start["type"], "message_start");
    assert_eq!(start["message"]["role"], "assistant");

    // The text_delta blocks reassemble to the scripted text.
    let text: String = events
        .iter()
        .filter(|e| e.event == "content_block_delta")
        .filter_map(|e| e.data["delta"]["text"].as_str())
        .collect();
    assert_eq!(text, "hello from jig");

    // The terminal message_delta carries the end-of-turn stop reason.
    let message_delta = events
        .iter()
        .find(|e| e.event == "message_delta")
        .expect("a message_delta frame");
    assert_eq!(message_delta.data["delta"]["stop_reason"], "end_turn");

    // The request was projected as an Anthropic view, with the top-level system
    // prompt surfaced as a leading system message.
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let recorded = &requests[0];
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(recorded.method, "POST");
    let view = recorded
        .view
        .as_ref()
        .expect("dialect route projects a view");
    assert_eq!(view.dialect, Dialect::Anthropic);
    assert_eq!(view.model.as_deref(), Some("claude-fake"));
    assert_eq!(
        view.messages.first().map(|m| m.role.as_str()),
        Some("system")
    );
    assert_eq!(
        view.messages.first().map(|m| m.content.as_str()),
        Some("be terse")
    );
}

#[test]
fn anthropic_tool_call_renders_tool_use_block_and_stop_reason() {
    // M5 acceptance: a ToolCall turn renders a `tool_use` content block carrying
    // the id and name, with `input_json_delta` fragments for the arguments, and
    // the terminal message_delta reports stop_reason "tool_use".
    let reply = Reply {
        turns: vec![Turn::ToolCall {
            id: "toolu_1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({ "path": "out.txt" }),
        }],
        usage: Usage::default(),
        stop: StopReason::ToolCalls,
    };
    let fake = start_with(Script::Fixed(reply));

    let body = post_messages(
        &fake,
        serde_json::json!({
            "model": "claude-fake",
            "messages": [{ "role": "user", "content": "go" }],
        }),
    );
    let events = parse_sse(&body);

    // A tool_use content block opens, carrying the id and name.
    let tool_start = events
        .iter()
        .find(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use")
        .expect("a tool_use content_block_start");
    assert_eq!(tool_start.data["content_block"]["id"], "toolu_1");
    assert_eq!(tool_start.data["content_block"]["name"], "write");

    // The arguments stream as input_json_delta partial_json fragments that
    // reassemble into the original JSON.
    let partial: String = events
        .iter()
        .filter(|e| e.event == "content_block_delta")
        .filter(|e| e.data["delta"]["type"] == "input_json_delta")
        .filter_map(|e| e.data["delta"]["partial_json"].as_str())
        .collect();
    let parsed: Value = serde_json::from_str(&partial).expect("partial_json is valid JSON");
    assert_eq!(parsed, serde_json::json!({ "path": "out.txt" }));

    // The terminal message_delta carries the tool-use stop reason.
    let message_delta = events
        .iter()
        .find(|e| e.event == "message_delta")
        .expect("a message_delta frame");
    assert_eq!(message_delta.data["delta"]["stop_reason"], "tool_use");
}

#[test]
fn anthropic_rule_branches_on_prior_tool_results() {
    // Drive a one-step tool-use loop over /v1/messages: with no tool_result
    // blocks yet, hand off; once a tool_result comes back, return final text.
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
    let first = post_messages(
        &fake,
        serde_json::json!({
            "model": "claude-fake",
            "messages": [{ "role": "user", "content": "please write" }],
        }),
    );
    let first_events = parse_sse(&first);
    let first_stop = first_events
        .iter()
        .find(|e| e.event == "message_delta")
        .map(|e| e.data["delta"]["stop_reason"].clone());
    assert_eq!(first_stop, Some(Value::String("tool_use".to_string())));

    // Turn 2: the transcript now includes a tool_result block.
    let second = post_messages(
        &fake,
        serde_json::json!({
            "model": "claude-fake",
            "messages": [
                { "role": "user", "content": "please write" },
                {
                    "role": "assistant",
                    "content": [{ "type": "tool_use", "id": "t1", "name": "write", "input": {} }],
                },
                {
                    "role": "user",
                    "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }],
                },
            ],
        }),
    );
    let second_events = parse_sse(&second);
    let text: String = second_events
        .iter()
        .filter(|e| e.event == "content_block_delta")
        .filter_map(|e| e.data["delta"]["text"].as_str())
        .collect();
    assert_eq!(text, "all done");

    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].view.as_ref().unwrap().prior_tool_results, 0);
    assert_eq!(requests[1].view.as_ref().unwrap().prior_tool_results, 1);
}

#[test]
fn unknown_path_is_404() {
    let fake = start_with(Script::Fixed(Reply::text("unused")));
    let status = support::get_status(&format!("{}/nope", fake.base_url()));
    assert_eq!(status, 404);
}

/// Start a `FakeLlm` serving `script`.
fn start_with(script: Script) -> jig_server::FakeLlm {
    jig_server::FakeLlm::start(script).expect("FakeLlm starts")
}

/// POST an Anthropic messages request body, returning the SSE response body.
fn post_messages(fake: &jig_server::FakeLlm, body: Value) -> String {
    support::post_json(
        &format!("{}/v1/messages", fake.base_url()),
        // Auth is irrelevant to jig, but real Anthropic clients send these;
        // prove we accept them.
        &[("x-api-key", "test-key"), ("anthropic-version", "2023-06-01")],
        &body,
    )
}
