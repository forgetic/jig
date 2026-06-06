//! Synchronous end-to-end test for the OpenAI dialect.
//!
//! This is the M1 acceptance test: a plain `#[test]` (no `#[tokio::main]`,
//! no async runtime of its own) that starts a `FakeLlm`, hits its `base_url()`
//! with blocking `reqwest`, asserts the streamed reply parses and ends in
//! `[DONE]`, then lets `Drop` tear the runtime thread down. It demonstrates
//! that the entire in-process lifecycle is start → blocking HTTP → drop.

use jig_core::{Reply, Script};
use serde_json::Value;

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
        let client = reqwest::blocking::Client::new();
        client
            .post(format!("{}/chat/completions", self.fake.base_url()))
            // Auth is irrelevant to jig, but real clients send it; prove we
            // accept it.
            .header("Authorization", "Bearer test-key")
            .json(&serde_json::json!({
                "model": "fake",
                "stream": true,
                "messages": [{ "role": "user", "content": "hi" }],
            }))
            .send()
            .expect("request succeeds")
            .text()
            .expect("body is readable")
    }

    fn get_status(&self, path: &str) -> u16 {
        let client = reqwest::blocking::Client::new();
        client
            .get(format!("{}{path}", self.fake.base_url()))
            .send()
            .expect("request succeeds")
            .status()
            .as_u16()
    }
}
