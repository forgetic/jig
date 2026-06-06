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
}
