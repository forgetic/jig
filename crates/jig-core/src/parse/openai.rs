//! OpenAI / DeepSeek chat-completions SSE → canonical [`Reply`] parser.
//!
//! The inverse of [`crate::render::render_openai`]. It consumes the bytes of an
//! OpenAI `/chat/completions` `stream: true` event stream — whether produced by
//! jig's own renderer or captured from a real OpenAI-compatible backend
//! (DeepSeek) by the recorder (#18) — and reconstructs the canonical [`Reply`]:
//! text turns, tool-call turns, [`Usage`], and the [`StopReason`]. Recovering
//! the canonical model from an *authoritative* capture is the keystone the
//! structural-template machinery (P2, #14) and the T1/T2 conformance checks
//! build on, exactly as it is for the Anthropic (P3) and Codex (P4) parsers: T1
//! is "render → strip → == template", and a parser is what lets a capture be
//! reduced to the same canonical shape a render produces.
//!
//! # Event model
//!
//! OpenAI streams `data:`-only frames (no `event:` line); each `data:` payload
//! is one `chat.completion.chunk` carrying a `choices[0].delta`. The stream
//! terminates with the sentinel `data: [DONE]`. This folds the chunk deltas
//! (see the M1 renderer and the issue #14 scope) back into the model:
//!
//! - `delta.role` (`"assistant"`) — the role bootstrap; carries no surface and
//!   is ignored.
//! - `delta.content` — appends to the single assistant text run. Consecutive
//!   content deltas concatenate into one [`Turn::Text`], so a capture that
//!   streamed `"foo"` as `"f"`,`"oo"` parses to the same `Reply` as a one-shot
//!   `"foo"` render — the coalescing property T1 relies on.
//! - `delta.tool_calls[]` — each entry is keyed by its `index`. A header delta
//!   carries `id` + `function.name` (and empty `arguments`); subsequent deltas
//!   at the same `index` carry `function.arguments` string fragments that the
//!   client concatenates, then parses once — so do we.
//! - `choices[0].finish_reason` — the terminal stop field (`stop`/`tool_calls`/
//!   `length`/…), present on the final chunk.
//! - `usage` — the final chunk's `{prompt_tokens, completion_tokens}` (DeepSeek,
//!   like OpenAI with `stream_options.include_usage`, sends `usage: null` on the
//!   intermediate chunks and the real counts on the last).
//! - `[DONE]` — terminates the stream.
//!
//! # Stop reason
//!
//! The terminal `finish_reason` maps to the canonical [`StopReason`]: `stop`,
//! `length`, and `content_filter` are normal completions (`render_openai` emits
//! `stop` for [`StopReason::Stop`]); `tool_calls` is a tool hand-off. Anything
//! else is an explicit error rather than a silent default, so an unexpected
//! provider value surfaces instead of masquerading as a clean stop. A stream
//! with no `finish_reason` at all is [`ParseError::MissingFinishReason`].
//!
//! # Faithfulness to `render_openai`
//!
//! `render_openai` emits one `content` delta per [`Turn::Text`], all before any
//! tool-call deltas, so a multi-text reply reassembles into a single
//! concatenated text turn — the same coalescing the Anthropic/Codex parsers
//! perform. An empty role-bootstrap `content` (`""`) contributes nothing, so a
//! reply with only a tool call does not gain a phantom empty text turn.
//! [`Turn::Thinking`] turns carry no chat-completions surface and so never
//! reappear. Tool calls materialize in ascending `index`, matching how the
//! client assembles the final `tool_calls` array.

use serde_json::Value;

use crate::{Reply, StopReason, Turn, Usage};

use super::sse::parse_sse;

