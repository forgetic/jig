//! Dialect-agnostic core for `jig`.
//!
//! This crate is intentionally async-free: it owns the canonical [`Reply`] /
//! [`Turn`] model, the [`Script`] that yields a [`Reply`] per request, and the
//! per-dialect SSE renderers (just OpenAI for M1). Everything here is pure and
//! synchronous so it unit-tests without a runtime.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub mod conform;
pub mod parse;
pub mod render;
pub mod request;
pub mod script_file;

pub use parse::{
    AnthropicParseError, CodexParseError, OpenAiParseError, SseEvent, parse_anthropic_sse,
    parse_codex_sse, parse_openai_sse, parse_sse,
};
pub use render::{render_anthropic, render_codex, render_openai};
pub use request::{Dialect, RequestView, ViewMessage};
pub use script_file::{ReplySpec, ScriptFile, ScriptFileError, StopSpec, ToolCallSpec, TurnSpec};

/// The jig repository's `fixtures/` root, resolved from this crate's
/// compile-time manifest dir.
///
/// Valid only for workspace and path-dependency consumers (jig is never
/// published, so the source tree — and with it `fixtures/` — is always on
/// disk). This is how a **subject SDK** anchors its conformance tests to jig's
/// authoritative templates from its own repository without hardcoding a
/// relative checkout layout: the path dependency already pins where jig lives.
pub fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// One thing the fake model emits within a single assistant turn.
///
/// M1 only renders [`Turn::Text`]; the other variants are part of the canonical
/// model so downstream milestones (thinking blocks, tool-call rendering) can
/// extend the renderers without reshaping core types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Turn {
    /// Plain assistant text.
    Text(String),
    /// Reasoning / "thinking" content (rendered in M5).
    Thinking(String),
    /// A tool call the model wants the caller to execute (rendered in M5).
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
}

/// Canned input/output token counts. Never computed — `jig` does not count
/// tokens (see bootstrap.md "Non-goals").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl Usage {
    /// `prompt_tokens + completion_tokens`, surfaced to dialects that emit a
    /// `total_tokens` field (OpenAI).
    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

impl Default for Usage {
    fn default() -> Self {
        Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
        }
    }
}

/// Why a streamed reply ended. Maps to each dialect's terminal stop field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// Normal completion (`finish_reason: "stop"`).
    Stop,
    /// The reply ends with tool calls the caller must execute.
    ToolCalls,
    /// The model signalled an error.
    Error,
}

impl StopReason {
    /// The OpenAI chat-completions `finish_reason` string for this stop reason.
    pub fn openai_finish_reason(&self) -> &'static str {
        match self {
            StopReason::Stop => "stop",
            StopReason::ToolCalls => "tool_calls",
            StopReason::Error => "stop",
        }
    }

    /// The Anthropic messages `stop_reason` string for this stop reason.
    ///
    /// Anthropic signals a normal end-of-turn with `end_turn` and a tool-use
    /// hand-off with `tool_use`; there is no dedicated error value in the
    /// streamed `message_delta`, so an errored reply also ends as `end_turn`.
    pub fn anthropic_stop_reason(&self) -> &'static str {
        match self {
            StopReason::Stop => "end_turn",
            StopReason::ToolCalls => "tool_use",
            StopReason::Error => "end_turn",
        }
    }
}

/// A single assistant response: one HTTP request maps to one streamed reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub turns: Vec<Turn>,
    pub usage: Usage,
    pub stop: StopReason,
}

impl Reply {
    /// Build a single-text-turn reply that stops normally — the common case for
    /// `run_decision`-style callers and the M1 default.
    pub fn text(content: impl Into<String>) -> Self {
        Reply {
            turns: vec![Turn::Text(content.into())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        }
    }
}

/// A request recorded for later assertion.
///
/// Captured per incoming request behind shared state and surfaced via
/// `FakeLlm::requests()` (in `jig-server`) so a synchronous test can assert what
/// the client actually sent — path, method, dialect, the raw body, and the
/// normalized [`RequestView`] projection (see bootstrap.md "Public API").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRequest {
    /// Request path, query string stripped (e.g. `/chat/completions`).
    pub path: String,
    /// HTTP method (e.g. `POST`).
    pub method: String,
    /// The raw request body bytes, verbatim.
    pub body: Vec<u8>,
    /// The normalized projection of the body, if the route mapped to a dialect.
    /// `None` for routes without a dialect projection (e.g. a `404` path).
    pub view: Option<RequestView>,
}

impl RecordedRequest {
    /// The raw body as a UTF-8 string (lossy). Convenience for assertions.
    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// Decides which [`Reply`] to serve for a given request.
///
/// The server holds a `Script` behind shared state and calls
/// [`Script::next_reply`] per request, passing the parsed [`RequestView`], so
/// adding variants does not change the server seam.
pub enum Script {
    /// Serve the same reply for every request.
    Fixed(Reply),
    /// Serve replies in order; once exhausted, the last reply repeats for every
    /// further request. An empty sequence is treated as a single default
    /// [`Reply::text`] so a misconfigured script never panics the server.
    ///
    /// The cursor is interior-mutable so the server can keep the script behind a
    /// shared `Arc` and advance it per request without `&mut` access.
    Sequence {
        replies: Vec<Reply>,
        cursor: Mutex<usize>,
    },
    /// Decide the reply from the parsed request — turn count, last message,
    /// model, etc. The closure must be `Send + Sync` because it runs on the
    /// dedicated runtime thread while the handle lives on the caller's thread.
    Rule(Box<dyn Fn(&RequestView) -> Reply + Send + Sync>),
}

impl Script {
    /// Build a [`Script::Sequence`] from an ordered list of replies.
    pub fn sequence(replies: Vec<Reply>) -> Self {
        Script::Sequence {
            replies,
            cursor: Mutex::new(0),
        }
    }

