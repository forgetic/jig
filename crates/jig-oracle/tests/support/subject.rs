//! The pi-SDK **subject** driver: build a `pi_agent_rust` provider pointed at a
//! `base_url` and run the issue #13 scenarios through it, for all three wire
//! dialects (issue #17, P6).
//!
//! This is the shared driving core used by two callers:
//!
//! - the **offline oracle** (`tests/oracle.rs`) points `base_url` at an
//!   in-process `jig_server::FakeLlm` with synthetic credentials and asserts the
//!   SDK decodes jig and completes the loop — no network, in `cargo test`; and
//! - the **online recording harness** (`tests/pi_subject_record.rs`, `#[ignore]`)
//!   points `base_url` at the passthrough recorder with the **real** credentials
//!   from `~/.pi/agent/auth.json`, capturing redacted `subject` fixtures.
//!
//! The SDK is consumed **directly** — no smith. The only smith-derived code is
//! the Anthropic subscription workaround in [`crate::anthropic_oauth`], applied
//! here for the anthropic dialect.

use std::collections::HashMap;

use pi::model::{
    AssistantMessage, ContentBlock, Message, StopReason, TextContent, ToolCall, ToolResultMessage,
    Usage, UserContent, UserMessage,
};
use pi::models::ModelEntry;
use pi::provider::{Context, InputType, Model, ModelCost, StreamOptions, ToolDef};

use super::anthropic_oauth::{CLAUDE_CODE_SYSTEM_IDENTITY, request_headers};

/// One wire dialect the SDK can be pointed at. The `provider` ids are
/// deliberately *non-canonical* for openai/anthropic so the SDK takes its
/// generic `api`-routed path; the anthropic subscription workaround is applied by
/// this module, not by the SDK's (absent) native path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// OpenAI/DeepSeek chat-completions.
    OpenAi,
    /// Anthropic messages (subscription OAuth via the duplicated workaround).
    Anthropic,
    /// OpenAI Codex responses.
    Codex,
}

impl Dialect {
    /// The fixture-tree slug (`openai` / `anthropic` / `codex`).
    pub fn slug(self) -> &'static str {
        match self {
            Dialect::OpenAi => "openai",
            Dialect::Anthropic => "anthropic",
            Dialect::Codex => "codex",
        }
    }

    /// The pi-SDK `api` string selecting the request encoder + base-url normalizer.
    pub fn api(self) -> &'static str {
        match self {
            Dialect::OpenAi => "openai-completions",
            Dialect::Anthropic => "anthropic-messages",
            Dialect::Codex => "openai-codex-responses",
        }
    }

    /// A non-canonical pi-SDK provider id (so the generic api path is taken). For
    /// codex the canonical `openai-codex` id is required — its provider does the
    /// `chatgpt_account_id` claim extraction the responses path needs.
    pub fn provider(self) -> &'static str {
        match self {
            Dialect::OpenAi => "deepseek",
            Dialect::Anthropic => "kimi",
            Dialect::Codex => "openai-codex",
        }
    }

    /// The jig/recorder route the normalized `base_url` resolves to.
    pub fn route(self) -> &'static str {
        match self {
            Dialect::OpenAi => "/chat/completions",
            Dialect::Anthropic => "/v1/messages",
            Dialect::Codex => "/backend-api/codex/responses",
        }
    }

    /// All three dialects, in fixture-tree order.
    pub fn all() -> [Dialect; 3] {
        [Dialect::OpenAi, Dialect::Anthropic, Dialect::Codex]
    }

    /// A **currently-valid** model id for the real backend this dialect records
    /// against. The request must name a model the backend accepts or it 400s, so
    /// these are concrete provider model names (not jig's own placeholder). The
    /// *requested* model is a harness/config choice, not SDK wire behaviour, so
    /// T3 masks it before comparing against the authoritative template (see
    /// `jig_core::conform::strip_request_cross_client`); this only has to satisfy
    /// the live backend.
    pub fn model_id(self) -> &'static str {
        match self {
            // DeepSeek's older `deepseek-chat` alias was retired; `deepseek-v4-flash`
            // is the current cheapest chat model.
            Dialect::OpenAi => "deepseek-v4-flash",
            Dialect::Anthropic => "claude-sonnet-4-5",
            Dialect::Codex => "gpt-5.5",
        }
    }
}

