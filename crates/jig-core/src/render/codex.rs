//! OpenAI Codex responses SSE renderer.
//!
//! Emits typed `event:`/`data:` frames (Codex **requires** the `event:` line)
//! in the order the SDK parser accepts (see bootstrap.md "Minimal SSE
//! sequences"). M4 renders the [`Turn::Text`] happy path only:
//!
//! one `response.output_text.delta` per text turn → `response.completed` (with
//! `usage`).
//!
//! M5 adds tool-call output items: `response.output_item.added` (a
//! `function_call` item carrying id/call_id/name) →
//! `response.function_call_arguments.delta` → `response.output_item.done`.

use serde_json::json;

use super::SseFrame;
use crate::{Reply, Turn};

/// Build a typed Codex SSE frame: an `event:` line plus its `data:` JSON.
fn event(name: &str, data: serde_json::Value) -> SseFrame {
    SseFrame {
        event: Some(name.to_string()),
        data: data.to_string(),
    }
}

/// Render a [`Reply`] into OpenAI Codex responses SSE frames.
///
/// Frame sequence:
/// 1. one `response.output_text.delta` per [`Turn::Text`] (output item `msg_1`,
///    content index 0).
/// 2. one tool-call output item per [`Turn::ToolCall`], each at its own
///    `output_index`: `response.output_item.added` (a `function_call` item
///    carrying id/call_id/name) → `response.function_call_arguments.delta`
///    (the serialized arguments) → `response.output_item.done`.
/// 3. `response.completed` — terminates the stream and carries `usage`.
///
/// [`Turn::Thinking`] turns carry no surface here and are skipped. Unlike the
/// OpenAI chat-completions stream, Codex has no `[DONE]` sentinel: the
/// `response.completed` event is the terminator.
pub fn render_codex(reply: &Reply) -> Vec<SseFrame> {
    let mut frames = Vec::new();

    // 1. One output_text delta per text turn.
    for turn in &reply.turns {
        if let Turn::Text(text) = turn {
            frames.push(event(
                "response.output_text.delta",
                json!({
                    "type": "response.output_text.delta",
                    "item_id": "msg_1",
                    "content_index": 0,
                    "delta": text,
                }),
            ));
        }
    }

    // 2. One function_call output item per tool call. The text output item
    //    occupies output_index 0, so tool calls start at output_index 1. The
    //    item id and call_id both derive from the call's id: the client echoes
    //    `call_id` back on the matching `function_call_output`. Arguments stream
    //    as a single serialized-JSON delta that the client concatenates.
    let mut output_index = 1usize;
    for turn in &reply.turns {
        if let Turn::ToolCall { id, name, args } = turn {
            let index = output_index;
            output_index += 1;
            let item_id = format!("fc_{id}");

            frames.push(event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": index,
                    "item": {
                        "type": "function_call",
                        "id": item_id,
                        "call_id": id,
                        "name": name,
                        "arguments": "",
                    },
                }),
            ));
            frames.push(event(
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": index,
                    "item_id": item_id,
                    "delta": args.to_string(),
                }),
            ));
            frames.push(event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": index,
                    "item": {
                        "type": "function_call",
                        "id": item_id,
                        "call_id": id,
                        "name": name,
                        "arguments": args.to_string(),
                    },
                }),
            ));
        }
    }

    // 3. Terminal completion frame: carries final token usage.
    frames.push(event(
        "response.completed",
        json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": reply.usage.prompt_tokens,
                    "output_tokens": reply.usage.completion_tokens,
                },
            },
        }),
    ));

    frames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StopReason, Usage};
    use serde_json::Value;

    /// The `data:` payloads of the frames whose `event:` line is `name`.
    fn events_named<'a>(frames: &'a [SseFrame], name: &str) -> Vec<&'a SseFrame> {
        frames
            .iter()
            .filter(|f| f.event.as_deref() == Some(name))
            .collect()
    }

    #[test]
    fn every_frame_carries_an_event_line() {
        let frames = render_codex(&Reply::text("hi"));
        assert!(
            frames.iter().all(|f| f.event.is_some()),
            "Codex frames must all carry an event: line"
        );
    }

    #[test]
    fn frames_close_with_response_completed() {
        let frames = render_codex(&Reply::text("hi"));
        assert_eq!(
            frames.last().unwrap().event.as_deref(),
            Some("response.completed")
        );
    }

    #[test]
    fn frames_are_in_canonical_order() {
        let frames = render_codex(&Reply::text("hi"));
        let order: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
        assert_eq!(
            order,
            vec!["response.output_text.delta", "response.completed"]
        );
    }

    #[test]
    fn each_text_turn_becomes_an_output_text_delta() {
        let reply = Reply {
            turns: vec![Turn::Text("foo".to_string()), Turn::Text("bar".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let texts: Vec<String> = events_named(&render_codex(&reply), "response.output_text.delta")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["delta"].as_str().map(String::from))
            .collect();
        assert_eq!(texts, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn completed_frame_carries_token_usage() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
            },
            stop: StopReason::Stop,
        };
        let frames = render_codex(&reply);
        let completed: Value =
            serde_json::from_str(&events_named(&frames, "response.completed")[0].data).unwrap();
        assert_eq!(completed["response"]["usage"]["input_tokens"], 5);
        assert_eq!(completed["response"]["usage"]["output_tokens"], 7);
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
        let texts: Vec<String> = events_named(&render_codex(&reply), "response.output_text.delta")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["delta"].as_str().map(String::from))
            .collect();
        assert_eq!(texts, vec!["visible".to_string()]);
    }

    #[test]
    fn tool_call_emits_function_call_item_and_arguments() {
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt" }),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let frames = render_codex(&reply);

        // The output item is added as a function_call carrying name + call_id.
        let added: Value =
            serde_json::from_str(&events_named(&frames, "response.output_item.added")[0].data)
                .unwrap();
        assert_eq!(added["item"]["type"], "function_call");
        assert_eq!(added["item"]["call_id"], "call_1");
        assert_eq!(added["item"]["name"], "write");
        assert_eq!(added["output_index"], 1);

        // Arguments stream as function_call_arguments.delta fragments that
        // reassemble into the original JSON.
        let args: String = events_named(&frames, "response.function_call_arguments.delta")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["delta"].as_str().map(String::from))
            .collect();
        let parsed: Value = serde_json::from_str(&args).expect("arguments are valid JSON");
        assert_eq!(parsed, serde_json::json!({ "path": "out.txt" }));

        // The item is finalized with output_item.done before completion.
        let done = events_named(&frames, "response.output_item.done");
        assert_eq!(done.len(), 1);
        let done_value: Value = serde_json::from_str(&done[0].data).unwrap();
        assert_eq!(done_value["item"]["call_id"], "call_1");

        // response.completed is still the terminator.
        assert_eq!(
            frames.last().unwrap().event.as_deref(),
            Some("response.completed")
        );
    }

    #[test]
    fn multiple_tool_calls_get_distinct_output_indices() {
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
        let added: Vec<Value> = events_named(&render_codex(&reply), "response.output_item.added")
            .iter()
            .map(|f| serde_json::from_str::<Value>(&f.data).unwrap())
            .collect();
        assert_eq!(added.len(), 2);
        assert_eq!(added[0]["output_index"], 1);
        assert_eq!(added[0]["item"]["call_id"], "call_a");
        assert_eq!(added[1]["output_index"], 2);
        assert_eq!(added[1]["item"]["call_id"], "call_b");
    }
}
