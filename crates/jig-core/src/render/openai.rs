//! OpenAI / DeepSeek chat-completions SSE renderer.
//!
//! Emits `data:`-only frames terminated by `data: [DONE]`, matching the shape
//! the `pi_agent_rust` OpenAI parser accepts (see bootstrap.md "Minimal SSE
//! sequences"). M1 renders [`Turn::Text`]; M5 adds [`Turn::ToolCall`].

use serde_json::json;

use super::SseFrame;
use crate::{Reply, Turn};

/// Render a [`Reply`] into OpenAI chat-completions SSE frames.
///
/// Frame sequence:
/// 1. role bootstrap: `delta: {"role":"assistant"}`
/// 2. one content delta per [`Turn::Text`]; one or two tool-call deltas per
///    [`Turn::ToolCall`] (a header delta carrying `id`/`name`, then an
///    arguments delta — split to exercise the SDK's argument-reassembly path)
/// 3. a final frame carrying `finish_reason` + `usage`
/// 4. the `[DONE]` sentinel
///
/// [`Turn::Thinking`] turns carry no chat-completions surface and are skipped.
pub fn render_openai(reply: &Reply) -> Vec<SseFrame> {
    let mut frames = Vec::new();

    // 1. Role bootstrap frame.
    frames.push(SseFrame::data(
        json!({
            "choices": [{ "delta": { "role": "assistant" }, "finish_reason": null }]
        })
        .to_string(),
    ));

    // 2. One content delta per text turn; tool-call deltas per tool call.
    //
    // Each tool call occupies its own `tool_calls[]` slot, indexed by the order
    // tool calls appear in the reply (`index`). OpenAI streams the call as a
    // header delta (`index` + `id` + `function.name`, empty `arguments`) followed
    // by one or more `function.arguments` string fragments that the client
    // concatenates. We emit the arguments as a single fragment carrying the whole
    // serialized JSON — valid, and still flowing through the chunk-reassembly
    // path because `name` and `arguments` arrive in separate frames.
    let mut tool_index = 0usize;
    for turn in &reply.turns {
        match turn {
            Turn::Text(text) => {
                frames.push(SseFrame::data(
                    json!({
                        "choices": [{ "delta": { "content": text }, "finish_reason": null }]
                    })
                    .to_string(),
                ));
            }
            Turn::ToolCall { id, name, args } => {
                let index = tool_index;
                tool_index += 1;

                // Header delta: opens the tool-call slot with id + name.
                frames.push(SseFrame::data(
                    json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [{
                                    "index": index,
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": "" },
                                }],
                            },
                            "finish_reason": null,
                        }]
                    })
                    .to_string(),
                ));

                // Arguments delta: the serialized JSON, carried as one fragment.
                frames.push(SseFrame::data(
                    json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [{
                                    "index": index,
                                    "function": { "arguments": args.to_string() },
                                }],
                            },
                            "finish_reason": null,
                        }]
                    })
                    .to_string(),
                ));
            }
            Turn::Thinking(_) => {}
        }
    }

    // 3. Terminal frame: finish_reason + usage.
    frames.push(SseFrame::data(
        json!({
            "choices": [{ "delta": {}, "finish_reason": reply.stop.openai_finish_reason() }],
            "usage": {
                "prompt_tokens": reply.usage.prompt_tokens,
                "completion_tokens": reply.usage.completion_tokens,
                "total_tokens": reply.usage.total_tokens(),
            }
        })
        .to_string(),
    ));

    // 4. Stream terminator.
    frames.push(SseFrame::data("[DONE]"));

    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StopReason, Usage};
    use serde_json::Value;

    fn data_payloads(frames: &[SseFrame]) -> Vec<String> {
        frames.iter().map(|f| f.data.clone()).collect()
    }

    #[test]
    fn frames_start_with_role_and_end_with_done() {
        let frames = render_openai(&Reply::text("hello"));
        let payloads = data_payloads(&frames);

        // First frame bootstraps the assistant role.
        let first: Value = serde_json::from_str(&payloads[0]).unwrap();
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");

        // Last frame is the DONE sentinel; OpenAI frames carry no event line.
        assert_eq!(payloads.last().unwrap(), "[DONE]");
        assert!(frames.iter().all(|f| f.event.is_none()));
    }

    #[test]
    fn each_text_turn_becomes_a_content_delta() {
        let reply = Reply {
            turns: vec![Turn::Text("foo".to_string()), Turn::Text("bar".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let contents: Vec<String> = render_openai(&reply)
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(String::from)
            })
            .collect();
        assert_eq!(contents, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn terminal_frame_carries_finish_reason_and_usage() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
            },
            stop: StopReason::Stop,
        };
        let frames = render_openai(&reply);
        // The terminal frame is the one before [DONE].
        let terminal: Value = serde_json::from_str(&frames[frames.len() - 2].data).unwrap();
        assert_eq!(terminal["choices"][0]["finish_reason"], "stop");
        assert_eq!(terminal["usage"]["prompt_tokens"], 5);
        assert_eq!(terminal["usage"]["completion_tokens"], 7);
        assert_eq!(terminal["usage"]["total_tokens"], 12);
    }

    #[test]
    fn tool_call_stop_reason_is_rendered() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let frames = render_openai(&reply);
        let terminal: Value = serde_json::from_str(&frames[frames.len() - 2].data).unwrap();
        assert_eq!(terminal["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn thinking_turns_are_skipped() {
        let reply = Reply {
            turns: vec![
                Turn::Thinking("hmm".to_string()),
                Turn::Text("visible".to_string()),
            ],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let contents: Vec<String> = render_openai(&reply)
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(String::from)
            })
            .collect();
        assert_eq!(contents, vec!["visible".to_string()]);
    }

    /// Collect the `delta.tool_calls[]` entries across all rendered frames.
    fn tool_call_deltas(frames: &[SseFrame]) -> Vec<Value> {
        frames
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["choices"][0]["delta"]["tool_calls"].as_array().cloned())
            .flatten()
            .collect()
    }

    #[test]
    fn tool_call_emits_id_name_and_chunked_arguments() {
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt", "contents": "hi" }),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let frames = render_openai(&reply);
        let deltas = tool_call_deltas(&frames);

        // A header delta (id + name) then an arguments delta, same index 0.
        assert_eq!(deltas.len(), 2, "header + arguments deltas");
        assert_eq!(deltas[0]["index"], 0);
        assert_eq!(deltas[0]["id"], "call_1");
        assert_eq!(deltas[0]["function"]["name"], "write");
        assert_eq!(deltas[1]["index"], 0);

        // The arguments fragments reassemble into the original JSON object.
        let args: String = deltas
            .iter()
            .filter_map(|d| d["function"]["arguments"].as_str())
            .collect();
        let parsed: Value = serde_json::from_str(&args).expect("arguments are valid JSON");
        assert_eq!(
            parsed,
            serde_json::json!({ "path": "out.txt", "contents": "hi" })
        );

        // The terminal frame reports the tool-call stop reason.
        let terminal: Value = serde_json::from_str(&frames[frames.len() - 2].data).unwrap();
        assert_eq!(terminal["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn multiple_tool_calls_get_distinct_indices() {
        let reply = Reply {
            turns: vec![
                Turn::ToolCall {
                    id: "call_a".to_string(),
                    name: "read".to_string(),
                    args: serde_json::json!({ "path": "a" }),
                },
                Turn::ToolCall {
                    id: "call_b".to_string(),
                    name: "read".to_string(),
                    args: serde_json::json!({ "path": "b" }),
                },
            ],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let deltas = tool_call_deltas(&render_openai(&reply));
        // The two header deltas carry distinct ids and indices.
        let headers: Vec<&Value> = deltas.iter().filter(|d| d["id"].is_string()).collect();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0]["index"], 0);
        assert_eq!(headers[0]["id"], "call_a");
        assert_eq!(headers[1]["index"], 1);
        assert_eq!(headers[1]["id"], "call_b");
    }
}
