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
mod reference_delivery;
pub mod render;
pub mod request;
pub mod script_file;

pub use parse::{
    AnthropicParseError, CodexParseError, OpenAiParseError, SseEvent, parse_anthropic_sse,
    parse_codex_sse, parse_openai_sse, parse_sse,
};
pub use render::{render_anthropic, render_codex, render_openai};
pub use request::{Dialect, RequestView, ViewMessage};
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

/// Build the best reply-shaped fallback for callers still using `next_reply` on
/// an action script that produced a non-reply outcome.
fn non_reply_action_fallback() -> Reply {
    Reply {
        turns: Vec::new(),
        usage: Usage::default(),
        stop: StopReason::Error,
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

/// Decides which [`ScriptAction`] to serve for a given request.
///
/// The original reply-only variants and constructors remain available. The
/// server calls [`Script::next_action`] so scripts can now return provider-shaped
/// failures as well as normal replies, while old in-process consumers can keep
/// using [`Script::next_reply`] when they only care about successful replies.
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
    /// Serve the same action for every request.
    FixedAction(ScriptAction),
    /// Serve actions in order; once exhausted, the last action repeats for every
    /// further request.
    ActionSequence {
        actions: Vec<ScriptAction>,
        cursor: Mutex<usize>,
    },
    /// Decide the action from the parsed request.
    ActionRule(Box<dyn Fn(&RequestView) -> ScriptAction + Send + Sync>),
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

    /// Build a fixed script action.
    pub fn fixed_action(action: impl Into<ScriptAction>) -> Self {
        Script::FixedAction(action.into())
    }

    /// Build a sequence from an ordered list of actions.
    pub fn action_sequence(actions: Vec<ScriptAction>) -> Self {
        Script::ActionSequence {
            actions,
            cursor: Mutex::new(0),
        }
    }

    /// Build a request-aware action rule.
    pub fn action_rule(f: impl Fn(&RequestView) -> ScriptAction + Send + Sync + 'static) -> Self {
        Script::ActionRule(Box::new(f))
    }

    /// Produce the action for the next request.
    ///
    /// `view` is the normalized projection of the request body. Reply-only
    /// variants are lifted into [`ScriptAction::Reply`]. Sequence cursors
    /// advance and then clamp at the final element so the last item repeats.
    pub fn next_action(&self, view: &RequestView) -> ScriptAction {
        match self {
            Script::Fixed(reply) => ScriptAction::Reply(reply.clone()),
            Script::Sequence { replies, cursor } => choose_next(replies, cursor)
                .map(ScriptAction::Reply)
                .unwrap_or_else(|| ScriptAction::Reply(Reply::text(""))),
            Script::Rule(f) => ScriptAction::Reply(f(view)),
            Script::FixedAction(action) => action.clone(),
            Script::ActionSequence { actions, cursor } => {
                choose_next(actions, cursor).unwrap_or_else(|| ScriptAction::Reply(Reply::text("")))
            }
            Script::ActionRule(f) => f(view),
        }
    }

    /// Produce the reply for the next request.
    ///
    /// This is the original reply-only API. On action-aware scripts it advances
    /// the same cursor as [`Script::next_action`]; if the selected action is not a
    /// reply, it returns an empty error-stopped reply because HTTP errors cannot
    /// be represented as [`Reply`].
    pub fn next_reply(&self, view: &RequestView) -> Reply {
        match self.next_action(view) {
            ScriptAction::Reply(reply) => reply,
            ScriptAction::HttpError(_)
            | ScriptAction::StreamError(_)
            | ScriptAction::AbortStream(_) => non_reply_action_fallback(),
        }
    }
}

fn choose_next<T: Clone>(items: &[T], cursor: &Mutex<usize>) -> Option<T> {
    if items.is_empty() {
        return None;
    }

    // Lock to read+advance the cursor. The lock is uncontended on the
    // single-threaded runtime; recover from poisoning rather than panicking so
    // one bad request can't wedge the server.
    let mut idx = cursor.lock().unwrap_or_else(|p| p.into_inner());
    let chosen = items[*idx].clone();
    // Advance, clamping at the last index so it repeats once exhausted.
    if *idx + 1 < items.len() {
        *idx += 1;
    }
    Some(chosen)
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
    fn action_sequence_serves_errors_and_replies_in_order() {
        let script = Script::action_sequence(vec![
            ScriptAction::HttpError(HttpError::provider(500, "server_error", "try again")),
            ScriptAction::Reply(Reply::text("success")),
        ]);
        let view = view_with_turns(0);

        match script.next_action(&view) {
            ScriptAction::HttpError(error) => {
                assert_eq!(error.status, 500);
                assert_eq!(
                    error.render_body(Dialect::OpenAi).body,
                    r#"{"error":{"code":"server_error","message":"try again"}}"#
                );
            }
            other => panic!("expected HTTP error action, got {other:?}"),
        }
        assert_eq!(
            script.next_action(&view),
            ScriptAction::Reply(Reply::text("success"))
        );
        assert_eq!(
            script.next_action(&view),
            ScriptAction::Reply(Reply::text("success"))
        );
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
