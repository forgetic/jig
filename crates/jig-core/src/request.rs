//! Dialect body parsing → [`RequestView`].
//!
//! The server parses each incoming request body once and projects it into a
//! normalized, read-only [`RequestView`] so a [`crate::Script::Rule`] can branch
//! on the request (turn count, last message, model) without each rule
//! re-parsing three different wire schemas. M2 ships the OpenAI/DeepSeek
//! chat-completions projection; M3 adds Anthropic and M4 adds Codex, all
//! projecting into the *same* view shape (see bootstrap.md "Scripting / driving
//! model").

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

/// Flatten an Anthropic message `content` field to a single string.
///
/// The Anthropic dialect allows `content` to be either a plain string or an
/// array of typed blocks. Only `text` blocks contribute to the flattened text;
/// `tool_use` / `tool_result` / other block types are ignored here (their
/// presence is accounted for separately). Anything unexpected flattens to the
/// empty string.
fn flatten_anthropic_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Count `tool_result` blocks in an Anthropic message `content` field.
///
/// Anthropic feeds tool outputs back as `user`-role messages whose `content`
/// array contains `{ "type": "tool_result", ... }` blocks. A single user turn
/// can carry several, so we count blocks, not messages.
fn count_anthropic_tool_results(content: &Value) -> usize {
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .count(),
        _ => 0,
    }
}

/// Flatten an Anthropic top-level `system` field to a single string.
///
/// `system` is a top-level sibling of `messages` (not a message), and may be a
/// plain string or an array of `text` blocks. Both collapse to their text.
fn flatten_anthropic_system(system: &Value) -> String {
    flatten_anthropic_content(system)
}

/// Project an Anthropic messages request body into a [`RequestView`].
///
/// `body` is the raw request bytes. Like [`parse_openai`], a body that is not
/// valid JSON, or that lacks the expected fields, still yields a usable (if
/// sparse) view rather than an error.
///
/// Normalization choices that keep the view dialect-agnostic:
/// - The top-level `system` prompt is projected as a leading
///   `ViewMessage { role: "system", .. }` so rules see it the same way as the
///   OpenAI `system` message.
/// - Tool-result accounting: Anthropic feeds tool outputs back as `user`-role
///   messages carrying `tool_result` content blocks, so the count of prior tool
///   results is the number of `tool_result` blocks across all messages (not the
///   number of messages).
pub fn parse_anthropic(body: &[u8]) -> RequestView {
    let json: Value = serde_json::from_slice(body).unwrap_or(Value::Null);

    let model = json
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut messages = Vec::new();
    let mut prior_tool_results = 0usize;

    // Project the top-level system prompt (if any) as a leading system message.
    if let Some(system) = json.get("system") {
        let content = flatten_anthropic_system(system);
        if !content.is_empty() {
            messages.push(ViewMessage {
                role: "system".to_string(),
                content,
            });
        }
    }

    if let Some(arr) = json.get("messages").and_then(Value::as_array) {
        for msg in arr {
            let role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let content_value = msg.get("content");
            if let Some(content) = content_value {
                prior_tool_results += count_anthropic_tool_results(content);
            }
            let content = content_value
                .map(flatten_anthropic_content)
                .unwrap_or_default();
            messages.push(ViewMessage { role, content });
        }
    }

    RequestView {
        dialect: Dialect::Anthropic,
        model,
        messages,
        prior_tool_results,
    }
}

