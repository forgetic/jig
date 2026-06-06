//! OpenAI Codex responses SSE → canonical [`Reply`] parser.
//!
//! The inverse of [`crate::render::render_codex`]. It consumes the bytes of a
//! Codex `/backend-api/codex/responses` event stream — whether produced by
//! jig's own renderer or captured from the real ChatGPT backend by the recorder
//! (#18) — and reconstructs the canonical [`Reply`]: text turns, tool-call
//! turns, and [`Usage`]. Recovering the canonical model from an *authoritative*
//! capture is the keystone the structural-template machinery (P2, #14) and the
//! T1/T2 conformance checks build on, exactly as it is for the merged Anthropic
//! parser (P3).
//!
//! # Event model
//!
//! Codex streams typed `event:`/`data:` frames; the `data:` payload also repeats
//! the event name in its `"type"`. As with the Anthropic parser we drive off the
//! `event:` line (what the client dispatches on), falling back to the data
//! `type` when a capture omitted the event line. The frames this folds back
//! (see the M4 renderer and the issue #16 scope) are:
//!
//! - `response.output_text.delta` — appends `delta` to the single text output
//!   item (Codex streams all visible text under one item, `content_index` 0).
//!   Consecutive deltas concatenate into one [`Turn::Text`].
//! - `response.output_item.added` — opens an output item at an `output_index`. A
//!   `function_call` item (carrying `call_id`/`name`) becomes a pending
//!   [`Turn::ToolCall`] whose arguments accumulate from its deltas.
//! - `response.function_call_arguments.delta` — appends a serialized-JSON
//!   `delta` fragment to the open function-call item at its `output_index` (the
//!   client concatenates fragments, then parses once — so do we).
//! - `response.output_item.done` — finalizes a function-call item; its terminal
//!   `arguments` string is authoritative when present, otherwise the accumulated
//!   delta fragments are used.
//! - `response.completed` — terminates the stream (Codex has **no** `[DONE]`
//!   sentinel) and carries `response.usage.{input_tokens,output_tokens}`.
//!
//! # Stop reason
//!
//! Unlike Anthropic, the Codex stream carries no terminal `stop_reason` field —
//! `render_codex` emits none. The canonical [`StopReason`] is therefore inferred
//! from the reply's shape: a reply that ends with one or more tool-call items is
//! a [`StopReason::ToolCalls`] hand-off, otherwise a normal [`StopReason::Stop`].
//! This matches how `render_codex` chooses frames from a [`Reply`] (it keys on
//! the turns, not on `reply.stop`), so render → parse round-trips the stop reason
//! for the `Stop` and `ToolCalls` cases it can express.
//!
//! # Faithfulness to `render_codex`
//!
//! `render_codex` emits one `response.output_text.delta` per [`Turn::Text`], all
//! under the same output item, so a multi-text reply reassembles into a single
//! concatenated text turn — the same coalescing the Anthropic parser performs and
//! the property T1 relies on. [`Turn::Thinking`] turns carry no surface in the
//! Codex stream and so never reappear. Output items materialize in ascending
//! `output_index`, matching how the client assembles the final output array.

use serde_json::Value;

use crate::{Reply, StopReason, Turn, Usage};

use super::sse::parse_sse;

