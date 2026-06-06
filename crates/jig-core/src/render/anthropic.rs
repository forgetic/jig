//! Anthropic messages SSE renderer.
//!
//! Emits typed `event:`/`data:` frames (Anthropic **requires** the `event:`
//! line) in the order the SDK parser accepts (see bootstrap.md "Minimal SSE
//! sequences"). M3 renders the [`Turn::Text`] happy path only:
//!
//! `message_start` → `content_block_start` (text) → one `content_block_delta`
//! (`text_delta`) per text turn → `content_block_stop` → `message_delta`
//! (`stop_reason`) → `message_stop`.
//!
//! `ping` events are ignored by the client and are omitted. M5 adds tool-call
//! rendering: a `tool_use` content block (`content_block_start` carrying the
//! id/name) with `input_json_delta` deltas, then `content_block_stop`.

use serde_json::json;

use super::SseFrame;
use crate::{Reply, Turn};

/// Build a typed Anthropic SSE frame: an `event:` line plus its `data:` JSON.
fn event(name: &str, data: serde_json::Value) -> SseFrame {
    SseFrame {
        event: Some(name.to_string()),
        data: data.to_string(),
    }
}

/// Render a [`Reply`] into Anthropic messages SSE frames.
///
/// Frame sequence:
/// 1. `message_start` — opens the message with `input_tokens` usage.
/// 2. a text content block at index 0: `content_block_start` (text) → one
///    `content_block_delta` (`text_delta`) per [`Turn::Text`] →
///    `content_block_stop`.
/// 3. one tool-use content block per [`Turn::ToolCall`], at the next index:
///    `content_block_start` (`tool_use` carrying id/name) → `content_block_delta`
///    (`input_json_delta` carrying the serialized arguments) →
///    `content_block_stop`.
/// 4. `message_delta` — carries `stop_reason` + `output_tokens` usage.
/// 5. `message_stop` — terminates the stream.
///
/// The text block at index 0 is always opened and closed (even with no text
/// turns) so the surrounding sequence is stable; [`Turn::Thinking`] turns carry
/// no surface here and are skipped.
pub fn render_anthropic(reply: &Reply) -> Vec<SseFrame> {
    let mut frames = Vec::new();

    // 1. Open the message. `content` starts empty; usage reports input tokens.
    frames.push(event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "fake",
                "content": [],
                "stop_reason": null,
                "usage": {
                    "input_tokens": reply.usage.prompt_tokens,
                    "output_tokens": 0,
                },
            },
        }),
    ));

    // 2. Open a single text content block at index 0.
    frames.push(event(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" },
        }),
    ));

    // One text_delta per text turn. Tool calls open their own blocks below.
    for turn in &reply.turns {
        if let Turn::Text(text) = turn {
            frames.push(event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "text_delta", "text": text },
                }),
            ));
        }
    }

    // Close the text block.
    frames.push(event(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 0 }),
    ));

    // 3. One tool_use content block per tool call, each at its own index after
    //    the text block. Anthropic streams the JSON input as `input_json_delta`
    //    `partial_json` fragments that the client concatenates and parses; we
    //    carry the whole serialized object as one fragment.
    let mut block_index = 1usize;
    for turn in &reply.turns {
        if let Turn::ToolCall { id, name, args } = turn {
            let index = block_index;
            block_index += 1;

            frames.push(event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {},
                    },
                }),
            ));
            frames.push(event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "input_json_delta", "partial_json": args.to_string() },
                }),
            ));
            frames.push(event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
        }
    }

    // 4. Terminal message_delta: stop_reason + output-token usage.
    frames.push(event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": reply.stop.anthropic_stop_reason() },
            "usage": { "output_tokens": reply.usage.completion_tokens },
        }),
    ));

    // 5. Stream terminator.
    frames.push(event("message_stop", json!({ "type": "message_stop" })));

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
        let frames = render_anthropic(&Reply::text("hi"));
        assert!(
            frames.iter().all(|f| f.event.is_some()),
            "Anthropic frames must all carry an event: line"
        );
    }

    #[test]
    fn frames_open_with_message_start_and_close_with_message_stop() {
        let frames = render_anthropic(&Reply::text("hi"));
        assert_eq!(
            frames.first().unwrap().event.as_deref(),
            Some("message_start")
        );
        assert_eq!(
            frames.last().unwrap().event.as_deref(),
            Some("message_stop")
        );
    }

    #[test]
    fn frames_are_in_canonical_order() {
        let frames = render_anthropic(&Reply::text("hi"));
        let order: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
        assert_eq!(
            order,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }

    #[test]
    fn each_text_turn_becomes_a_text_delta() {
        let reply = Reply {
            turns: vec![Turn::Text("foo".to_string()), Turn::Text("bar".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let texts: Vec<String> = events_named(&render_anthropic(&reply), "content_block_delta")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["delta"]["text"].as_str().map(String::from))
            .collect();
        assert_eq!(texts, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn message_start_reports_input_tokens() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
            },
            stop: StopReason::Stop,
        };
        let frames = render_anthropic(&reply);
        let start: Value =
            serde_json::from_str(&events_named(&frames, "message_start")[0].data).unwrap();
        assert_eq!(start["message"]["usage"]["input_tokens"], 5);
    }

    #[test]
    fn message_delta_carries_stop_reason_and_output_tokens() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
            },
            stop: StopReason::Stop,
        };
        let frames = render_anthropic(&reply);
        let delta: Value =
            serde_json::from_str(&events_named(&frames, "message_delta")[0].data).unwrap();
        assert_eq!(delta["delta"]["stop_reason"], "end_turn");
        assert_eq!(delta["usage"]["output_tokens"], 7);
    }

    #[test]
    fn tool_call_stop_reason_is_rendered() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let frames = render_anthropic(&reply);
        let delta: Value =
            serde_json::from_str(&events_named(&frames, "message_delta")[0].data).unwrap();
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
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
        let texts: Vec<String> = events_named(&render_anthropic(&reply), "content_block_delta")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["delta"]["text"].as_str().map(String::from))
            .collect();
        assert_eq!(texts, vec!["visible".to_string()]);
    }

    #[test]
    fn tool_call_opens_a_tool_use_block_with_input_json_delta() {
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "toolu_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt" }),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let frames = render_anthropic(&reply);

        // The tool_use block opens at index 1 (after the index-0 text block),
        // carrying the id and name.
        let start = events_named(&frames, "content_block_start")
            .into_iter()
            .map(|f| serde_json::from_str::<Value>(&f.data).unwrap())
            .find(|v| v["content_block"]["type"] == "tool_use")
            .expect("a tool_use content_block_start");
        assert_eq!(start["index"], 1);
        assert_eq!(start["content_block"]["id"], "toolu_1");
        assert_eq!(start["content_block"]["name"], "write");

        // Its input arrives as input_json_delta partial_json that parses back to
        // the original arguments object.
        let partial: String = events_named(&frames, "content_block_delta")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter(|v| v["delta"]["type"] == "input_json_delta")
            .filter_map(|v| v["delta"]["partial_json"].as_str().map(String::from))
            .collect();
        let parsed: Value = serde_json::from_str(&partial).expect("partial_json is valid JSON");
        assert_eq!(parsed, serde_json::json!({ "path": "out.txt" }));

        // The block is closed, and the terminal stop_reason reflects tool use.
        let stop_indices: Vec<i64> = events_named(&frames, "content_block_stop")
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(&f.data).ok())
            .filter_map(|v| v["index"].as_i64())
            .collect();
        assert!(stop_indices.contains(&1), "tool_use block is closed");
        let delta: Value =
            serde_json::from_str(&events_named(&frames, "message_delta")[0].data).unwrap();
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn multiple_tool_calls_get_distinct_block_indices() {
        let reply = Reply {
            turns: vec![
                Turn::ToolCall {
                    id: "toolu_a".to_string(),
                    name: "read".to_string(),
                    args: serde_json::json!({ "path": "a" }),
                },
                Turn::ToolCall {
                    id: "toolu_b".to_string(),
                    name: "read".to_string(),
                    args: serde_json::json!({ "path": "b" }),
                },
            ],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let starts: Vec<Value> = events_named(&render_anthropic(&reply), "content_block_start")
            .into_iter()
            .map(|f| serde_json::from_str::<Value>(&f.data).unwrap())
            .filter(|v| v["content_block"]["type"] == "tool_use")
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0]["index"], 1);
        assert_eq!(starts[0]["content_block"]["id"], "toolu_a");
        assert_eq!(starts[1]["index"], 2);
        assert_eq!(starts[1]["content_block"]["id"], "toolu_b");
    }
}