/// Flatten a Codex responses input item `content` field to a single string.
///
/// A Codex `input` message item carries a `content` array of typed parts
/// (`{ "type": "input_text" | "output_text", "text": "…" }`). Both text part
/// kinds contribute their text; anything else is ignored. A plain-string
/// `content` is also accepted for permissiveness.
fn flatten_codex_content(content: &Value) -> String {
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

/// Project an OpenAI Codex responses request body into a [`RequestView`].
///
/// `body` is the raw request bytes. Like [`parse_openai`], a body that is not
/// valid JSON, or that lacks the expected fields, still yields a usable (if
/// sparse) view rather than an error.
///
/// Normalization choices that keep the view dialect-agnostic:
/// - The top-level `instructions` string is projected as a leading
///   `ViewMessage { role: "system", .. }` so rules see it the same way as the
///   OpenAI `system` message and the Anthropic `system` prompt.
/// - The `input` array carries the conversation. `message` items project to a
///   `ViewMessage` with their flattened text; their `role` is preserved.
/// - Tool-result accounting: Codex feeds tool outputs back as `input` items with
///   `type: "function_call_output"`, so the count of prior tool results is the
///   number of those items.
pub fn parse_codex(body: &[u8]) -> RequestView {
    let json: Value = serde_json::from_slice(body).unwrap_or(Value::Null);

    let model = json
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut messages = Vec::new();
    let mut prior_tool_results = 0usize;

    // Project the top-level instructions (if any) as a leading system message.
    if let Some(instructions) = json.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(ViewMessage {
            role: "system".to_string(),
            content: instructions.to_string(),
        });
    }

    if let Some(arr) = json.get("input").and_then(Value::as_array) {
        for item in arr {
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("message");
            if item_type == "function_call_output" {
                prior_tool_results += 1;
                continue;
            }
            // Non-message items (e.g. a bare `function_call`) carry no text for
            // the view; skip them.
            if item_type != "message" {
                continue;
            }
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let content = item
                .get("content")
                .map(flatten_codex_content)
                .unwrap_or_default();
            messages.push(ViewMessage { role, content });
        }
    }

    RequestView {
        dialect: Dialect::Codex,
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

    #[test]
    fn anthropic_projects_system_prompt_as_a_leading_system_message() {
        let body = json!({
            "model": "claude-fake",
            "system": "be terse",
            "messages": [
                { "role": "user", "content": "hi" },
            ],
        })
        .to_string();

        let view = parse_anthropic(body.as_bytes());
        assert_eq!(view.dialect, Dialect::Anthropic);
        assert_eq!(view.model.as_deref(), Some("claude-fake"));
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[0].role, "system");
        assert_eq!(view.messages[0].content, "be terse");
        assert_eq!(view.last_message().unwrap().role, "user");
        assert_eq!(view.last_message().unwrap().content, "hi");
    }

    #[test]
    fn anthropic_flattens_string_and_text_block_content() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "plain" },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "foo" },
                        { "type": "text", "text": "bar" },
                    ],
                },
            ],
        })
        .to_string();

        let view = parse_anthropic(body.as_bytes());
        assert_eq!(view.messages[0].content, "plain");
        assert_eq!(view.messages[1].content, "foobar");
    }

    #[test]
    fn anthropic_system_as_text_blocks_flattens() {
        let body = json!({
            "system": [
                { "type": "text", "text": "a" },
                { "type": "text", "text": "b" },
            ],
            "messages": [{ "role": "user", "content": "hi" }],
        })
        .to_string();

        let view = parse_anthropic(body.as_bytes());
        assert_eq!(view.messages[0].role, "system");
        assert_eq!(view.messages[0].content, "ab");
    }

    #[test]
    fn anthropic_counts_tool_result_blocks_as_prior_tool_results() {
        // Tool outputs come back as user-role messages carrying tool_result
        // blocks; a single user turn can carry several.
        let body = json!({
            "model": "fake",
            "messages": [
                { "role": "user", "content": "do it" },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "tool_use", "id": "t1", "name": "write", "input": {} },
                    ],
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "t1", "content": "ok" },
                        { "type": "tool_result", "tool_use_id": "t2", "content": "ok" },
                    ],
                },
            ],
        })
        .to_string();

        let view = parse_anthropic(body.as_bytes());
        assert_eq!(view.prior_tool_results, 2);
    }

    #[test]
    fn anthropic_invalid_body_yields_an_empty_view_not_an_error() {
        let view = parse_anthropic(b"not json at all");
        assert_eq!(view.dialect, Dialect::Anthropic);
        assert!(view.model.is_none());
        assert!(view.messages.is_empty());
        assert_eq!(view.prior_tool_results, 0);
        assert!(view.last_message().is_none());
    }

    #[test]
    fn codex_projects_instructions_as_a_leading_system_message() {
        let body = json!({
            "model": "gpt-fake",
            "instructions": "be terse",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hi" }],
                },
            ],
        })
        .to_string();

        let view = parse_codex(body.as_bytes());
        assert_eq!(view.dialect, Dialect::Codex);
        assert_eq!(view.model.as_deref(), Some("gpt-fake"));
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[0].role, "system");
        assert_eq!(view.messages[0].content, "be terse");
        assert_eq!(view.last_message().unwrap().role, "user");
        assert_eq!(view.last_message().unwrap().content, "hi");
    }

    #[test]
    fn codex_flattens_string_and_text_part_content() {
        let body = json!({
            "input": [
                { "type": "message", "role": "user", "content": "plain" },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        { "type": "output_text", "text": "foo" },
                        { "type": "output_text", "text": "bar" },
                    ],
                },
            ],
        })
        .to_string();

        let view = parse_codex(body.as_bytes());
        assert_eq!(view.messages[0].content, "plain");
        assert_eq!(view.messages[1].content, "foobar");
    }

    #[test]
    fn codex_counts_function_call_outputs_as_prior_tool_results() {
        // Codex feeds tool outputs back as input items of type
        // function_call_output; each one is a completed tool-use turn.
        let body = json!({
            "model": "fake",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "do it" }],
                },
                { "type": "function_call", "call_id": "c1", "name": "write", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c1", "output": "ok" },
                { "type": "function_call", "call_id": "c2", "name": "write", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c2", "output": "ok" },
            ],
        })
        .to_string();

        let view = parse_codex(body.as_bytes());
        assert_eq!(view.prior_tool_results, 2);
        // function_call / function_call_output items carry no view message; only
        // the single user message survives.
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.messages[0].role, "user");
    }

    #[test]
    fn codex_invalid_body_yields_an_empty_view_not_an_error() {
        let view = parse_codex(b"not json at all");
        assert_eq!(view.dialect, Dialect::Codex);
        assert!(view.model.is_none());
        assert!(view.messages.is_empty());
        assert_eq!(view.prior_tool_results, 0);
        assert!(view.last_message().is_none());
    }
}
