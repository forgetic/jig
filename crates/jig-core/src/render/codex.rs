//! OpenAI Codex responses SSE renderer.
//!
//! Emits typed `event:`/`data:` frames (Codex **requires** the `event:` line)
//! in the order the SDK parser accepts (see bootstrap.md "Minimal SSE
//! sequences"). M4 renders the [`Turn::Text`] happy path only:
//!
//! one `response.output_text.delta` per text turn → `response.completed` (with
//! `usage`).
//!
//! Tool-call output items (`response.output_item.added` with a `function_call`
//! item, `response.function_call_arguments.delta`, `response.output_item.done`)
//! are M5; non-text turns are skipped here.

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
/// Frame sequence (single output item `msg_1`, content index 0):
/// 1. one `response.output_text.delta` per [`Turn::Text`].
/// 2. `response.completed` — terminates the stream and carries `usage`.
///
/// Non-text turns are skipped in M4 (tool-call rendering is M5). Unlike the
/// OpenAI chat-completions stream, Codex has no `[DONE]` sentinel: the
/// `response.completed` event is the terminator.
pub fn render_codex(reply: &Reply) -> Vec<SseFrame> {
    let mut frames = Vec::new();

    // 1. One output_text delta per text turn. Non-text turns are skipped in M4.
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

    // 2. Terminal completion frame: carries final token usage.
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
    fn non_text_turns_are_skipped_in_m4() {
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
}
