//! Secret redaction, applied at capture time.
//!
//! Nothing secret is ever written to a fixture: this module rewrites the value
//! of every sensitive header to a **stable placeholder** *before* the captured
//! request/response is handed to the fixture writer. Stable placeholders (rather
//! than dropping the header) keep the fixture a faithful shape — a consumer can
//! still see that an `authorization` header was present and what scheme it
//! used — without leaking the credential.
//!
//! Redaction is a pure, synchronous transform over `(name, value)` header pairs
//! so it unit-tests without a runtime or a network leg (see issue #18
//! acceptance: "add a unit test for the redactor").

/// A captured HTTP header: name as received (case preserved) and its value.
///
/// The recorder captures headers verbatim, then runs [`redact_headers`] over the
/// list before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Header {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// The placeholder a redacted header value is replaced with. Stable across runs
/// and dialects so fixtures diff cleanly.
pub const REDACTED: &str = "REDACTED";

/// Header names whose value is always a credential and must be redacted.
///
/// Matched case-insensitively. Covers the bearer/key auth headers, the OAuth
/// account-identifying headers some providers send (`chatgpt-account-id`,
/// `openai-organization`, …), and the cookie headers. `proxy-authorization` is
/// included for completeness even though the recorder does not use a proxy.
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "set-cookie",
    "openai-organization",
    "openai-project",
    "chatgpt-account-id",
    "x-account-id",
    // Claude Code stamps every `/v1/messages` request with a per-session UUID.
    // It is account/session *identity*, not a credential, but issue #15's
    // acceptance is explicit — "no secrets under `fixtures/`, only `REDACTED`
    // for auth/identity" and calls out session UUIDs as volatile identity — so
    // the captured value is redacted at the recording layer too (the header
    // *name* survives, so the fixture still records that the client sends it).
    "x-claude-code-session-id",
];

/// Header-name prefixes that mark a family of account/session headers, any of
/// which may carry identifying or secret material. Matched case-insensitively.
const SECRET_PREFIXES: &[&str] = &["x-oauth-", "x-stainless-account"];

/// Whether a header name must have its value redacted.
pub fn is_secret_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if SECRET_HEADERS.contains(&lower.as_str()) {
        return true;
    }
    SECRET_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// Redact a single header value if its name marks it secret; otherwise return
/// the value unchanged. Splitting this out keeps [`redact_headers`] trivial and
/// lets callers redact an individual value (e.g. when streaming).
pub fn redact_value(name: &str, value: &str) -> String {
    if is_secret_header(name) {
        REDACTED.to_string()
    } else {
        value.to_string()
    }
}

/// Return a redacted copy of a captured header list: every secret header's value
/// is replaced with [`REDACTED`], every other header is preserved verbatim.
///
/// Header *names* are never dropped or rewritten — only secret *values* change —
/// so the fixture still records which headers the client sent.
pub fn redact_headers(headers: &[Header]) -> Vec<Header> {
    headers
        .iter()
        .map(|h| Header::new(h.name.clone(), redact_value(&h.name, &h.value)))
        .collect()
}

/// JSON **body** keys whose value carries account/session identity and must be
/// redacted before a request is written to a fixture, wherever they appear.
///
/// Header redaction is not enough: some clients carry identity in the request
/// *body* too. Claude Code, for example, sends a `metadata.user_id` whose value
/// is a JSON blob of `device_id` / `account_uuid` / `session_id` — real account
/// identifiers, not a credential, but identity the committed fixture must not
/// leak (issue #15 acceptance: "no secrets under `fixtures/` — only `REDACTED`
/// for auth/identity"). Matched exactly (case-sensitive); wire JSON keys are
/// stable. The value is replaced wholesale with [`REDACTED`] regardless of type,
/// so the *shape* (the key is present) is still recorded without the identity.
const SECRET_BODY_KEYS: &[&str] = &["user_id", "account_uuid", "device_id", "session_id"];

/// Recursively redact every [`SECRET_BODY_KEYS`] value in a JSON request body to
/// [`REDACTED`], preserving all other structure (keys, nesting, array order).
///
/// Pure transform over `serde_json::Value`; the proxy applies it to the parsed
/// request body before the fixture is written. A non-identity body is returned
/// structurally unchanged.
pub fn redact_body_value(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if SECRET_BODY_KEYS.contains(&k.as_str()) {
                        (k.clone(), Value::String(REDACTED.to_string()))
                    } else {
                        (k.clone(), redact_body_value(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_body_value).collect()),
        other => other.clone(),
    }
}

