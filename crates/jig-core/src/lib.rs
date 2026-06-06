//! Dialect-agnostic core for `jig`.
//!
//! This crate is intentionally async-free: it owns the canonical [`Reply`] /
//! [`Turn`] model, the [`Script`] that yields a [`Reply`] per request, and the
//! per-dialect SSE renderers (just OpenAI for M1). Everything here is pure and
//! synchronous so it unit-tests without a runtime.

use serde::{Deserialize, Serialize};

pub mod render;

pub use render::render_openai;

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

/// Decides which [`Reply`] to serve for a given request.
///
/// M1 only implements [`Script::Fixed`]; `Sequence`/`Rule` land in M2. The
/// server holds a `Script` behind shared state and calls [`Script::next_reply`]
/// per request, so adding variants later does not change the server seam.
pub enum Script {
    /// Serve the same reply for every request.
    Fixed(Reply),
}

impl Script {
    /// Produce the reply for the next request. M2 will thread a parsed
    /// `RequestView` through here for `Sequence`/`Rule`.
    pub fn next_reply(&self) -> Reply {
        match self {
            Script::Fixed(reply) => reply.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_total_is_the_sum() {
        let usage = Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
        };
        assert_eq!(usage.total_tokens(), 7);
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
        assert_eq!(script.next_reply(), script.next_reply());
        assert_eq!(script.next_reply(), Reply::text("same"));
    }
}