/// Why parsing an OpenAI chat-completions SSE stream failed.
///
/// Parsing is deliberately lenient about *extra* structure (unknown fields, the
/// `usage: null` intermediate chunks, the role bootstrap) but strict about the
/// invariants a downstream consumer relies on: a terminal `finish_reason` must
/// be present and mappable, and a tool call's accumulated arguments must be
/// valid JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No chunk carried a `choices[0].finish_reason`, so the reply has no
    /// terminal state to map. A complete OpenAI stream always sends one on the
    /// final content chunk, before `[DONE]`.
    MissingFinishReason,
    /// A tool call's concatenated `function.arguments` fragments did not parse
    /// as JSON. The string is the accumulated, unparseable arguments.
    InvalidToolArguments(String),
    /// A chunk carried a `finish_reason` value this model has no mapping for.
    /// The string is the unrecognized wire value.
    UnknownFinishReason(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingFinishReason => {
                write!(f, "stream carried no choices[0].finish_reason")
            }
            ParseError::InvalidToolArguments(args) => {
                write!(f, "tool call arguments are not valid JSON: {args}")
            }
            ParseError::UnknownFinishReason(value) => {
                write!(f, "unrecognized finish_reason: {value}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Map an OpenAI wire `finish_reason` to the canonical [`StopReason`].
///
/// `stop`, `length`, and `content_filter` are all normal completions
/// (`render_openai` emits `stop` for [`StopReason::Stop`]); `tool_calls` is a
/// tool hand-off. Anything else is an explicit error rather than a silent
/// default, so an unexpected provider value surfaces instead of masquerading as
/// a clean stop.
fn map_finish_reason(value: &str) -> Result<StopReason, ParseError> {
    match value {
        "stop" | "length" | "content_filter" => Ok(StopReason::Stop),
        "tool_calls" | "function_call" => Ok(StopReason::ToolCalls),
        other => Err(ParseError::UnknownFinishReason(other.to_string())),
    }
}

/// A tool call being assembled, keyed by its `index` in the `tool_calls` array.
struct ToolCall {
    index: u64,
    id: String,
    name: String,
    arguments: String,
}

/// Parse an OpenAI `/chat/completions` SSE byte stream into a canonical
/// [`Reply`].
///
/// Pure and synchronous. Lenient about unknown fields, the `usage: null`
/// intermediate chunks, and the role bootstrap; strict about the terminal
/// `finish_reason` and tool-argument validity (see [`ParseError`]).
pub fn parse_openai_sse(bytes: &[u8]) -> Result<Reply, ParseError> {
    let events = parse_sse(bytes);

    let mut text = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut prompt_tokens: u32 = 0;
    let mut completion_tokens: u32 = 0;
    let mut stop_reason: Option<StopReason> = None;

    for ev in &events {
        // OpenAI uses `data:`-only frames; the `[DONE]` sentinel is not JSON and
        // just terminates the stream.
        if ev.data.trim() == "[DONE]" {
            continue;
        }
        let chunk: Value = match serde_json::from_str(&ev.data) {
            Ok(value) => value,
            // A non-JSON, non-sentinel data line (should not happen in a real
            // capture) carries nothing to fold; skip it rather than abort.
            Err(_) => continue,
        };

        // Every payload field hangs off the first choice's `delta`.
        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first());

        if let Some(delta) = choice.and_then(|c| c.get("delta")) {
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                text.push_str(content);
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tool_calls {
                    fold_tool_call_delta(&mut calls, tc);
                }
            }
        }

        if let Some(reason) = choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(Value::as_str)
        {
            stop_reason = Some(map_finish_reason(reason)?);
        }

        // Usage is `null` on intermediate chunks and the real object on the
        // final one; read it whenever it is present and an object.
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            prompt_tokens = u32_field(usage, "prompt_tokens").unwrap_or(prompt_tokens);
            completion_tokens = u32_field(usage, "completion_tokens").unwrap_or(completion_tokens);
        }
    }

    let stop = stop_reason.ok_or(ParseError::MissingFinishReason)?;

    // Materialize turns: the concatenated text run first (render_openai emits all
    // content deltas before any tool-call deltas), then tool calls in ascending
    // `index` — the order the client assembles the `tool_calls` array in. An
    // empty text run (only a role bootstrap, or a pure tool call) contributes no
    // turn, keeping render → parse an exact round-trip.
    let mut turns = Vec::new();
    if !text.is_empty() {
        turns.push(Turn::Text(text));
    }
    calls.sort_by_key(|c| c.index);
    for call in calls {
        // The client concatenates argument fragments, then parses once. An empty
        // buffer means an empty arguments object.
        let args: Value = if call.arguments.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&call.arguments)
                .map_err(|_| ParseError::InvalidToolArguments(call.arguments.clone()))?
        };
        turns.push(Turn::ToolCall {
            id: call.id,
            name: call.name,
            args,
        });
    }

    Ok(Reply {
        turns,
        usage: Usage {
            prompt_tokens,
            completion_tokens,
        },
        stop,
    })
}

