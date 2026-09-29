//! Dialect-agnostic core for `jig`.
//!
//! This crate is intentionally async-free: it owns the canonical [`Reply`] /
//! [`Turn`] model, the [`Script`] that yields a [`ScriptAction`] per request,
//! and the per-dialect SSE renderers. Everything here is pure and synchronous
//! so it unit-tests without a runtime.

use serde::{Deserialize, Serialize};

pub mod conform;
pub mod parse;
mod reference_delivery;
pub mod render;
pub mod request;
mod script;
pub mod script_file;

pub use parse::{
    AnthropicParseError, CodexParseError, OpenAiParseError, SseEvent, parse_anthropic_sse,
    parse_codex_sse, parse_openai_sse, parse_sse,
};
pub use render::{render_anthropic, render_codex, render_openai};
pub use request::{Dialect, RequestView, ViewMessage};
pub use script::{Next, Phase, Phases, Plan, Rule, Script, Sequence};
pub use script_file::{
    ActionObjectSpec, ActionSpec, CountSpec, DialectSpec, HttpErrorBodySpec, HttpErrorSpec,
    PhaseMatcher, PhaseSpec, RawErrorSpec, ReferenceDeliverySpec, ReplySpec, ScriptFile,
    ScriptFileError, StopSpec, ToolCallSpec, TurnSpec,
};

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

/// One outcome a script can produce for a request.
///
/// [`ScriptAction::Reply`] is the existing successful model response path. The
/// non-reply variants let `FakeLlm` exercise provider/API failures that clients
/// must see as transport or API errors rather than as successful SSE streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptAction {
    /// Render a normal provider SSE response.
    Reply(Reply),
    /// Return a non-2xx provider-shaped HTTP response before any SSE framing.
    HttpError(HttpError),
    /// Future extension point for provider in-band stream error frames.
    StreamError(StreamError),
    /// Future extension point for closing a stream before its terminal frame.
    AbortStream(AbortStream),
}

impl From<Reply> for ScriptAction {
    fn from(reply: Reply) -> Self {
        ScriptAction::Reply(reply)
    }
}

impl From<HttpError> for ScriptAction {
    fn from(error: HttpError) -> Self {
        ScriptAction::HttpError(error)
    }
}

impl ScriptAction {
    /// Build a normal reply action.
    pub fn reply(reply: Reply) -> Self {
        ScriptAction::Reply(reply)
    }

    /// Build an HTTP error action.
    pub fn http_error(error: HttpError) -> Self {
        ScriptAction::HttpError(error)
    }
}

/// A scripted non-2xx HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpError {
    /// HTTP status code to return.
    pub status: u16,
    /// Error body, either provider-shaped for a dialect or raw text/JSON.
    pub body: ErrorBody,
    /// Extra response headers. `Content-Length` and `Connection` are managed by
    /// the server; `Content-Type` can be supplied here to override the body
    /// default.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

impl HttpError {
    /// Build a provider-shaped HTTP error using the request route's dialect.
    pub fn provider(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        HttpError {
            status,
            body: ErrorBody::provider(code, message),
            headers: Vec::new(),
        }
    }

    /// Build an exact raw HTTP error body.
    pub fn raw(status: u16, content_type: impl Into<String>, body: impl Into<String>) -> Self {
        HttpError {
            status,
            body: ErrorBody::Raw {
                content_type: content_type.into(),
                body: body.into(),
            },
            headers: Vec::new(),
        }
    }

    /// Render the configured body for the route dialect currently being served.
    pub fn render_body(&self, route_dialect: Dialect) -> RenderedErrorBody {
        self.body.render(route_dialect)
    }
}

/// Body shape for a scripted HTTP error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorBody {
    /// Generate a provider-shaped JSON body. `dialect: None` means use the
    /// dialect of the route handling the request.
    Provider {
        dialect: Option<Dialect>,
        code: String,
        message: String,
        error_type: Option<String>,
        #[serde(default)]
        extra: serde_json::Value,
    },
    /// Use exact bytes and content type from a fixture or parser regression.
    Raw { content_type: String, body: String },
}

impl ErrorBody {
    /// Build a provider-shaped body using the request route's dialect.
    pub fn provider(code: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorBody::Provider {
            dialect: None,
            code: code.into(),
            message: message.into(),
            error_type: None,
            extra: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Render this body for a route dialect.
    pub fn render(&self, route_dialect: Dialect) -> RenderedErrorBody {
        render_error_body(self, route_dialect)
    }
}

/// A rendered HTTP error body with the content type the server should send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedErrorBody {
    pub content_type: String,
    pub body: String,
}

/// Render a scripted error body for a route dialect.
pub fn render_error_body(body: &ErrorBody, route_dialect: Dialect) -> RenderedErrorBody {
    match body {
        ErrorBody::Raw { content_type, body } => RenderedErrorBody {
            content_type: content_type.clone(),
            body: body.clone(),
        },
        ErrorBody::Provider {
            dialect,
            code,
            message,
            error_type,
            extra,
        } => {
            let dialect = dialect.unwrap_or(route_dialect);
            let value = match dialect {
                Dialect::OpenAi | Dialect::Codex => {
                    let mut error = merged_extra_object(extra);
                    error.insert("code".to_string(), serde_json::Value::String(code.clone()));
                    error.insert(
                        "message".to_string(),
                        serde_json::Value::String(message.clone()),
                    );
                    if let Some(error_type) = error_type {
                        error.insert(
                            "type".to_string(),
                            serde_json::Value::String(error_type.clone()),
                        );
                    }
                    serde_json::json!({ "error": error })
                }
                Dialect::Anthropic => {
                    let mut error = merged_extra_object(extra);
                    error.insert(
                        "type".to_string(),
                        serde_json::Value::String(
                            error_type.clone().unwrap_or_else(|| code.clone()),
                        ),
                    );
                    error.insert(
                        "message".to_string(),
                        serde_json::Value::String(message.clone()),
                    );
                    serde_json::json!({ "type": "error", "error": error })
                }
            };
            RenderedErrorBody {
                content_type: "application/json".to_string(),
                body: serde_json::to_string(&value).expect("provider error JSON serializes"),
            }
        }
    }
}

fn merged_extra_object(extra: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut object = serde_json::Map::new();
    match extra {
        serde_json::Value::Object(extra) => {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        serde_json::Value::Null => {}
        other => {
            object.insert("extra".to_string(), other.clone());
        }
    }
    object
}

/// Future extension point for provider in-band stream error frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamError {
    pub body: ErrorBody,
}

/// Future extension point for truncated stream / dropped connection coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbortStream {
    pub reply: Reply,
    pub after_frames: Option<usize>,
    pub after_bytes: Option<usize>,
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
    fn provider_error_body_uses_anthropic_shape_and_merges_extra() {
        let body = ErrorBody::Provider {
            dialect: None,
            code: "server_error".to_string(),
            message: "temporary".to_string(),
            error_type: None,
            extra: serde_json::json!({ "request_id": "req_1" }),
        };

        let rendered = body.render(Dialect::Anthropic);
        assert_eq!(rendered.content_type, "application/json");
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "server_error");
        assert_eq!(json["error"]["message"], "temporary");
        assert_eq!(json["error"]["request_id"], "req_1");
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
