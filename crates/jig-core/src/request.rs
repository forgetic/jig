//! Dialect body parsing → [`RequestView`].
//!
//! The server parses each incoming request body once and projects it into a
//! normalized, read-only [`RequestView`] so a [`crate::Script::Rule`] can branch
//! on the request (turn count, last message, model) without each rule
//! re-parsing three different wire schemas. M2 ships the OpenAI/DeepSeek
//! chat-completions projection only; M3/M4 add Anthropic and Codex projections
//! into the *same* view shape (see bootstrap.md "Scripting / driving model").

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which wire dialect a request arrived on. Determined by the route, not the
/// body (see the route table in bootstrap.md "Why this shape").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dialect {
    /// OpenAI / DeepSeek chat-completions (`POST /chat/completions`).
    OpenAi,
    /// Anthropic messages (`POST /v1/messages`). Projected in M3.
    Anthropic,
    /// OpenAI Codex responses (`POST /backend-api/codex/responses`).
    /// Projected in M4.
    Codex,
}

/// One message in the normalized view: a role and its flattened text content.
///
/// Content is reduced to a single string regardless of whether the dialect sent
/// a plain string or an array of content parts — rules only need the text, and
/// keeping the shape flat avoids leaking per-dialect part structures into the
/// view. The original body is preserved verbatim on [`crate::RecordedRequest`]
/// for tests that need the raw shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewMessage {
    pub role: String,
    pub content: String,
}

/// A normalized, read-only projection of an incoming request.
///
/// Dialect-agnostic by design: every dialect projects into this same shape so
/// rules written against it keep working as M3/M4 add dialects. It exposes only
/// what a rule needs to decide a reply — not a faithful round-trip of the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestView {
    /// The dialect the request arrived on (from the route).
    pub dialect: Dialect,
    /// The requested model id, if the body carried one.
    pub model: Option<String>,
    /// The conversation messages / instructions, in order.
    pub messages: Vec<ViewMessage>,
    /// How many prior tool results appear in this request — i.e. how many turns
    /// of the tool-use loop have already completed. Lets a rule branch on turn
    /// count without re-parsing tool schemas (issue #4 acceptance).
    pub prior_tool_results: usize,
}

impl RequestView {
    /// The most recent message in the conversation, if any. Convenience for
    /// rules that branch on "what did the caller just say".
    pub fn last_message(&self) -> Option<&ViewMessage> {
        self.messages.last()
    }

    /// Build a view directly for tests / future in-process callers.
    pub fn new(
        dialect: Dialect,
        model: Option<String>,
        messages: Vec<ViewMessage>,
        prior_tool_results: usize,
    ) -> Self {
        RequestView {
            dialect,
            model,
            messages,
            prior_tool_results,
        }
    }
}

/// Flatten a chat-completions `content` field to a single string.
///
/// The OpenAI dialect allows `content` to be either a plain string or an array
/// of typed parts (`{ "type": "text", "text": "…" }`). Both collapse to their
/// concatenated text; anything else flattens to the empty string.
fn flatten_openai_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Project an OpenAI / DeepSeek chat-completions request body into a
/// [`RequestView`].
///
/// `body` is the raw request bytes. A body that is not valid JSON, or that
/// lacks the expected fields, still yields a usable (if sparse) view rather than
/// an error — `jig` is a permissive test double, and a rule can branch on an
/// empty `messages` just as well.
///
/// Tool-result accounting: chat-completions feeds tool outputs back as messages
/// with `role: "tool"`, so the count of prior tool results is the number of
/// `tool`-role messages.
pub fn parse_openai(body: &[u8]) -> RequestView {
    let json: Value = serde_json::from_slice(body).unwrap_or(Value::Null);

    let model = json
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut messages = Vec::new();
    let mut prior_tool_results = 0usize;

    if let Some(arr) = json.get("messages").and_then(Value::as_array) {
        for msg in arr {
            let role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if role == "tool" {
                prior_tool_results += 1;
            }
            let content = msg
                .get("content")
                .map(flatten_openai_content)
                .unwrap_or_default();
            messages.push(ViewMessage { role, content });
        }
    }

    RequestView {
        dialect: Dialect::OpenAi,
        model,
        messages,
        prior_tool_results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_model_and_messages_in_order() {
        let body = json!({
            "model": "deepseek-chat",
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": "hi" },
            ],
        })
        .to_string();

        let view = parse_openai(body.as_bytes());
        assert_eq!(view.dialect, Dialect::OpenAi);
        assert_eq!(view.model.as_deref(), Some("deepseek-chat"));
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[0].role, "system");
        assert_eq!(view.messages[0].content, "be terse");
        assert_eq!(view.last_message().unwrap().content, "hi");
    }

    #[test]
    fn counts_tool_role_messages_as_prior_tool_results() {
        let body = json!({
            "model": "fake",
            "messages": [
                { "role": "user", "content": "do it" },
                { "role": "assistant", "content": "" },
                { "role": "tool", "content": "result-1" },
                { "role": "assistant", "content": "" },
                { "role": "tool", "content": "result-2" },
            ],
        })
        .to_string();

        let view = parse_openai(body.as_bytes());
        assert_eq!(view.prior_tool_results, 2);
    }

    #[test]
    fn flattens_array_content_parts_to_text() {
        let body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "foo" },
                        { "type": "text", "text": "bar" },
                    ],
                },
            ],
        })
        .to_string();

        let view = parse_openai(body.as_bytes());
        assert_eq!(view.messages[0].content, "foobar");
    }

    #[test]
    fn invalid_body_yields_an_empty_view_not_an_error() {
        let view = parse_openai(b"not json at all");
        assert_eq!(view.dialect, Dialect::OpenAi);
        assert!(view.model.is_none());
        assert!(view.messages.is_empty());
        assert_eq!(view.prior_tool_results, 0);
        assert!(view.last_message().is_none());
    }
}
