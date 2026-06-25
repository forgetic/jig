use jig_core::{Dialect, ErrorBody, HttpError, Script, ScriptAction};
use serde_json::Value;

mod support;

#[test]
fn openai_route_can_return_provider_http_error_and_records_request() {
    let fake = start_with_error(provider_error(None));

    let response = support::post_json_response(
        &format!("{}/chat/completions", fake.base_url()),
        &[("authorization", "Bearer test-key")],
        &serde_json::json!({
            "model": "gpt-fake",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    );

    assert_http_error_headers(&response, 500);
    let body: Value = serde_json::from_str(&response.body).expect("JSON error body");
    assert_eq!(body["error"]["code"], "server_error");
    assert_eq!(body["error"]["message"], "temporary upstream failure");
    assert_eq!(body["error"]["request_id"], "req_1");

    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/chat/completions");
    assert_eq!(requests[0].view.as_ref().unwrap().dialect, Dialect::OpenAi);
}

#[test]
fn anthropic_route_can_return_provider_http_error_and_records_request() {
    let fake = start_with_error(provider_error(None));

    let response = support::post_json_response(
        &format!("{}/v1/messages", fake.base_url()),
        &[("x-api-key", "test-key")],
        &serde_json::json!({
            "model": "claude-fake",
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    );

    assert_http_error_headers(&response, 500);
    let body: Value = serde_json::from_str(&response.body).expect("JSON error body");
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["message"], "temporary upstream failure");
    assert_eq!(body["error"]["request_id"], "req_1");

    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/messages");
    assert_eq!(requests[0].view.as_ref().unwrap().dialect, Dialect::Anthropic);
}

#[test]
fn codex_route_can_return_provider_http_error_and_records_request() {
    let fake = start_with_error(provider_error(None));

    let response = support::post_json_response(
        &format!("{}/backend-api/codex/responses", fake.base_url()),
        &[("authorization", "Bearer test-key")],
        &serde_json::json!({
            "model": "gpt-fake",
            "instructions": "be terse",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hi" }],
                },
            ],
        }),
    );

    assert_http_error_headers(&response, 500);
    let body: Value = serde_json::from_str(&response.body).expect("JSON error body");
    assert_eq!(body["error"]["code"], "server_error");
    assert_eq!(body["error"]["message"], "temporary upstream failure");
    assert_eq!(body["error"]["request_id"], "req_1");

    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/backend-api/codex/responses");
    assert_eq!(requests[0].view.as_ref().unwrap().dialect, Dialect::Codex);
}

fn start_with_error(error: HttpError) -> jig_server::FakeLlm {
    jig_server::FakeLlm::start(Script::fixed_action(ScriptAction::HttpError(error)))
        .expect("FakeLlm starts")
}

fn provider_error(dialect: Option<Dialect>) -> HttpError {
    HttpError {
        status: 500,
        body: ErrorBody::Provider {
            dialect,
            code: "server_error".to_string(),
            message: "temporary upstream failure".to_string(),
            error_type: None,
            extra: serde_json::json!({ "request_id": "req_1" }),
        },
        headers: vec![("x-jig-test".to_string(), "provider-error".to_string())],
    }
}

fn assert_http_error_headers(response: &support::Response, status: u16) {
    assert_eq!(response.status, status);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.header("connection"), Some("close"));
    assert_eq!(response.header("x-jig-test"), Some("provider-error"));
    assert_eq!(response.header("transfer-encoding"), None);
    assert_eq!(
        response.header("content-length"),
        Some(response.body.len().to_string().as_str())
    );
    assert!(!response.body.contains("data: "), "error body must not be SSE");
}