/// Why parsing a Codex SSE stream failed.
///
/// Parsing is deliberately lenient about *extra* structure (unknown events,
/// extra fields) but strict about the invariants a downstream consumer relies
/// on: the stream must terminate with `response.completed`, and a function
/// call's accumulated arguments must be valid JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The stream carried no `response.completed` event, so it never terminated.
    /// A complete Codex stream always ends with one (there is no `[DONE]`).
    MissingCompletion,
    /// A function call's concatenated argument fragments did not parse as JSON.
    /// The string is the accumulated, unparseable arguments.
    InvalidToolArguments(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingCompletion => {
                write!(f, "stream carried no response.completed event")
            }
            ParseError::InvalidToolArguments(args) => {
                write!(f, "function call arguments are not valid JSON: {args}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// A function-call output item being assembled, keyed by its `output_index`.
struct FunctionCall {
    output_index: u64,
    call_id: String,
    name: String,
    /// Arguments accumulated from `function_call_arguments.delta` fragments.
    partial_args: String,
    /// The terminal `arguments` string from `output_item.done`, if seen. When
    /// present this is authoritative — a real capture sends the full arguments
    /// on the `done` item, and the client uses that.
    final_args: Option<String>,
}

/// Parse a Codex `/backend-api/codex/responses` SSE byte stream into a canonical
/// [`Reply`].
///
/// Pure and synchronous. Lenient about unknown events and extra fields; strict
/// about the terminating `response.completed` and function-argument validity
/// (see [`ParseError`]).
pub fn parse_codex_sse(bytes: &[u8]) -> Result<Reply, ParseError> {
    let events = parse_sse(bytes);

    let mut text = String::new();
    let mut calls: Vec<FunctionCall> = Vec::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut completed = false;

    for ev in &events {
        let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
        let kind = ev
            .event
            .as_deref()
            .or_else(|| data.get("type").and_then(Value::as_str))
            .unwrap_or("");

        match kind {
            "response.output_text.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            "response.output_item.added" => {
                let item = data.get("item");
                let is_function_call = item.and_then(|i| i.get("type")).and_then(Value::as_str)
                    == Some("function_call");
                if is_function_call {
                    let output_index = data
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    // The client echoes `call_id` back on the matching
                    // function_call_output, so that — not the item `id` — is the
                    // canonical tool-call id.
                    calls.push(FunctionCall {
                        output_index,
                        call_id: str_field(item, "call_id"),
                        name: str_field(item, "name"),
                        partial_args: String::new(),
                        final_args: None,
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let output_index = data
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some(call) = call_at_mut(&mut calls, output_index)
                    && let Some(delta) = data.get("delta").and_then(Value::as_str)
                {
                    call.partial_args.push_str(delta);
                }
            }
            "response.output_item.done" => {
                let output_index = data
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                // A done item carries the full, final arguments string; prefer it
                // over the accumulated deltas when present.
                if let Some(args) = data
                    .get("item")
                    .and_then(|i| i.get("arguments"))
                    .and_then(Value::as_str)
                    && let Some(call) = call_at_mut(&mut calls, output_index)
                {
                    call.final_args = Some(args.to_string());
                }
            }
            "response.completed" => {
                completed = true;
                if let Some(usage) = data.get("response").and_then(|r| r.get("usage")) {
                    input_tokens = u32_field(usage, "input_tokens").unwrap_or(input_tokens);
                    output_tokens = u32_field(usage, "output_tokens").unwrap_or(output_tokens);
                }
            }
            // Other events (e.g. `response.output_item.added` for a text item,
            // unknown future events) carry no accumulation here.
            _ => {}
        }
    }

    if !completed {
        return Err(ParseError::MissingCompletion);
    }

    // Materialize turns: the single text turn first (render_codex emits all text
    // before any function-call items), then function calls in ascending
    // output_index — the order the client assembles the output array in.
    let mut turns = Vec::new();
    if !text.is_empty() {
        turns.push(Turn::Text(text));
    }
    calls.sort_by_key(|c| c.output_index);
    let has_calls = !calls.is_empty();
    for call in calls {
        // Prefer the authoritative `done` arguments; fall back to accumulated
        // delta fragments. An empty buffer means an empty arguments object.
        let raw = call.final_args.unwrap_or(call.partial_args);
        let args: Value = if raw.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&raw).map_err(|_| ParseError::InvalidToolArguments(raw.clone()))?
        };
        turns.push(Turn::ToolCall {
            id: call.call_id,
            name: call.name,
            args,
        });
    }

    // Codex carries no terminal stop field; infer it from the reply shape, the
    // same way render_codex chooses frames from a Reply's turns.
    let stop = if has_calls {
        StopReason::ToolCalls
    } else {
        StopReason::Stop
    };

    Ok(Reply {
        turns,
        usage: Usage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
        },
        stop,
    })
}

/// Find the in-progress function-call item with the given `output_index`.
/// Searches from the end since deltas target the most recently opened item.
fn call_at_mut(calls: &mut [FunctionCall], output_index: u64) -> Option<&mut FunctionCall> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.output_index == output_index)
}

/// Read a `u32` field from a JSON object, if present and in range.
fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value.get(key).and_then(Value::as_u64).map(|n| n as u32)
}

