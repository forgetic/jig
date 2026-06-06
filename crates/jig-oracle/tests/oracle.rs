//! Offline pi-SDK ↔ jig oracle.
//!
//! Drives the **pi SDK** (`pi_agent_rust`, consumed directly) against an
//! in-process jig with no network and no real credentials, and asserts the SDK
//! decodes jig's SSE into its canonical [`StreamEvent`] model and completes the
//! loop. jig's response contract is anchored to the official recordings (P1–P4);
//! this test measures that the pi SDK — the *subject* driver — actually parses
//! and drives off jig.
//!
//! Coverage: all three wire dialects (OpenAI/DeepSeek chat-completions,
//! Anthropic messages, OpenAI Codex responses), each for (a) a single text
//! reply and (b) a tool-call → tool-result → final loop.
//!
//! Credentials: none are real. OpenAI/Anthropic accept any bearer. Codex
//! validates that the bearer is a JWT carrying a `chatgpt_account_id` claim
//! *before* sending, so the test mints a synthetic unsigned JWT locally — the
//! same shape the SDK's own provider tests use ("Codex OAuth needs only bearer
//! resolution", per issue #17). Nothing leaves the machine.

use std::collections::HashMap;

use base64::Engine;
use futures::StreamExt;
use jig_core::{Reply, Script, StopReason as JigStop, Turn, Usage as JigUsage};
use jig_server::FakeLlm;

use pi::model::{
    AssistantMessage, ContentBlock, Message, StopReason, StreamEvent, TextContent, ToolCall,
    ToolResultMessage, Usage, UserContent, UserMessage,
};
use pi::models::ModelEntry;
use pi::provider::{Context, InputType, Model, ModelCost, Provider, StreamOptions, ToolDef};
use pi::providers::create_provider;

/// One wire dialect the SDK can be pointed at jig for.
struct DialectCase {
    /// Human label for assertion messages.
    name: &'static str,
    /// pi-SDK provider id. Deliberately *non-canonical* for openai/anthropic so
    /// the SDK takes its generic `api:`-routed path (and, for anthropic, skips
    /// the native subscription-OAuth workaround) — the offline oracle only
    /// needs the wire format, not real auth.
    provider: &'static str,
    /// pi-SDK `api` string. Selects the request encoder + base-url normalizer.
    api: &'static str,
    /// The jig route the normalized `base_url` must resolve to.
    path: &'static str,
}

fn cases() -> [DialectCase; 3] {
    [
        DialectCase {
            name: "openai",
            provider: "deepseek",
            api: "openai-completions",
            path: "/chat/completions",
        },
        DialectCase {
            name: "anthropic",
            provider: "kimi",
            api: "anthropic-messages",
            path: "/v1/messages",
        },
        DialectCase {
            name: "codex",
            provider: "openai-codex",
            api: "openai-codex-responses",
            path: "/backend-api/codex/responses",
        },
    ]
}

/// A synthetic, unsigned Codex OAuth JWT carrying a `chatgpt_account_id` claim.
///
/// Mirrors how the SDK's own `openai_responses` tests build a token, so the
/// codex provider's pre-flight claim check passes without any real OAuth or
/// network. The signature segment is a placeholder — the SDK only base64-decodes
/// the payload to read the claim; it never verifies the signature.
fn synthetic_codex_bearer() -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = b64.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = b64.encode(
        serde_json::to_vec(&serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_oracle_test" }
        }))
        .expect("encode synthetic jwt payload"),
    );
    format!("{header}.{payload}.sig")
}

/// The bearer the SDK should send for a given dialect: a synthetic JWT for
/// codex, an arbitrary placeholder otherwise.
fn api_key_for(api: &str) -> String {
    if api == "openai-codex-responses" {
        synthetic_codex_bearer()
    } else {
        "oracle-test-key".to_string()
    }
}