/// Build a pi-SDK [`ModelEntry`] for `dialect` pointed at `base_url`, carrying
/// `api_key` as the bearer and any extra per-model `headers`.
///
/// The SDK's `create_provider` normalizes the custom host into the dialect's
/// concrete path, so `base_url = http://127.0.0.1:PORT` reaches the matching
/// route on jig or the recorder.
pub fn model_entry(
    dialect: Dialect,
    base_url: &str,
    api_key: Option<String>,
    headers: HashMap<String, String>,
) -> ModelEntry {
    ModelEntry {
        model: Model {
            id: dialect.model_id().to_string(),
            name: dialect.model_id().to_string(),
            api: dialect.api().to_string(),
            provider: dialect.provider().to_string(),
            base_url: base_url.to_string(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
            context_window: 200_000,
            max_tokens: 8192,
            headers: HashMap::new(),
        },
        api_key,
        headers,
        auth_header: true,
        compat: None,
        oauth_config: None,
    }
}

/// The scenarios the subject driver records, mirroring the issue #13 matrix that
/// already has authoritative templates. `thinking-text` is intentionally omitted:
/// jig drives the model deterministically only via the prompt, and forcing a
/// thinking turn from the SDK is not reliable, so the subject set is the three
/// tool-shape scenarios whose request grammar T3 validates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// A single user turn → one text reply.
    SingleText,
    /// A single user turn → one tool call (tool-call request grammar).
    ToolCall,
    /// user + assistant(tool_call) + tool_result → final text (the multi-turn
    /// request grammar: how the SDK echoes a prior tool call and feeds a result).
    ToolResultFinal,
}

impl Scenario {
    /// The fixture-tree scenario slug.
    pub fn slug(self) -> &'static str {
        match self {
            Scenario::SingleText => "single-text",
            Scenario::ToolCall => "tool-call",
            Scenario::ToolResultFinal => "tool-result-final",
        }
    }

    /// All three subject scenarios, in fixture-tree order.
    pub fn all() -> [Scenario; 3] {
        [
            Scenario::SingleText,
            Scenario::ToolCall,
            Scenario::ToolResultFinal,
        ]
    }
}

/// The single tool the tool scenarios expose. A fixed, minimal function schema so
/// the recorded request's tool grammar is deterministic and reviewable.
pub fn weather_tool() -> ToolDef {
    ToolDef {
        name: "get_weather".to_string(),
        description: "Get the current weather for a city".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
        }),
    }
}

/// Build the [`Context`] for a `(dialect, scenario)` cell.
///
/// For the anthropic dialect the **subscription OAuth** workaround is applied:
/// the system prompt is the mandatory [`CLAUDE_CODE_SYSTEM_IDENTITY`] and the
/// role instruction is folded into the user turn (the SDK sends `system` as a
/// single string and Anthropic rejects any other first system block with a 429).
/// Every other dialect uses a normal system prompt.
pub fn context_for(dialect: Dialect, scenario: Scenario) -> Context<'static> {
    let anthropic = dialect == Dialect::Anthropic;

    // The role instruction that drives the scenario shape. Folded into the user
    // turn for anthropic; used as the system prompt otherwise.
    let role = "You are a terse test assistant. Follow the instruction exactly.";

    let system_prompt = if anthropic {
        Some(CLAUDE_CODE_SYSTEM_IDENTITY.to_string())
    } else {
        Some(role.to_string())
    };

    // Prefix the user turn with the role for anthropic so the model still gets it.
    let prefix = if anthropic {
        format!("{role}\n\n")
    } else {
        String::new()
    };

    let tools = match scenario {
        Scenario::SingleText => vec![],
        Scenario::ToolCall | Scenario::ToolResultFinal => vec![weather_tool()],
    };

    let messages = match scenario {
        Scenario::SingleText => vec![user(&format!("{prefix}Reply with exactly: hello"))],
        Scenario::ToolCall => vec![user(&format!(
            "{prefix}Call the get_weather tool for the city Paris. Do not reply with text."
        ))],
        Scenario::ToolResultFinal => {
            // The prior assistant tool call + its result, fed back so the SDK
            // encodes the multi-turn request grammar (how it echoes a tool call
            // and a tool result). The follow-up asks for the final text.
            let call = ToolCall {
                id: "call_jig_subject_1".to_string(),
                name: "get_weather".to_string(),
                arguments: serde_json::json!({ "city": "Paris" }),
                thought_signature: None,
            };
            vec![
                user(&format!("{prefix}What is the weather in Paris?")),
                Message::assistant(AssistantMessage {
                    content: vec![ContentBlock::ToolCall(call.clone())],
                    api: dialect.api().to_string(),
                    provider: dialect.provider().to_string(),
                    model: dialect.model_id().to_string(),
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                }),
                Message::tool_result(ToolResultMessage {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    content: vec![ContentBlock::Text(TextContent::new("sunny, 24C"))],
                    details: None,
                    is_error: false,
                    timestamp: 0,
                }),
                user("Now tell me the weather in one short sentence."),
            ]
        }
    };

    Context::owned(system_prompt, messages, tools)
}