/// Read a string field from an optional JSON object, defaulting to empty.
fn str_field(value: Option<&Value>, key: &str) -> String {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{frames_to_body, render_codex};

    /// Render a reply to wire bytes the way the server would, for round-trips.
    fn rendered(reply: &Reply) -> Vec<u8> {
        frames_to_body(&render_codex(reply)).into_bytes()
    }

    #[test]
    fn round_trips_a_single_text_reply() {
        let reply = Reply::text("hello world");
        let parsed = parse_codex_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn round_trips_multiple_text_turns() {
        // render_codex emits one output_text.delta per text turn, all under the
        // same output item — so they reassemble into a single concatenated turn.
        let reply = Reply {
            turns: vec![Turn::Text("foo".to_string()), Turn::Text("bar".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let parsed = parse_codex_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("foobar".to_string())]);
        assert_eq!(parsed.stop, StopReason::Stop);
    }

    #[test]
    fn round_trips_usage() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage {
                prompt_tokens: 11,
                completion_tokens: 7,
            },
            stop: StopReason::Stop,
        };
        let parsed = parse_codex_sse(&rendered(&reply)).unwrap();
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
        let parsed = parse_codex_sse(&rendered(&reply)).unwrap();
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
        let parsed = parse_codex_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn thinking_turns_round_trip_as_their_visible_surface() {
        // render_codex skips Thinking turns entirely, so a reply with a thinking
        // + text turn renders the same as just the text turn.
        let reply = Reply {
            turns: vec![
                Turn::Thinking("hmm".to_string()),
                Turn::Text("answer".to_string()),
            ],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let parsed = parse_codex_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("answer".to_string())]);
    }

    #[test]
    fn concatenates_fragmented_text_deltas() {
        // A real capture streams text in fragments; they must concatenate.
        let stream = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"content_index\":0,\"delta\":\"Hel\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"content_index\":0,\"delta\":\"lo\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
        );
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("Hello".to_string())]);
        assert_eq!(parsed.usage.prompt_tokens, 3);
        assert_eq!(parsed.usage.completion_tokens, 2);
        assert_eq!(parsed.stop, StopReason::Stop);
    }

    #[test]
    fn concatenates_fragmented_tool_arguments() {
        // function_call_arguments.delta fragments join into one JSON object,
        // exactly as the Codex client reassembles them. The `done` item here
        // carries no arguments, so the accumulated fragments are used.
        let stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_9\",\"name\":\"search\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"item_id\":\"fc_1\",\"delta\":\"{\\\"q\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"item_id\":\"fc_1\",\"delta\":\"\\\"rust\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
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
    fn done_item_arguments_are_authoritative() {
        // When output_item.done carries the full arguments, they win over any
        // (here absent) accumulated deltas — matching how a real stream sends the
        // complete arguments on the done item.
        let stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_7\",\"name\":\"write\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_7\",\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"x\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
        assert_eq!(
            parsed.turns,
            vec![Turn::ToolCall {
                id: "call_7".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "x" }),
            }]
        );
    }

    #[test]
    fn drives_off_data_type_when_event_line_absent() {
        // A capture that dropped the `event:` line still parses via the data
        // payload's `"type"`, as the Anthropic parser does.
        let stream = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("hi".to_string())]);
    }

    #[test]
    fn tool_call_with_no_argument_deltas_is_an_empty_object() {
        let stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"ping\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"ping\"}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
        assert_eq!(
            parsed.turns,
            vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "ping".to_string(),
                args: serde_json::json!({}),
            }]
        );
        assert_eq!(parsed.stop, StopReason::ToolCalls);
    }

    #[test]
    fn multiple_tool_calls_emit_in_output_index_order() {
        // Even if a later output_index's frames interleave, calls materialize in
        // ascending output_index order.
        let stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":2,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_b\",\"name\":\"read\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_a\",\"name\":\"read\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":1,\"delta\":\"{\\\"p\\\":\\\"a\\\"}\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":2,\"delta\":\"{\\\"p\\\":\\\"b\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(
            parsed.turns[0],
            Turn::ToolCall {
                id: "call_a".to_string(),
                name: "read".to_string(),
                args: serde_json::json!({ "p": "a" }),
            }
        );
        assert_eq!(
            parsed.turns[1],
            Turn::ToolCall {
                id: "call_b".to_string(),
                name: "read".to_string(),
                args: serde_json::json!({ "p": "b" }),
            }
        );
    }

    #[test]
    fn missing_completion_is_an_error() {
        // No response.completed: the stream never terminated.
        let stream = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
        );
        assert_eq!(
            parse_codex_sse(stream.as_bytes()),
            Err(ParseError::MissingCompletion)
        );
    }

    #[test]
    fn invalid_tool_arguments_is_an_error() {
        let stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"n\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":1,\"delta\":\"{not json\"}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        assert_eq!(
            parse_codex_sse(stream.as_bytes()),
            Err(ParseError::InvalidToolArguments("{not json".to_string()))
        );
    }

    #[test]
    fn empty_reply_completes_with_a_normal_stop() {
        // A bare response.completed (no output items) parses to an empty,
        // normally-stopped reply rather than erroring.
        let stream = "event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";
        let parsed = parse_codex_sse(stream.as_bytes()).unwrap();
        assert!(parsed.turns.is_empty());
        assert_eq!(parsed.stop, StopReason::Stop);
        assert_eq!(parsed.usage.prompt_tokens, 1);
    }
}
