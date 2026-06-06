//! OpenAI / DeepSeek chat-completions SSE renderer.
//!
//! Emits `data:`-only frames terminated by `data: [DONE]`, matching the shape
//! the `pi_agent_rust` OpenAI parser accepts (see bootstrap.md "Minimal SSE
//! sequences"). M1 renders [`Turn::Text`] only.

use serde_json::json;

use super::SseFrame;
use crate::{Reply, Turn};

/// Render a [`Reply`] into OpenAI chat-completions SSE frames.
///
/// Frame sequence:
/// 1. role bootstrap: `delta: {"role":"assistant"}`
/// 2. one content delta per [`Turn::Text`]
/// 3. a final frame carrying `finish_reason` + `usage`
/// 4. the `[DONE]` sentinel
///
/// Non-text turns are skipped in M1 (tool-call rendering is M5).
pub fn render_openai(reply: &Reply) -> Vec<SseFrame> {
    let mut frames = Vec::new();

    // 1. Role bootstrap frame.
    frames.push(SseFrame::data(
        json!({
            "choices": [{ "delta": { "role": "assistant" }, "finish_reason": null }]
        })
        .to_string(),
    ));

    // 2. One content delta per text turn.
    for turn in &reply.turns {
        if let Turn::Text(text) = turn {
            frames.push(SseFrame::data(
                json!({
                    "choices": [{ "delta": { "content": text }, "finish_reason": null }]
                })
                .to_string(),
            ));
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
    fn non_text_turns_are_skipped_in_m1() {
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
}
