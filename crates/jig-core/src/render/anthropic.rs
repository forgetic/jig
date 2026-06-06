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
//! `ping` events are ignored by the client and are omitted. Tool-call rendering
//! (`tool_use` content block + `input_json_delta`) is M5; non-text turns are
//! skipped here.

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
/// Frame sequence (single text content block, index 0):
/// 1. `message_start` — opens the message with `input_tokens` usage.
/// 2. `content_block_start` — opens the text block at index 0.
/// 3. one `content_block_delta` (`text_delta`) per [`Turn::Text`].
/// 4. `content_block_stop` — closes the text block.
/// 5. `message_delta` — carries `stop_reason` + `output_tokens` usage.
/// 6. `message_stop` — terminates the stream.
///
/// Non-text turns are skipped in M3 (tool-call rendering is M5).
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

    // 3. One text_delta per text turn. Non-text turns are skipped in M3.
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

    // 4. Close the text block.
    frames.push(event(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 0 }),
    ));

    // 5. Terminal message_delta: stop_reason + output-token usage.
    frames.push(event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": reply.stop.anthropic_stop_reason() },
            "usage": { "output_tokens": reply.usage.completion_tokens },
        }),
    ));

    // 6. Stream terminator.
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
    fn non_text_turns_are_skipped_in_m3() {
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
}
