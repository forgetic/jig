//! Anthropic messages SSE → canonical [`Reply`] parser.
//!
//! The inverse of [`crate::render::render_anthropic`]. It consumes the bytes of
//! an Anthropic `/v1/messages` event stream — whether produced by jig's own
//! renderer or captured from real Claude Code traffic by the recorder (#18) —
//! and reconstructs the canonical [`Reply`]: text turns, tool-call turns,
//! [`Usage`], and the [`StopReason`].
//!
//! # Event model
//!
//! It folds Anthropic's streamed block protocol (see bootstrap.md "Minimal SSE
//! sequences" and the issue #15 scope) back into the model:
//!
//! - `message_start` — opens the message; `usage.input_tokens` seeds prompt
//!   tokens, `usage.output_tokens` (usually 0 here) seeds completion tokens.
//! - `content_block_start` — opens a block at an `index`. A `text` block becomes
//!   a [`Turn::Text`]; a `tool_use` block (carrying `id`/`name`) becomes a
//!   [`Turn::ToolCall`] whose input is accumulated from its deltas.
//! - `content_block_delta` — `text_delta` appends to the open text block;
//!   `input_json_delta` appends a `partial_json` fragment to the open tool block
//!   (the client concatenates fragments, then parses once — so do we).
//! - `content_block_stop` — closes a block, emitting its turn in block-index
//!   order.
//! - `message_delta` — carries the terminal `stop_reason` and the final
//!   `output_tokens`.
//! - `message_stop` — terminates the stream.
//! - `ping` (and any unrecognized event) — ignored, as the client ignores it.
//!
//! Blocks are emitted in ascending `index` order regardless of the order their
//! `stop` events arrive, matching how a client assembles the final message.
//! Consecutive `text_delta`s within a block are concatenated into a single
//! [`Turn::Text`], so a capture that streamed `"foo"` as `"f"`,`"oo"` parses to
//! the same `Reply` as a one-shot `"foo"` render — the property T1 relies on.
//!
//! # Faithfulness to `render_anthropic`
//!
//! `render_anthropic` always opens a text block at index 0 even when there are
//! no text turns, then closes it. Parsing that back would yield an empty-string
//! text turn the original `Reply` never had. To make round-tripping exact, an
//! empty text block contributes **no** turn (an empty text turn carries no
//! surface anyway). A text block with content always contributes its turn.

use serde_json::Value;

use crate::{Reply, StopReason, Turn, Usage};

use super::sse::parse_sse;

