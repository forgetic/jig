//! M6 acceptance: load a script file, start the server via the public API, and
//! assert a request is served per the scripted reply.
//!
//! This is the end-to-end proof for the standalone binary's one job — turning a
//! script *file* into served replies — without spawning a subprocess: it parses
//! the same [`ScriptFile`] schema `src/main.rs` loads, lowers it to a [`Script`],
//! starts a real [`FakeLlm`], and drives it with a blocking HTTP client exactly
//! as the binary's clients would.

use jig_core::ScriptFile;
use serde_json::Value;

mod support;

/// Split a `text/event-stream` body into the payloads of its `data:` lines.
fn data_payloads(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(str::to_string)
        .collect()
}

/// Reassemble the streamed OpenAI `delta.content` fragments into one string.
fn streamed_openai_text(body: &str) -> String {
    data_payloads(body)
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

fn post_chat_completions(base_url: &str) -> String {
    support::post_json(
        &format!("{base_url}/chat/completions"),
        &[],
        &serde_json::json!({
            "model": "fake",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    )
}

#[test]
fn fixed_script_file_is_served_per_the_scripted_reply() {
    // The same JSON a user would write into a file and pass to `jig`.
    let script = ScriptFile::from_json_str(r#"{ "fixed": { "text": "scripted reply" } }"#)
        .expect("script file parses")
        .into_script();

    let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
    let body = post_chat_completions(&fake.base_url());

    assert_eq!(
        data_payloads(&body).last().map(String::as_str),
        Some("[DONE]"),
        "stream did not end with [DONE]; body was:\n{body}"
    );
    assert_eq!(streamed_openai_text(&body), "scripted reply");
}

#[test]
fn sequence_script_file_serves_replies_in_order_then_repeats_the_last() {
    let json = r#"{ "sequence": [ { "text": "first" }, { "text": "second" } ] }"#;
    let script = ScriptFile::from_json_str(json)
        .expect("script file parses")
        .into_script();

    let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
    let base = fake.base_url();

    assert_eq!(streamed_openai_text(&post_chat_completions(&base)), "first");
    assert_eq!(
        streamed_openai_text(&post_chat_completions(&base)),
        "second"
    );
    // Exhausted: the last reply repeats from here on (M2 behaviour, preserved
    // through the file-format path).
    assert_eq!(
        streamed_openai_text(&post_chat_completions(&base)),
        "second"
    );

    // The server recorded all three requests.
    assert_eq!(fake.requests().len(), 3);
}

#[test]
fn full_form_tool_call_script_file_renders_a_tool_call() {
    // A full-form reply file exercising a tool call + explicit stop reason — the
    // part of the schema beyond the text shorthand.
    let json = r#"
        {
          "fixed": {
            "turns": [
              { "tool_call": { "id": "call_1", "name": "write",
                               "args": { "path": "out.txt" } } }
            ],
            "stop": "tool_calls"
          }
        }
    "#;
    let script = ScriptFile::from_json_str(json)
        .expect("script file parses")
        .into_script();

    let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
    let body = post_chat_completions(&fake.base_url());

    let json_frames: Vec<Value> = data_payloads(&body)
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .map(|p| serde_json::from_str(p).expect("frame is valid JSON"))
        .collect();

    let header = json_frames
        .iter()
        .filter_map(|v| v["choices"][0]["delta"]["tool_calls"].as_array())
        .flatten()
        .find(|d| d["id"].is_string())
        .expect("a tool-call header delta with an id");
    assert_eq!(header["id"], "call_1");
    assert_eq!(header["function"]["name"], "write");

    assert!(
        json_frames
            .iter()
            .any(|v| v["choices"][0]["finish_reason"] == "tool_calls"),
        "expected finish_reason == tool_calls; body was:\n{body}"
    );
}