/// Redact identity from a request body **string**. If the body parses as JSON,
/// [`redact_body_value`] is applied and the result re-serialized compactly;
/// otherwise the body is returned unchanged (a non-JSON body carries no JSON
/// identity keys to redact). Re-serialization is compact to match how clients
/// send request bodies (no incidental whitespace churn in the fixture).
pub fn redact_body_str(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => redact_body_value(&value).to_string(),
        Err(_) => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_is_redacted_case_insensitively() {
        assert!(is_secret_header("Authorization"));
        assert!(is_secret_header("authorization"));
        assert!(is_secret_header("AUTHORIZATION"));
        assert_eq!(
            redact_value("Authorization", "Bearer sk-secret-123"),
            REDACTED
        );
    }

    #[test]
    fn api_key_oauth_and_cookie_headers_are_redacted() {
        for name in [
            "x-api-key",
            "api-key",
            "Cookie",
            "Set-Cookie",
            "OpenAI-Organization",
            "chatgpt-account-id",
            "x-oauth-token",
            "x-stainless-account-id",
        ] {
            assert!(is_secret_header(name), "{name} should be secret");
            assert_eq!(redact_value(name, "super-secret"), REDACTED, "{name}");
        }
    }

    #[test]
    fn non_secret_headers_pass_through_unchanged() {
        assert!(!is_secret_header("content-type"));
        assert!(!is_secret_header("user-agent"));
        assert_eq!(
            redact_value("Content-Type", "application/json"),
            "application/json"
        );
    }

    #[test]
    fn redact_headers_preserves_names_and_order_and_redacts_only_secrets() {
        let captured = vec![
            Header::new("Host", "api.openai.com"),
            Header::new("Authorization", "Bearer sk-live-deadbeef"),
            Header::new("Content-Type", "application/json"),
            Header::new("Cookie", "session=abc123"),
            Header::new("X-Api-Key", "key-987"),
        ];

        let redacted = redact_headers(&captured);

        assert_eq!(
            redacted,
            vec![
                Header::new("Host", "api.openai.com"),
                Header::new("Authorization", REDACTED),
                Header::new("Content-Type", "application/json"),
                Header::new("Cookie", REDACTED),
                Header::new("X-Api-Key", REDACTED),
            ]
        );
    }

    #[test]
    fn no_secret_material_survives_redaction() {
        let secret = "sk-live-this-must-never-be-written";
        let captured = vec![
            Header::new("authorization", format!("Bearer {secret}")),
            Header::new("x-api-key", secret),
            Header::new("cookie", format!("auth={secret}")),
        ];

        let redacted = redact_headers(&captured);

        for h in &redacted {
            assert!(
                !h.value.contains(secret),
                "secret leaked in header {}: {}",
                h.name,
                h.value
            );
        }
    }

    #[test]
    fn claude_code_session_id_header_is_redacted() {
        // Claude Code's per-session UUID is account/session identity (issue #15).
        assert!(is_secret_header("X-Claude-Code-Session-Id"));
        assert!(is_secret_header("x-claude-code-session-id"));
        assert_eq!(
            redact_value(
                "X-Claude-Code-Session-Id",
                "82970728-3980-46ae-8017-cd44af0058cb"
            ),
            REDACTED
        );
    }

    #[test]
    fn body_identity_keys_are_redacted_in_place() {
        // Claude Code carries `metadata.user_id` (and, on other paths, nested
        // account/session ids) in the request *body*; the value is redacted while
        // the key — the shape — survives.
        let body = r#"{"model":"claude-sonnet-4-5","metadata":{"user_id":"acct-uuid-1234"},"messages":[]}"#;
        let redacted = redact_body_str(body);
        let v: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(v["metadata"]["user_id"], REDACTED);
        // Non-identity structure is preserved.
        assert_eq!(v["model"], "claude-sonnet-4-5");
        assert!(v["messages"].is_array());
        // The raw id is gone.
        assert!(!redacted.contains("acct-uuid-1234"));
    }

    #[test]
    fn non_json_body_passes_through_unchanged() {
        assert_eq!(redact_body_str("not json at all"), "not json at all");
    }
}