    /// Build a [`Script::Rule`] from a decision closure.
    pub fn rule(f: impl Fn(&RequestView) -> Reply + Send + Sync + 'static) -> Self {
        Script::Rule(Box::new(f))
    }

    /// Produce the reply for the next request.
    ///
    /// `view` is the normalized projection of the request body. `Fixed` ignores
    /// it; `Sequence` advances its cursor; `Rule` decides from it.
    pub fn next_reply(&self, view: &RequestView) -> Reply {
        match self {
            Script::Fixed(reply) => reply.clone(),
            Script::Sequence { replies, cursor } => {
                if replies.is_empty() {
                    return Reply::text("");
                }
                // Lock to read+advance the cursor. The lock is uncontended on
                // the single-threaded runtime; recover from poisoning rather
                // than panicking so one bad request can't wedge the server.
                let mut idx = cursor.lock().unwrap_or_else(|p| p.into_inner());
                let chosen = replies[*idx].clone();
                // Advance, clamping at the last index so it repeats once
                // exhausted (issue #4: "the last one repeats once exhausted").
                if *idx + 1 < replies.len() {
                    *idx += 1;
                }
                chosen
            }
            Script::Rule(f) => f(view),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal OpenAI view with the given prior-tool-result count, for
    /// exercising scripts without standing up a server.
    fn view_with_turns(prior_tool_results: usize) -> RequestView {
        RequestView::new(
            Dialect::OpenAi,
            Some("fake".to_string()),
            vec![],
            prior_tool_results,
        )
    }

    #[test]
    fn usage_total_is_the_sum() {
        let usage = Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
        };
        assert_eq!(usage.total_tokens(), 7);
    }

    #[test]
    fn stop_reasons_map_to_each_dialect() {
        assert_eq!(StopReason::Stop.openai_finish_reason(), "stop");
        assert_eq!(StopReason::ToolCalls.openai_finish_reason(), "tool_calls");
        assert_eq!(StopReason::Stop.anthropic_stop_reason(), "end_turn");
        assert_eq!(StopReason::ToolCalls.anthropic_stop_reason(), "tool_use");
    }

    #[test]
    fn reply_text_is_a_single_stop_turn() {
        let reply = Reply::text("hi");
        assert_eq!(reply.turns, vec![Turn::Text("hi".to_string())]);
        assert_eq!(reply.stop, StopReason::Stop);
    }

    #[test]
    fn fixed_script_repeats_the_same_reply() {
        let script = Script::Fixed(Reply::text("same"));
        let view = view_with_turns(0);
        assert_eq!(script.next_reply(&view), script.next_reply(&view));
        assert_eq!(script.next_reply(&view), Reply::text("same"));
    }

    #[test]
    fn sequence_serves_in_order_then_repeats_the_last() {
        let script = Script::sequence(vec![
            Reply::text("first"),
            Reply::text("second"),
            Reply::text("third"),
        ]);
        let view = view_with_turns(0);
        assert_eq!(script.next_reply(&view), Reply::text("first"));
        assert_eq!(script.next_reply(&view), Reply::text("second"));
        assert_eq!(script.next_reply(&view), Reply::text("third"));
        // Exhausted: the last reply repeats from here on.
        assert_eq!(script.next_reply(&view), Reply::text("third"));
        assert_eq!(script.next_reply(&view), Reply::text("third"));
    }

    #[test]
    fn empty_sequence_yields_an_empty_text_reply() {
        let script = Script::sequence(vec![]);
        let view = view_with_turns(0);
        assert_eq!(script.next_reply(&view), Reply::text(""));
    }

    #[test]
    fn rule_script_branches_on_the_request_view() {
        let script = Script::rule(|view| {
            if view.prior_tool_results == 0 {
                Reply {
                    turns: vec![Turn::ToolCall {
                        id: "call_1".to_string(),
                        name: "write".to_string(),
                        args: serde_json::json!({ "path": "x" }),
                    }],
                    usage: Usage::default(),
                    stop: StopReason::ToolCalls,
                }
            } else {
                Reply::text("done")
            }
        });

        // Turn 1: no prior tool results → a tool call.
        let first = script.next_reply(&view_with_turns(0));
        assert_eq!(first.stop, StopReason::ToolCalls);

        // Turn 2: one prior tool result → the final text.
        let second = script.next_reply(&view_with_turns(1));
        assert_eq!(second, Reply::text("done"));
    }

    #[test]
    fn recorded_request_exposes_body_as_str() {
        let recorded = RecordedRequest {
            path: "/chat/completions".to_string(),
            method: "POST".to_string(),
            body: b"{\"model\":\"fake\"}".to_vec(),
            view: None,
        };
        assert_eq!(recorded.body_str(), "{\"model\":\"fake\"}");
    }
}