/// Why parsing an Anthropic SSE stream failed.
///
/// Parsing is deliberately lenient about *extra* structure (unknown events,
/// extra fields, `ping`s) but strict about the invariants a downstream consumer
/// relies on: a terminal stop reason must be present, and a `tool_use` block's
/// accumulated input must be valid JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The stream carried no `message_delta` with a `stop_reason`, so the reply
    /// has no terminal state to map. A complete Anthropic stream always sends
    /// one before `message_stop`.
    MissingStopReason,
    /// A `tool_use` block's concatenated `input_json_delta` fragments did not
    /// parse as JSON. The string is the accumulated, unparseable input.
    InvalidToolInput(String),
    /// A `message_delta` carried a `stop_reason` value this model has no mapping
    /// for. The string is the unrecognized wire value.
    UnknownStopReason(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingStopReason => {
                write!(f, "stream carried no message_delta stop_reason")
            }
            ParseError::InvalidToolInput(input) => {
                write!(f, "tool_use input is not valid JSON: {input}")
            }
            ParseError::UnknownStopReason(value) => {
                write!(f, "unrecognized stop_reason: {value}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Map an Anthropic wire `stop_reason` to the canonical [`StopReason`].
///
/// `end_turn`, `max_tokens`, and `stop_sequence` are all normal completions
/// (`render_anthropic` emits `end_turn` for [`StopReason::Stop`]); `tool_use`
/// is a tool hand-off. Anything else is an explicit error rather than a silent
/// default, so an unexpected provider value surfaces instead of masquerading as
/// a clean stop.
fn map_stop_reason(value: &str) -> Result<StopReason, ParseError> {
    match value {
        "end_turn" | "max_tokens" | "stop_sequence" => Ok(StopReason::Stop),
        "tool_use" => Ok(StopReason::ToolCalls),
        other => Err(ParseError::UnknownStopReason(other.to_string())),
    }
}

/// A content block being assembled, keyed by its `index` in arrival order.
enum Block {
    /// A text block accumulating `text_delta`s.
    Text { index: u64, text: String },
    /// A `tool_use` block accumulating `input_json_delta` `partial_json`.
    ToolUse {
        index: u64,
        id: String,
        name: String,
        partial_json: String,
    },
}

impl Block {
    fn index(&self) -> u64 {
        match self {
            Block::Text { index, .. } | Block::ToolUse { index, .. } => *index,
        }
    }
}

/// Parse an Anthropic `/v1/messages` SSE byte stream into a canonical [`Reply`].
///
/// Pure and synchronous. Lenient about unknown events and extra fields; strict
/// about the terminal `stop_reason` and tool-input validity (see [`ParseError`]).
pub fn parse_anthropic_sse(bytes: &[u8]) -> Result<Reply, ParseError> {
    let events = parse_sse(bytes);

    let mut blocks: Vec<Block> = Vec::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut stop_reason: Option<StopReason> = None;

    for ev in &events {
        // Anthropic carries the event name on the `event:` line; the `data:`
        // payload also has a `"type"`. Drive off the event-line name (what the
        // client dispatches on), falling back to the data `type` if a capture
        // omitted the event line.
        let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
        let kind = ev
            .event
            .as_deref()
            .or_else(|| data.get("type").and_then(Value::as_str))
            .unwrap_or("");

        match kind {
            "message_start" => {
                if let Some(usage) = data.get("message").and_then(|m| m.get("usage")) {
                    input_tokens = u32_field(usage, "input_tokens").unwrap_or(input_tokens);
                    output_tokens = u32_field(usage, "output_tokens").unwrap_or(output_tokens);
                }
            }
            "content_block_start" => {
                let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = data.get("content_block");
                let block_type = block
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("text");
                match block_type {
                    "tool_use" => blocks.push(Block::ToolUse {
                        index,
                        id: str_field(block, "id"),
                        name: str_field(block, "name"),
                        partial_json: String::new(),
                    }),
                    // Treat text and any non-tool block (e.g. `thinking`) as a
                    // text-accumulating block; render_anthropic only emits
                    // `text` and `tool_use`, and thinking carries no canonical
                    // surface, so this stays faithful while tolerating captures.
                    _ => blocks.push(Block::Text {
                        index,
                        text: String::new(),
                    }),
                }
            }
            "content_block_delta" => {
                let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = data.get("delta");
                let delta_type = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        if let Some(Block::Text { text, .. }) = block_at_mut(&mut blocks, index) {
                            text.push_str(str_field(delta, "text").as_str());
                        }
                    }
                    "input_json_delta" => {
                        if let Some(Block::ToolUse { partial_json, .. }) =
                            block_at_mut(&mut blocks, index)
                        {
                            partial_json.push_str(str_field(delta, "partial_json").as_str());
                        }
                    }
                    // Other delta types (e.g. `thinking_delta`) carry no
                    // canonical surface; ignore.
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    stop_reason = Some(map_stop_reason(reason)?);
                }
                if let Some(usage) = data.get("usage")
                    && let Some(out) = u32_field(usage, "output_tokens")
                {
                    output_tokens = out;
                }
            }
            // `content_block_stop`, `message_stop`, `ping`, and anything else
            // need no accumulation here — blocks are materialized after the fold.
            _ => {}
        }
    }

    let stop = stop_reason.ok_or(ParseError::MissingStopReason)?;

    // Materialize turns in ascending block index, matching how a client
    // assembles the final message content array.
    blocks.sort_by_key(Block::index);
    let mut turns = Vec::new();
    for block in blocks {
        match block {
            // An empty text block contributes no turn — render_anthropic always
            // opens an index-0 text block even with no text, so dropping the
            // empty case makes render → parse an exact round-trip.
            Block::Text { text, .. } => {
                if !text.is_empty() {
                    turns.push(Turn::Text(text));
                }
            }
            Block::ToolUse {
                id,
                name,
                partial_json,
                ..
            } => {
                // The client concatenates partial_json fragments, then parses
                // once. An empty buffer means an empty input object.
                let args: Value = if partial_json.trim().is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&partial_json)
                        .map_err(|_| ParseError::InvalidToolInput(partial_json.clone()))?
                };
                turns.push(Turn::ToolCall { id, name, args });
            }
        }
    }

    Ok(Reply {
        turns,
        usage: Usage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
        },
        stop,
    })
}