/// Build a pi-SDK [`ModelEntry`] pointing at jig's `base_url`. The SDK's
/// `create_provider` normalizes the custom host into the dialect's concrete
/// path, so `base_url = http://127.0.0.1:PORT` reaches the matching jig route.
fn model_entry(case: &DialectCase, base_url: &str) -> ModelEntry {
    ModelEntry {
        model: Model {
            id: "oracle-test-model".to_string(),
            name: "oracle-test-model".to_string(),
            api: case.api.to_string(),
            provider: case.provider.to_string(),
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
        api_key: Some(api_key_for(case.api)),
        headers: HashMap::new(),
        auth_header: true,
        compat: None,
        oauth_config: None,
    }
}

fn jig_usage() -> JigUsage {
    JigUsage {
        prompt_tokens: 5,
        completion_tokens: 3,
    }
}

/// What the SDK decoded from one streamed completion.
struct Decoded {
    text: String,
    tool_calls: Vec<ToolCall>,
    done: bool,
    stop: Option<StopReason>,
}

/// Drive one completion to completion on the SDK's own current-thread runtime,
/// collecting the canonical [`StreamEvent`]s it decodes from jig's SSE.
fn drive(provider: &dyn Provider, ctx: &Context<'_>, api_key: &str) -> Decoded {
    let options = StreamOptions {
        api_key: Some(api_key.to_string()),
        ..Default::default()
    };
    let runtime = asupersync::runtime::RuntimeBuilder::current_thread()
        .build()
        .expect("build pi-SDK runtime");
    runtime.block_on(async {
        let mut stream = provider.stream(ctx, &options).await.expect("stream start");
        let mut decoded = Decoded {
            text: String::new(),
            tool_calls: Vec::new(),
            done: false,
            stop: None,
        };
        while let Some(event) = stream.next().await {
            match event.expect("stream event") {
                StreamEvent::TextDelta { delta, .. } => decoded.text.push_str(&delta),
                StreamEvent::ToolCallEnd { tool_call, .. } => decoded.tool_calls.push(tool_call),
                StreamEvent::Done { reason, .. } => {
                    decoded.done = true;
                    decoded.stop = Some(reason);
                }
                _ => {}
            }
        }
        decoded
    })
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

/// The SDK parses a single scripted text reply from jig and reaches `Done`, on
/// every dialect, hitting the expected route exactly once.
#[test]
fn sdk_parses_jig_text_on_every_dialect() {
    for case in cases() {
        let reply = Reply {
            turns: vec![Turn::Text("hello from jig".to_string())],
            usage: jig_usage(),
            stop: JigStop::Stop,
        };
        let jig = FakeLlm::start(Script::Fixed(reply)).expect("start jig");
        let provider =
            create_provider(&model_entry(&case, &jig.base_url()), None).expect("create provider");

        let ctx = Context::owned(
            Some("you are a test".to_string()),
            vec![user("ping")],
            vec![],
        );
        let decoded = drive(provider.as_ref(), &ctx, &api_key_for(case.api));

        assert!(decoded.done, "[{}] SDK should reach Done", case.name);
        assert_eq!(
            decoded.text, "hello from jig",
            "[{}] decoded text",
            case.name
        );

        let requests = jig.requests();
        assert_eq!(requests.len(), 1, "[{}] one request", case.name);
        assert_eq!(requests[0].path, case.path, "[{}] route", case.name);
    }
}

/// The SDK completes a full agent loop against jig: it parses a tool call,
/// then — fed the tool result back as a follow-up turn — parses the final text.
/// Exercises tool-call request encoding *and* response decoding on every
/// dialect, with no network.
#[test]
fn sdk_completes_tool_call_then_tool_result_loop() {
    for case in cases() {
        // Turn 1: jig asks for a tool call. Turn 2: jig replies with final text.
        let tool_call_reply = Reply {
            turns: vec![Turn::ToolCall {
                id: "call_1".to_string(),
                name: "get_weather".to_string(),
                args: serde_json::json!({ "city": "Paris" }),
            }],
            usage: jig_usage(),
            stop: JigStop::ToolCalls,
        };
        let final_reply = Reply {
            turns: vec![Turn::Text("It is sunny.".to_string())],
            usage: jig_usage(),
            stop: JigStop::Stop,
        };
        let jig = FakeLlm::start(Script::sequence(vec![tool_call_reply, final_reply]))
            .expect("start jig");
        let provider =
            create_provider(&model_entry(&case, &jig.base_url()), None).expect("create provider");

        let tools = vec![ToolDef {
            name: "get_weather".to_string(),
            description: "Get the weather for a city".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
            }),
        }];

        // First turn: the SDK should decode jig's tool call.
        let ctx1 = Context::owned(
            Some("you are a test".to_string()),
            vec![user("weather in Paris?")],
            tools.clone(),
        );
        let turn1 = drive(provider.as_ref(), &ctx1, &api_key_for(case.api));
        assert!(turn1.done, "[{}] turn 1 done", case.name);
        assert_eq!(
            turn1.stop,
            Some(StopReason::ToolUse),
            "[{}] stop=ToolUse",
            case.name
        );
        assert_eq!(turn1.tool_calls.len(), 1, "[{}] one tool call", case.name);
        let call = turn1.tool_calls[0].clone();
        assert_eq!(call.name, "get_weather", "[{}] tool name", case.name);

        // Feed the assistant tool call + a tool result back, then ask again.
        let assistant = Message::assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall(call.clone())],
            api: case.api.to_string(),
            provider: case.provider.to_string(),
            model: "oracle-test-model".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 0,
        });
        let tool_result = Message::tool_result(ToolResultMessage {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content: vec![ContentBlock::Text(TextContent::new("sunny, 24C"))],
            details: None,
            is_error: false,
            timestamp: 0,
        });
        let ctx2 = Context::owned(
            Some("you are a test".to_string()),
            vec![user("weather in Paris?"), assistant, tool_result],
            tools,
        );
        let turn2 = drive(provider.as_ref(), &ctx2, &api_key_for(case.api));

        assert!(turn2.done, "[{}] turn 2 done", case.name);
        assert_eq!(turn2.text, "It is sunny.", "[{}] final text", case.name);
        assert_eq!(
            jig.requests().len(),
            2,
            "[{}] two requests total",
            case.name
        );
    }
}