/// The per-request [`StreamOptions`] for a dialect: the bearer plus, for
/// anthropic, the Claude Code identity headers (highest-priority per-request
/// headers the SDK applies after its own defaults).
pub fn stream_options(dialect: Dialect, api_key: &str) -> StreamOptions {
    let mut options = StreamOptions {
        api_key: Some(api_key.to_string()),
        ..Default::default()
    };
    if dialect == Dialect::Anthropic {
        options.headers = request_headers();
    }
    options
}

/// A user message with text content.
fn user(text: &str) -> Message {
    Message::User(UserMessage {
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_slug_api_route_are_consistent() {
        assert_eq!(Dialect::OpenAi.route(), "/chat/completions");
        assert_eq!(Dialect::Anthropic.route(), "/v1/messages");
        assert_eq!(Dialect::Codex.route(), "/backend-api/codex/responses");
        assert_eq!(
            Dialect::all().map(|d| d.slug()),
            ["openai", "anthropic", "codex"]
        );
    }

    #[test]
    fn anthropic_context_uses_identity_system_and_folds_role_into_user() {
        let ctx = context_for(Dialect::Anthropic, Scenario::SingleText);
        // System prompt is exactly the required identity line.
        assert_eq!(
            ctx.system_prompt.as_deref(),
            Some(CLAUDE_CODE_SYSTEM_IDENTITY)
        );
        // The role instruction is folded into the (first) user turn.
        match &ctx.messages[0] {
            Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            }) => assert!(
                text.contains("terse test assistant"),
                "role not folded into user turn: {text}"
            ),
            other => panic!("expected user turn, got {other:?}"),
        }
    }

    #[test]
    fn non_anthropic_context_uses_a_plain_system_prompt() {
        let ctx = context_for(Dialect::OpenAi, Scenario::SingleText);
        assert_ne!(
            ctx.system_prompt.as_deref(),
            Some(CLAUDE_CODE_SYSTEM_IDENTITY)
        );
        assert!(
            ctx.system_prompt
                .as_deref()
                .unwrap()
                .contains("terse test assistant")
        );
    }

    #[test]
    fn tool_scenarios_expose_the_weather_tool() {
        let single = context_for(Dialect::OpenAi, Scenario::SingleText);
        assert!(single.tools.is_empty());
        for scenario in [Scenario::ToolCall, Scenario::ToolResultFinal] {
            let ctx = context_for(Dialect::OpenAi, scenario);
            assert_eq!(ctx.tools.len(), 1);
            assert_eq!(ctx.tools[0].name, "get_weather");
        }
    }

    #[test]
    fn tool_result_final_carries_the_full_multi_turn_grammar() {
        let ctx = context_for(Dialect::OpenAi, Scenario::ToolResultFinal);
        // user, assistant(tool_call), tool_result, user.
        assert_eq!(ctx.messages.len(), 4);
        assert!(matches!(ctx.messages[1], Message::Assistant(_)));
    }

    #[test]
    fn anthropic_stream_options_carry_identity_headers() {
        let options = stream_options(Dialect::Anthropic, "bearer-x");
        assert_eq!(options.api_key.as_deref(), Some("bearer-x"));
        assert!(options.headers.contains_key("anthropic-beta"));
        assert_eq!(
            options.headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
    }

    #[test]
    fn non_anthropic_stream_options_have_no_identity_headers() {
        let options = stream_options(Dialect::OpenAi, "bearer-x");
        assert!(options.headers.is_empty());
    }

    #[test]
    fn model_entry_points_at_base_url_with_dialect_api() {
        let entry = model_entry(
            Dialect::Codex,
            "http://127.0.0.1:9999",
            Some("jwt".to_string()),
            HashMap::new(),
        );
        assert_eq!(entry.model.base_url, "http://127.0.0.1:9999");
        assert_eq!(entry.model.api, "openai-codex-responses");
        assert_eq!(entry.model.provider, "openai-codex");
        assert_eq!(entry.api_key.as_deref(), Some("jwt"));
    }
}