/// Find the in-progress block with the given `index`, for appending deltas.
/// Searches from the end since deltas target the most recently opened block.
fn block_at_mut(blocks: &mut [Block], index: u64) -> Option<&mut Block> {
    blocks.iter_mut().rev().find(|b| b.index() == index)
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
    use crate::render::{frames_to_body, render_anthropic};

    /// Render a reply to wire bytes the way the server would, for round-trips.
    fn rendered(reply: &Reply) -> Vec<u8> {
        frames_to_body(&render_anthropic(reply)).into_bytes()
    }

    #[test]
    fn round_trips_a_single_text_reply() {
        let reply = Reply::text("hello world");
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn round_trips_multiple_text_turns() {
        // render_anthropic emits one text_delta per text turn, all in the index-0
        // block — so they reassemble into a single concatenated text turn.
        let reply = Reply {
            turns: vec![Turn::Text("foo".to_string()), Turn::Text("bar".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
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
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.usage.prompt_tokens, 11);
        assert_eq!(parsed.usage.completion_tokens, 7);
        assert_eq!(parsed.stop, StopReason::Stop);
    }

    #[test]
    fn round_trips_a_single_tool_call() {
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "toolu_1".to_string(),
                name: "write".to_string(),
                args: serde_json::json!({ "path": "out.txt", "content": "hi" }),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn round_trips_text_then_multiple_tool_calls() {
        let reply = Reply {
            turns: vec![
                Turn::Text("let me look".to_string()),
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
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn empty_text_block_contributes_no_turn() {
        // render_anthropic always opens an index-0 text block; with only a tool
        // call, that block is empty and must not become a phantom text turn.
        let reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "toolu_1".to_string(),
                name: "noop".to_string(),
                args: serde_json::json!({}),
            }],
            usage: Usage::default(),
            stop: StopReason::ToolCalls,
        };
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns.len(), 1);
        assert!(matches!(parsed.turns[0], Turn::ToolCall { .. }));
    }

    #[test]
    fn thinking_turns_round_trip_as_their_visible_surface() {
        // render_anthropic skips Thinking turns entirely, so a reply with a
        // thinking + text turn renders the same as just the text turn.
        let reply = Reply {
            turns: vec![
                Turn::Thinking("hmm".to_string()),
                Turn::Text("answer".to_string()),
            ],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let parsed = parse_anthropic_sse(&rendered(&reply)).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("answer".to_string())]);
    }

    #[test]
    fn concatenates_fragmented_text_deltas() {
        // A real capture streams text in fragments; they must concatenate.
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let parsed = parse_anthropic_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("Hello".to_string())]);
        assert_eq!(parsed.usage.prompt_tokens, 3);
        assert_eq!(parsed.usage.completion_tokens, 2);
    }

    #[test]
    fn concatenates_fragmented_tool_input_json() {
        // input_json_delta partial_json arrives in fragments that join into one
        // JSON object, exactly as the Anthropic client reassembles it.
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"search\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"rust\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let parsed = parse_anthropic_sse(stream.as_bytes()).unwrap();
        assert_eq!(
            parsed.turns,
            vec![Turn::ToolCall {
                id: "toolu_9".to_string(),
                name: "search".to_string(),
                args: serde_json::json!({ "q": "rust" }),
            }]
        );
        assert_eq!(parsed.stop, StopReason::ToolCalls);
    }

    #[test]
    fn ignores_ping_events() {
        let stream = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let parsed = parse_anthropic_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns, vec![Turn::Text("hi".to_string())]);
    }

    #[test]
    fn maps_alternate_normal_stop_reasons_to_stop() {
        // The `STOP` placeholder is substituted per reason; building the stream
        // with `replace` keeps the JSON braces literal (no format escaping).
        let template = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"STOP\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        for reason in ["end_turn", "max_tokens", "stop_sequence"] {
            let stream = template.replace("STOP", reason);
            let parsed = parse_anthropic_sse(stream.as_bytes()).unwrap();
            assert_eq!(parsed.stop, StopReason::Stop, "stop_reason {reason}");
        }
    }

    #[test]
    fn missing_stop_reason_is_an_error() {
        let stream = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        assert_eq!(
            parse_anthropic_sse(stream.as_bytes()),
            Err(ParseError::MissingStopReason)
        );
    }

    #[test]
    fn unknown_stop_reason_is_an_error() {
        let stream = concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"refusal\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        assert_eq!(
            parse_anthropic_sse(stream.as_bytes()),
            Err(ParseError::UnknownStopReason("refusal".to_string()))
        );
    }

    #[test]
    fn invalid_tool_input_is_an_error() {
        let stream = concat!(
            "event: content_block_start\n",
            "data: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"n\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{not json\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        assert_eq!(
            parse_anthropic_sse(stream.as_bytes()),
            Err(ParseError::InvalidToolInput("{not json".to_string()))
        );
    }

    #[test]
    fn tool_use_with_no_input_deltas_is_an_empty_object() {
        let stream = concat!(
            "event: content_block_start\n",
            "data: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"ping\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let parsed = parse_anthropic_sse(stream.as_bytes()).unwrap();
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
    fn blocks_emit_in_index_order_even_if_stops_interleave() {
        // Index 1 opens before index 0 closes; turns still come out 0 then 1.
        let stream = concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"first\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"n\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let parsed = parse_anthropic_sse(stream.as_bytes()).unwrap();
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0], Turn::Text("first".to_string()));
        assert!(matches!(parsed.turns[1], Turn::ToolCall { .. }));
    }
}
