//! M5 acceptance test: the multi-turn tool-use loop shape `run_coding_agent`
//! expects (issue #7).
//!
//! A `Script::Sequence` serves turn 1 = a `write` `ToolCall` and turn 2 = the
//! final `Text`, across two successive HTTP requests. This mirrors the real
//! coding-agent loop: the client receives a tool call, executes it locally,
//! feeds the tool result back as a follow-up request, and gets the final answer.
//! The test drives the loop over the OpenAI dialect with a blocking HTTP client
//! and asserts via `fake.requests()` that the second request carried the prior tool
//! result — exactly the seam the loop depends on.

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

/// The non-sentinel JSON frames of an OpenAI SSE body.
fn json_frames(body: &str) -> Vec<Value> {
    data_payloads(body)
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .map(|p| serde_json::from_str(p).expect("frame is valid JSON"))
        .collect()
}

/// The terminal `finish_reason` of an OpenAI SSE body.
fn finish_reason(body: &str) -> Option<String> {
    json_frames(body)
        .iter()
        .filter_map(|v| v["choices"][0]["finish_reason"].as_str().map(String::from))
        .next_back()
}

/// Reassemble the streamed assistant text from an OpenAI SSE body.
fn streamed_content(body: &str) -> String {
    json_frames(body)
        .iter()
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(String::from)
        })
        .collect()
}

/// The first tool call's id, name, and reassembled arguments, if any.
fn first_tool_call(body: &str) -> Option<(String, String, String)> {
    let frames = json_frames(body);
    let deltas: Vec<Value> = frames
        .iter()
        .filter_map(|v| v["choices"][0]["delta"]["tool_calls"].as_array().cloned())
        .flatten()
        .collect();
    let header = deltas.iter().find(|d| d["id"].is_string())?;
    let id = header["id"].as_str()?.to_string();
    let name = header["function"]["name"].as_str()?.to_string();
    let args: String = deltas
        .iter()
        .filter_map(|d| d["function"]["arguments"].as_str())
        .collect();
    Some((id, name, args))
}

/// POST a chat-completions request carrying `messages`, returning the SSE body.
fn post_chat(base_url: &str, messages: Value) -> String {
    support::post_json(
        &format!("{base_url}/chat/completions"),
        &[("Authorization", "Bearer test-key")],
        &serde_json::json!({
            "model": "deepseek-chat",
            "stream": true,
            "messages": messages,
        }),
    )
}

#[test]
fn sequence_drives_a_tool_call_then_final_text_loop() {
    // Turn 1: a write tool call. Turn 2: the final text answer.
    let script = Script::sequence(vec![
        Reply {
            turns: vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt", "contents": "hello" }),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        },
        Reply::text("wrote it"),
    ]);
    let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
    let base = fake.base_url();

    // --- Turn 1: send the user prompt, receive the tool call. ---
    let first = post_chat(
        &base,
        serde_json::json!([{ "role": "user", "content": "please write out.txt" }]),
    );
    assert_eq!(
        finish_reason(&first).as_deref(),
        Some("tool_calls"),
        "turn 1 must end as a tool call; body was:\n{first}"
    );
    let (id, name, args) = first_tool_call(&first).expect("turn 1 carries a tool call");
    assert_eq!(id, "call_1");
    assert_eq!(name, "write");
    let args_json: Value = serde_json::from_str(&args).expect("tool arguments are valid JSON");
    assert_eq!(
        args_json,
        serde_json::json!({ "path": "out.txt", "contents": "hello" })
    );

    // --- Client executes the tool, feeds the result back, asks again. ---
    // The follow-up transcript echoes the assistant's tool call and appends the
    // tool result as a `role: "tool"` message keyed by the returned id.
    let second = post_chat(
        &base,
        serde_json::json!([
            { "role": "user", "content": "please write out.txt" },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": args },
                }],
            },
            { "role": "tool", "tool_call_id": "call_1", "content": "wrote out.txt (12 bytes)" },
        ]),
    );

    // --- Turn 2: the final text answer. ---
    assert_eq!(streamed_content(&second), "wrote it");
    assert_eq!(finish_reason(&second).as_deref(), Some("stop"));

    // The fake captured both requests; the second carried the prior tool result,
    // which is exactly what the loop reply selection relies on.
    let requests = fake.requests();
    assert_eq!(requests.len(), 2, "one capture per request");
    assert_eq!(
        requests[0].view.as_ref().unwrap().prior_tool_results,
        0,
        "the first request has no prior tool result"
    );
    assert_eq!(
        requests[1].view.as_ref().unwrap().prior_tool_results,
        1,
        "the second request carried the prior tool result"
    );
    // And the raw second body really does contain the fed-back tool output.
    assert!(
        requests[1].body_str().contains("wrote out.txt (12 bytes)"),
        "the second request body must carry the tool result verbatim"
    );
}
