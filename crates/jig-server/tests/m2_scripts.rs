//! M2 acceptance tests: `Sequence`/`Rule` scripts, `RequestView`, and request
//! capture (issue #4).
//!
//! Each test is a plain synchronous `#[test]` that starts a `FakeLlm`, drives it
//! with a blocking `std::net` HTTP client, and asserts on the streamed replies and on
//! `fake.requests()` — the same start → blocking HTTP → drop lifecycle the M1
//! test established, now exercising multi-turn scripting.

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

/// Reassemble the streamed assistant text from an OpenAI SSE body.
fn streamed_content(body: &str) -> String {
    data_payloads(body)
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(String::from)
        })
        .collect()
}

/// The terminal `finish_reason` of an OpenAI SSE body.
fn finish_reason(body: &str) -> Option<String> {
    data_payloads(body)
        .iter()
        .filter(|p| p.as_str() != "[DONE]")
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .filter_map(|v| v["choices"][0]["finish_reason"].as_str().map(String::from))
        .next_back()
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
fn sequence_serves_replies_in_order_and_captures_each_request() {
    // A three-step sequence: tool call, tool call, then final text. The last
    // reply repeats once the sequence is exhausted.
    let script = Script::sequence(vec![
        Reply {
            turns: vec![Turn::Text("step-1".to_string())],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        },
        Reply {
            turns: vec![Turn::Text("step-2".to_string())],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        },
        Reply::text("final"),
    ]);
    let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
    let base = fake.base_url();

    // Drive four successive HTTP requests; the user message changes each turn so
    // we can confirm capture order.
    let bodies: Vec<String> = (0..4)
        .map(|turn| {
            post_chat(
                &base,
                serde_json::json!([{ "role": "user", "content": format!("turn-{turn}") }]),
            )
        })
        .collect();

    // Each request returned the expected reply, in order; the last repeats.
    assert_eq!(streamed_content(&bodies[0]), "step-1");
    assert_eq!(finish_reason(&bodies[0]).as_deref(), Some("tool_calls"));
    assert_eq!(streamed_content(&bodies[1]), "step-2");
    assert_eq!(finish_reason(&bodies[1]).as_deref(), Some("tool_calls"));
    assert_eq!(streamed_content(&bodies[2]), "final");
    assert_eq!(finish_reason(&bodies[2]).as_deref(), Some("stop"));
    // Exhausted → repeats the last reply.
    assert_eq!(streamed_content(&bodies[3]), "final");

    // `requests()` captured them in order, with parsed views.
    let requests = fake.requests();
    assert_eq!(requests.len(), 4, "one capture per request");
    for (turn, recorded) in requests.iter().enumerate() {
        assert_eq!(recorded.path, "/chat/completions");
        assert_eq!(recorded.method, "POST");
        let view = recorded
            .view
            .as_ref()
            .expect("dialect route projects a view");
        assert_eq!(view.model.as_deref(), Some("deepseek-chat"));
        assert_eq!(
            view.last_message().map(|m| m.content.as_str()),
            Some(format!("turn-{turn}").as_str()),
            "captured requests preserve arrival order"
        );
        // The raw body is preserved verbatim for tests that need it.
        assert!(recorded.body_str().contains(&format!("turn-{turn}")));
    }
}

#[test]
fn rule_branches_on_request_view_turn_count() {
    // A rule modelling a one-step tool-use loop: while no tool result has come
    // back yet, ask for a tool call; once one has, return the final answer.
    let script = Script::rule(|view| {
        if view.prior_tool_results == 0 {
            Reply {
                turns: vec![Turn::ToolCall {
                    id: "call_1".to_string(),
                    name: "write".to_string(),
                    args: serde_json::json!({ "path": "out.txt" }),
                }],
                usage: Usage::default(),
                stop: StopReason::ToolCalls,
            }
        } else {
            Reply::text("all done")
        }
    });
    let fake = jig_server::FakeLlm::start(script).expect("FakeLlm starts");
    let base = fake.base_url();

    // Turn 1: no tool results in the transcript yet.
    let first = post_chat(
        &base,
        serde_json::json!([{ "role": "user", "content": "please write" }]),
    );
    assert_eq!(
        finish_reason(&first).as_deref(),
        Some("tool_calls"),
        "with no prior tool results the rule asks for a tool call"
    );

    // Turn 2: the transcript now includes a tool result.
    let second = post_chat(
        &base,
        serde_json::json!([
            { "role": "user", "content": "please write" },
            { "role": "assistant", "content": "" },
            { "role": "tool", "content": "wrote out.txt" },
        ]),
    );
    assert_eq!(streamed_content(&second), "all done");
    assert_eq!(finish_reason(&second).as_deref(), Some("stop"));

    // The captured views reflect the branch input the rule saw.
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].view.as_ref().unwrap().prior_tool_results, 0);
    assert_eq!(requests[1].view.as_ref().unwrap().prior_tool_results, 1);
}

#[test]
fn unknown_path_is_captured_without_a_view() {
    let fake =
        jig_server::FakeLlm::start(Script::Fixed(Reply::text("unused"))).expect("FakeLlm starts");
    let status = support::get_status(&format!("{}/nope", fake.base_url()));
    assert_eq!(status, 404);

    // The 404 path is still recorded, but with no dialect projection.
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/nope");
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].view.is_none());
}
