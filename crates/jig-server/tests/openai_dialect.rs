//! Synchronous end-to-end test for the OpenAI dialect.
//!
//! This is the M1 acceptance test: a plain `#[test]` with no async runtime of
//! its own that starts a `FakeLlm`, hits its `base_url()`
//! with a blocking HTTP client, asserts the streamed reply parses and ends in
//! `[DONE]`, then lets `Drop` tear the runtime thread down. It demonstrates
//! that the entire in-process lifecycle is start → blocking HTTP → drop.

use jig_core::{Reply, Script, StopReason, Turn, Usage};
use serde_json::Value;

mod support;

/// Split a `text/event-stream` body into the payloads of its `data:` lines.
fn data_payloads(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(str::to_string)
        .collect()
}

#[test]
fn openai_stream_parses_and_ends_in_done() {
    let fake = FakeLlmFixture::start("hello from jig");

    let body = fake.post_chat_completions();
    let payloads = data_payloads(&body);

    // The stream must terminate with the OpenAI sentinel.
    assert_eq!(
        payloads.last().map(String::as_str),
        Some("[DONE]"),
        "stream did not end with [DONE]; body was:\n{body}"
    );

    // Every non-sentinel frame must be parseable JSON with a `choices` array.
    let json_frames: Vec<Value> = payloads
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .map(|p| serde_json::from_str(p).expect("frame is valid JSON"))
        .collect();
    assert!(
        json_frames.iter().all(|v| v["choices"].is_array()),
        "every frame should carry a choices array"
    );

    // The first frame bootstraps the assistant role.
    assert_eq!(json_frames[0]["choices"][0]["delta"]["role"], "assistant");

    // The streamed content reassembles to the scripted text.
    let content: String = json_frames
        .iter()
        .filter_map(|v| v["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "hello from jig");

    // A terminal frame carries a non-null finish_reason.
    assert!(
        json_frames
            .iter()
            .any(|v| v["choices"][0]["finish_reason"] == "stop"),
        "expected a frame with finish_reason == stop"
    );
}

#[test]
fn openai_tool_call_carries_id_name_arguments_and_stop_reason() {
    // M5 acceptance: a ToolCall turn renders into `delta.tool_calls[]` entries
    // carrying the id, function name, and (chunked) arguments, with the terminal
    // frame reporting `finish_reason: "tool_calls"`.
    let reply = Reply {
        turns: vec![Turn::ToolCall {
            id: "call_1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({ "path": "out.txt", "contents": "hi" }),
        }],
        usage: Usage::default(),
        stop: StopReason::ToolCalls,
    };
    let fake = jig_server::FakeLlm::start(Script::Fixed(reply)).expect("FakeLlm starts");
    let body = FakeLlmFixture { fake }.post_chat_completions();

    let json_frames: Vec<Value> = data_payloads(&body)
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .map(|p| serde_json::from_str(p).expect("frame is valid JSON"))
        .collect();

    // Gather the tool_calls[] deltas across all frames.
    let tool_deltas: Vec<&Value> = json_frames
        .iter()
        .filter_map(|v| v["choices"][0]["delta"]["tool_calls"].as_array())
        .flatten()
        .collect();
    assert!(!tool_deltas.is_empty(), "expected tool_calls deltas");

    // The header delta carries the id and function name.
    let header = tool_deltas
        .iter()
        .find(|d| d["id"].is_string())
        .expect("a tool-call header delta with an id");
    assert_eq!(header["id"], "call_1");
    assert_eq!(header["function"]["name"], "write");

    // The argument fragments reassemble into the original JSON object.
    let args: String = tool_deltas
        .iter()
        .filter_map(|d| d["function"]["arguments"].as_str())
        .collect();
    let parsed: Value = serde_json::from_str(&args).expect("arguments are valid JSON");
    assert_eq!(
        parsed,
        serde_json::json!({ "path": "out.txt", "contents": "hi" })
    );

    // The terminal frame reports the tool-call stop reason.
    assert!(
        json_frames
            .iter()
            .any(|v| v["choices"][0]["finish_reason"] == "tool_calls"),
        "expected a frame with finish_reason == tool_calls; body was:\n{body}"
    );
}

#[test]
fn unknown_path_is_404() {
    let fake = FakeLlmFixture::start("unused");
    let status = fake.get_status("/nope");
    assert_eq!(status, 404);
}

/// Small wrapper so each test reads as start → call → (implicit) drop.
struct FakeLlmFixture {
    fake: jig_server::FakeLlm,
}

impl FakeLlmFixture {
    fn start(text: &str) -> Self {
        let script = Script::Fixed(Reply::text(text));
        let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
        FakeLlmFixture { fake }
    }

    fn post_chat_completions(&self) -> String {
        support::post_json(
            &format!("{}/chat/completions", self.fake.base_url()),
            // Auth is irrelevant to jig, but real clients send it; prove we
            // accept it.
            &[("Authorization", "Bearer test-key")],
            &serde_json::json!({
                "model": "fake",
                "stream": true,
                "messages": [{ "role": "user", "content": "hi" }],
            }),
        )
    }

    fn get_status(&self, path: &str) -> u16 {
        support::get_status(&format!("{}{path}", self.fake.base_url()))
    }
}