/// Fold one `delta.tool_calls[]` entry into the assembling call at its `index`,
/// opening the call on first sight and appending `id`/`name`/`arguments` as they
/// arrive. OpenAI splits a call into a header delta (`id` + `function.name`,
/// empty `arguments`) followed by `function.arguments` string fragments, all at
/// the same `index`; non-header fragments omit `index`, so a fragment with no
/// `index` targets the most recently opened call.
fn fold_tool_call_delta(calls: &mut Vec<ToolCall>, tc: &Value) {
    let index = tc.get("index").and_then(Value::as_u64);
    let function = tc.get("function");

    // Resolve the call this fragment targets: the one at `index` if given,
    // otherwise the most recently opened (the header always carries `index`).
    let slot = match index {
        Some(idx) => {
            if !calls.iter().any(|c| c.index == idx) {
                calls.push(ToolCall {
                    index: idx,
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
            }
            calls.iter_mut().find(|c| c.index == idx)
        }
        None => calls.last_mut(),
    };

    let Some(call) = slot else {
        return;
    };

    if let Some(id) = tc.get("id").and_then(Value::as_str)
        && !id.is_empty()
    {
        call.id = id.to_string();
    }
    if let Some(name) = function.and_then(|f| f.get("name")).and_then(Value::as_str)
        && !name.is_empty()
    {
        call.name = name.to_string();
    }
    if let Some(args) = function
        .and_then(|f| f.get("arguments"))
        .and_then(Value::as_str)
    {
        call.arguments.push_str(args);
    }
}

/// Read a `u32` field from a JSON object, if present and in range.
fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value.get(key).and_then(Value::as_u64).map(|n| n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{frames_to_body, render_openai};

    /// Render a reply to wire bytes the way the server would, for round-trips.
    fn rendered(reply: &Reply) -> Vec<u8> {
        frames_to_body(&render_openai(reply)).into_bytes()
    }

    #[test]
    fn round_trips_a_single_text_reply() {
        let reply = Reply::text("hello world");
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn round_trips_multiple_text_turns() {
        // render_openai emits one content delta per text turn; they reassemble
        // into a single concatenated text turn.
        let reply = Reply {
            turns: vec![Turn::Text("foo".to_string()), Turn::Text("bar".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("foobar".to_string())]);
        assert_eq!(parsed.stop, StopReason::Stop);
    }

    #[test]
    fn round_trips_usage_and_stop_reason() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage {
                prompt_tokens: 11,
                completion_tokens: 7,
            },
            stop: StopReason::Stop,
        };
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.usage.prompt_tokens, 11);
        assert_eq!(parsed.usage.completion_tokens, 7);
        assert_eq!(parsed.stop, StopReason::Stop);
    }

    #[test]
    fn round_trips_a_single_tool_call() {
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt", "content": "hi" }),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn round_trips_text_then_multiple_tool_calls() {
        let reply = Reply {
            turns: vec![
                Turn::Text("let me look".to_string()),
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
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn empty_role_bootstrap_contributes_no_text_turn() {
        // render_openai opens with `delta.content: ""`; with only a tool call,
        // that empty content must not become a phantom text turn.
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "noop".to_string(),
                args: serde_json::json!({}),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns.len(), 1);
        assert!(matches!(parsed.turns[0], Turn::ToolCall { .. }));
    }

    #[test]
    fn thinking_turns_round_trip_as_their_visible_surface() {
        // render_openai skips Thinking turns entirely.
        let reply = Reply {
            turns: vec![
                Turn::Thinking("hmm".to_string()),
                Turn::Text("answer".to_string()),
            ],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let parsed = parse_openai_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("answer".to_string())]);
    }

    #[test]
    fn concatenates_fragmented_content_deltas() {
        // A real capture streams text in fragments; they must concatenate. This
        // is the shape DeepSeek emits (data:-only chunks, usage: null until the
        // final frame).
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}],\"usage\":null}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}],\"usage\":null}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}],\"usage\":null}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        );
        let parsed = parse_openai_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("Hello".to_string())]);
        assert_eq!(parsed.usage.prompt_tokens, 3);
        assert_eq!(parsed.usage.completion_tokens, 2);
        assert_eq!(parsed.stop, StopReason::Stop);
    }

    #[test]
    fn concatenates_fragmented_tool_arguments() {
        // The header delta carries id + name with empty arguments; subsequent
        // deltas (often with no `id`/`name`) carry argument fragments that join
        // into one JSON object, exactly as the OpenAI client reassembles it.
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"type\":\"function\",\"function\":{\"name\":\"search\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"rust\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        let parsed = parse_openai_sse(stream.as_bytes()).unwrap();
        assert_eq!(
            parsed.turns,
            vec![Turn::ToolCall {
                id: "call_9".to_string(),
                name: "search".to_string(),
                args: serde_json::json!({ "q": "rust" }),
            }]
        );
        assert_eq!(parsed.stop, StopReason::ToolCalls);
    }

    #[test]
    fn parses_parallel_tool_calls_in_index_order() {
        // Two distinct `index` slots interleave; turns come out 0 then 1.
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{\\\"p\\\":\\\"b\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"p\\\":\\\"a\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let parsed = parse_openai_sse(stream.as_bytes()).unwrap();
        assert_eq!(
            parsed.turns,
            vec![
                Turn::ToolCall {
                    id: "call_a".to_string(),
                    name: "read".to_string(),
                    args: serde_json::json!({ "p": "a" }),
                },
                Turn::ToolCall {
                    id: "call_b".to_string(),
                    name: "read".to_string(),
                    args: serde_json::json!({ "p": "b" }),
                },
            ]
        );
    }

    #[test]
    fn maps_alternate_normal_finish_reasons_to_stop() {
        let template = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"REASON\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        for reason in ["stop", "length", "content_filter"] {
            let stream = template.replace("REASON", reason);
            let parsed = parse_openai_sse(stream.as_bytes()).unwrap();
            assert_eq!(parsed.stop, StopReason::Stop, "finish_reason {reason}");
        }
    }

    #[test]
    fn missing_finish_reason_is_an_error() {
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(
            parse_openai_sse(stream.as_bytes()),
            Err(ParseError::MissingFinishReason)
        );
    }

    #[test]
    fn unknown_finish_reason_is_an_error() {
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"refusal\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(
            parse_openai_sse(stream.as_bytes()),
            Err(ParseError::UnknownFinishReason("refusal".to_string()))
        );
    }

    #[test]
    fn invalid_tool_arguments_is_an_error() {
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"t\",\"function\":{\"name\":\"n\",\"arguments\":\"{not json\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(
            parse_openai_sse(stream.as_bytes()),
            Err(ParseError::InvalidToolArguments("{not json".to_string()))
        );
    }

    #[test]
    fn tool_call_with_no_argument_deltas_is_an_empty_object() {
        let stream = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"t\",\"function\":{\"name\":\"ping\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let parsed = parse_openai_sse(stream.as_bytes()).unwrap();
        assert_eq!(
            parsed.turns,
            vec![Turn::ToolCall {
                id: "t".to_string(),
                name: "ping".to_string(),
                args: serde_json::json!({}),
            }]
        );
    }

    #[test]
    fn done_sentinel_without_finish_reason_still_errors() {
        // A lone `[DONE]` is not a complete stream.
        assert_eq!(
            parse_openai_sse(b"data: [DONE]\n\n"),
            Err(ParseError::MissingFinishReason)
        );
    }
}
