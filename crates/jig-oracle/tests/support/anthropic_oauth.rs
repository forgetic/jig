//! Anthropic Claude **subscription OAuth** bearer resolution and Claude
//! Code-compatible request headers — the workaround the pi-SDK `subject` driver
//! needs to record real Anthropic traffic (issue #17, P6).
//!
//! # Why this is duplicated (with attribution)
//!
//! The pinned pi SDK (`pi_agent_rust`) has **no native Claude subscription
//! OAuth**: its `anthropic-messages` provider only knows API-key / bearer auth
//! and sends `system` as a single string. Driving it against the *real*
//! Anthropic backend with a subscription token therefore needs three things the
//! SDK does not supply itself, all reproduced here **verbatim in spirit** from
//! smith's
//! `smith/crates/smith-temper-agent/src/provider/anthropic_oauth.rs`:
//!
//! 1. [`request_headers`] — the Claude Code identity headers
//!    (`anthropic-beta`, `anthropic-version`, the `X-Stainless-*` family, fresh
//!    per-request UUIDs) the subscription path expects.
//! 2. [`resolve_bearer`](AnthropicOAuth::resolve_bearer) — read the OAuth access
//!    token from the shared `~/.pi/agent/auth.json` under provider key
//!    `anthropic`, accepting **both** credential schemas (the nodejs spelling
//!    `type:"oauth"` / `access` / `refresh` and the Rust SDK spelling
//!    `type:"o_auth"` / `access_token` / `refresh_token`), and refresh in place
//!    when the stored token is at or near expiry, writing it back in the schema
//!    it was read in.
//! 3. [`CLAUDE_CODE_SYSTEM_IDENTITY`] — the mandatory first `system` block. Any
//!    request whose first system block is not exactly this line is rejected with
//!    a generic `429`. Because the SDK sends `system` as a single string, the
//!    recording harness sets this identity *as* the system prompt and folds the
//!    scenario's role prompt into the user turn (see `subject.rs`).
//!
//! This is a **logic copy**, not a dependency: jig consumes `pi_agent_rust`
//! directly and takes **no** smith dependency (issue #17 / #13: "the only
//! smith-derived code is the duplicated Anthropic workaround"). The duplication
//! is deliberate and called out so a future reader knows the canonical source.
//!
//! # Testability
//!
//! Everything except the one network call ([`AnthropicEntry::refresh`]) is pure:
//! schema parsing, the identity headers, and the no-token-leak guarantees are
//! unit-tested offline with temp-file fixtures and run under the default
//! `cargo test`. The refresh itself is exercised only by the manual, online
//! recording harness.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

/// Identity line Anthropic's Claude **subscription OAuth** path requires as the
/// first `system` block. Any request whose first system block is not exactly this
/// line is rejected with a generic `429 rate_limit_error`, independent of
/// `anthropic-beta` flags. The SDK sends `system` as a single string and never
/// injects this itself, so the recording harness sends this identity as the
/// system prompt and folds the role prompt into the user turn.
///
/// Copied verbatim from smith's `anthropic_oauth.rs` (see module docs).
pub const CLAUDE_CODE_SYSTEM_IDENTITY: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Provider key under which the Anthropic credential lives in the auth file.
const PROVIDER_KEY: &str = "anthropic";
/// Compiled-in Anthropic OAuth refresh endpoint + public client id (matching the
/// SDK constants for `pi /login anthropic`).
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Refresh a token once it is within this many ms of expiry.
const REFRESH_WINDOW_MS: i64 = 5 * 60 * 1000;
/// Safety margin subtracted from a freshly issued token's lifetime.
const EXPIRY_SAFETY_MS: i64 = 5 * 60 * 1000;

/// The `anthropic-beta` flag set Claude Code sends. Copied from smith so the
/// recorded request body/headers match the real client's wire shape.
const ANTHROPIC_BETA: &str = concat!(
    "claude-code-20250219,",
    "oauth-2025-04-20,",
    "interleaved-thinking-2025-05-14,",
    "context-management-2025-06-27,",
    "prompt-caching-scope-2026-01-05,",
    "advisor-tool-2026-03-01,",
    "advanced-tool-use-2025-11-20,",
    "context-1m-2025-08-07,",
    "effort-2025-11-24,",
    "extended-cache-ttl-2025-04-11"
);

/// Why resolving the Anthropic OAuth bearer failed. The messages never echo any
/// token material — a refresh-token leak in an error string is exactly the class
/// of bug the no-token-leak unit test guards against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicOAuthError(pub String);

impl std::fmt::Display for AnthropicOAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AnthropicOAuthError {}

/// Claude Code-compatible headers injected on every Anthropic OAuth request.
///
/// Carries no token material — only client identity. The per-request UUIDs
/// (`x-client-request-id`, `X-Claude-Code-Session-Id`) are fresh each call, which
/// is why the recorder redacts the session id (see `jig_record::redact`).
///
/// Copied from smith's `request_headers()` (see module docs).
pub fn request_headers() -> HashMap<String, String> {
    HashMap::from([
        (
            "x-client-request-id".to_string(),
            Uuid::new_v4().to_string(),
        ),
        ("anthropic-beta".to_string(), ANTHROPIC_BETA.to_string()),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
        (
            "user-agent".to_string(),
            "claude-cli/2.1.139 (external, sdk-cli)".to_string(),
        ),
        ("x-app".to_string(), "cli".to_string()),
        (
            "X-Claude-Code-Session-Id".to_string(),
            Uuid::new_v4().to_string(),
        ),
        ("X-Stainless-Arch".to_string(), "x64".to_string()),
        ("X-Stainless-Lang".to_string(), "js".to_string()),
        ("X-Stainless-OS".to_string(), "Linux".to_string()),
        (
            "X-Stainless-Package-Version".to_string(),
            "0.93.0".to_string(),
        ),
        ("X-Stainless-Retry-Count".to_string(), "0".to_string()),
        ("X-Stainless-Runtime".to_string(), "node".to_string()),
        (
            "X-Stainless-Runtime-Version".to_string(),
            "v24.3.0".to_string(),
        ),
        ("X-Stainless-Timeout".to_string(), "600".to_string()),
    ])
}

/// Reads (and, when expiring, refreshes) the Anthropic subscription OAuth bearer
/// from a `~/.pi/agent/auth.json`-shaped credential file.
#[derive(Debug, Clone)]
pub struct AnthropicOAuth {
    auth_file: PathBuf,
}

impl AnthropicOAuth {
    /// Construct against an explicit auth-file path. The recording harness passes
    /// `~/.pi/agent/auth.json`; tests pass a temp fixture.
    pub fn new(auth_file: impl Into<PathBuf>) -> Self {
        Self {
            auth_file: auth_file.into(),
        }
    }

    /// Resolve a fresh access-token bearer, refreshing in place when the stored
    /// token is at or near expiry. The only network-touching method here; used by
    /// the online recording harness, never by `cargo test`.
    pub async fn resolve_bearer(&self) -> Result<String, AnthropicOAuthError> {
        let mut entry = AnthropicEntry::read(&self.auth_file)?;
        if entry.is_expiring(now_ms()) {
            entry.refresh().await?;
            entry.write_back(&self.auth_file)?;
        }
        Ok(entry.access)
    }
}

/// A parsed `anthropic` OAuth entry plus the schema it was read in.
struct AnthropicEntry {
    access: String,
    refresh: String,
    expires_ms: i64,
    /// `true` when the entry used the nodejs spelling (`access`/`refresh`).
    nodejs_schema: bool,
    /// The raw entry object, preserved so a write-back keeps unknown fields.
    raw: Map<String, Value>,
}

impl AnthropicEntry {
    /// Read and tolerantly parse the `anthropic` entry, accepting either schema.
    fn read(path: &Path) -> Result<Self, AnthropicOAuthError> {
        let raw = std::fs::read_to_string(path).map_err(|error| {
            AnthropicOAuthError(format!(
                "reading {}: {error}; run `pi /login anthropic` first",
                path.display()
            ))
        })?;
        let root: Value = serde_json::from_str(&raw)
            .map_err(|error| AnthropicOAuthError(format!("parsing {}: {error}", path.display())))?;
        let entry = root
            .get(PROVIDER_KEY)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AnthropicOAuthError(format!(
                    "no `{PROVIDER_KEY}` entry in {}; run `pi /login anthropic` first",
                    path.display()
                ))
            })?;

        let kind = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind != "oauth" && kind != "o_auth" {
            return Err(AnthropicOAuthError(format!(
                "`{PROVIDER_KEY}` entry in {} is `{kind}`, not OAuth; run \
                 `pi /login anthropic` first",
                path.display()
            )));
        }

        let nodejs_schema = entry.contains_key("access");
        let access = string_field(entry, "access", "access_token")
            .ok_or_else(|| missing_field(path, "access token"))?;
        let refresh = string_field(entry, "refresh", "refresh_token")
            .ok_or_else(|| missing_field(path, "refresh token"))?;
        let expires_ms = entry
            .get("expires")
            .and_then(Value::as_i64)
            .ok_or_else(|| missing_field(path, "expiry"))?;

        Ok(Self {
            access,
            refresh,
            expires_ms,
            nodejs_schema,
            raw: entry.clone(),
        })
    }

    /// `true` when the token is at or within [`REFRESH_WINDOW_MS`] of expiry.
    fn is_expiring(&self, now_ms: i64) -> bool {
        self.expires_ms <= now_ms.saturating_add(REFRESH_WINDOW_MS)
    }

    /// Refresh the token against the Anthropic endpoint using the stored refresh
    /// token, updating the in-memory entry in place. Network-touching; only the
    /// online recording harness reaches it. Uses the pi SDK's own HTTP client so
    /// no extra HTTP stack is pulled in.
    async fn refresh(&mut self) -> Result<(), AnthropicOAuthError> {
        let client = pi::http::client::Client::new();
        let request = client
            .post(ANTHROPIC_TOKEN_URL)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": ANTHROPIC_CLIENT_ID,
                "refresh_token": self.refresh,
            }))
            .map_err(|error| {
                AnthropicOAuthError(format!("building refresh request failed: {error}"))
            })?;

        let response = Box::pin(request.send()).await.map_err(|_| {
            // Never surface the body/error detail — it may echo the refresh token.
            AnthropicOAuthError("anthropic token refresh request failed".to_string())
        })?;
        let status = response.status();
        if !(200..300).contains(&status) {
            return Err(AnthropicOAuthError(format!(
                "anthropic token refresh failed (HTTP {status})"
            )));
        }
        let body = response.text().await.map_err(|_| {
            AnthropicOAuthError("reading anthropic token refresh response failed".to_string())
        })?;
        let refreshed: RefreshResponse = serde_json::from_str(&body).map_err(|error| {
            AnthropicOAuthError(format!("invalid token refresh response: {error}"))
        })?;

        self.access = refreshed.access_token;
        if let Some(refresh) = refreshed.refresh_token {
            self.refresh = refresh;
        }
        self.expires_ms = now_ms()
            .saturating_add(refreshed.expires_in.saturating_mul(1000))
            .saturating_sub(EXPIRY_SAFETY_MS);
        self.sync_raw();
        Ok(())
    }

    /// Mirror the refreshed fields into `raw` using the original schema's spelling
    /// so the on-disk file stays in the schema it was written in.
    fn sync_raw(&mut self) {
        let (access_key, refresh_key) = if self.nodejs_schema {
            ("access", "refresh")
        } else {
            ("access_token", "refresh_token")
        };
        self.raw
            .insert(access_key.to_string(), Value::String(self.access.clone()));
        self.raw
            .insert(refresh_key.to_string(), Value::String(self.refresh.clone()));
        self.raw
            .insert("expires".to_string(), Value::Number(self.expires_ms.into()));
    }

    /// Write the (refreshed) entry back, preserving every other provider entry.
    fn write_back(&self, path: &Path) -> Result<(), AnthropicOAuthError> {
        let mut root = match std::fs::read_to_string(path) {
            Ok(raw) => {
                serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| Value::Object(Map::new()))
            }
            Err(_) => Value::Object(Map::new()),
        };
        let object = root.as_object_mut().ok_or_else(|| {
            AnthropicOAuthError(format!("auth file {} is not a JSON object", path.display()))
        })?;
        object.insert(PROVIDER_KEY.to_string(), Value::Object(self.raw.clone()));
        let serialized = serde_json::to_string_pretty(&root).map_err(|error| {
            AnthropicOAuthError(format!("serializing auth file failed: {error}"))
        })?;
        std::fs::write(path, serialized)
            .map_err(|error| AnthropicOAuthError(format!("writing {}: {error}", path.display())))
    }
}

/// The token-endpoint refresh response (subset we consume).
#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: i64,
}

/// Read a string field accepting either of two key spellings.
fn string_field(entry: &Map<String, Value>, primary: &str, alternate: &str) -> Option<String> {
    entry
        .get(primary)
        .or_else(|| entry.get(alternate))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn missing_field(path: &Path, what: &str) -> AnthropicOAuthError {
    AnthropicOAuthError(format!(
        "`{PROVIDER_KEY}` entry in {} is missing its {what}; run `pi /login anthropic` first",
        path.display()
    ))
}

/// Current wall-clock time in unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| i64::try_from(delta.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        auth_file: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.auth_file);
        }
    }

    fn far_future_ms() -> i64 {
        now_ms() + 60 * 60 * 1000
    }

    fn write_fixture(contents: &str) -> Fixture {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "jig-anthropic-oauth-test-{}-{id}.json",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("write fixture");
        Fixture { auth_file: path }
    }

    #[test]
    fn reads_nodejs_schema_access_token() {
        let contents = serde_json::json!({
            "anthropic": {
                "type": "oauth",
                "access": "sk-ant-oat-node-access",
                "refresh": "node-refresh",
                "expires": far_future_ms(),
            }
        })
        .to_string();
        let fixture = write_fixture(&contents);
        let entry = AnthropicEntry::read(&fixture.auth_file).expect("read entry");
        assert!(entry.nodejs_schema);
        assert_eq!(entry.access, "sk-ant-oat-node-access");
        assert!(!entry.is_expiring(now_ms()));
    }

    #[test]
    fn reads_rust_schema_access_token() {
        let contents = serde_json::json!({
            "anthropic": {
                "type": "o_auth",
                "access_token": "sk-ant-oat-rust-access",
                "refresh_token": "rust-refresh",
                "expires": far_future_ms(),
            }
        })
        .to_string();
        let fixture = write_fixture(&contents);
        let entry = AnthropicEntry::read(&fixture.auth_file).expect("read entry");
        assert!(!entry.nodejs_schema);
        assert_eq!(entry.access, "sk-ant-oat-rust-access");
    }

    #[test]
    fn missing_entry_is_an_error_with_login_hint() {
        let contents = serde_json::json!({ "openai-codex": { "type": "oauth" } }).to_string();
        let fixture = write_fixture(&contents);
        let Err(error) = AnthropicEntry::read(&fixture.auth_file) else {
            panic!("expected missing entry error");
        };
        let rendered = format!("{error}");
        assert!(rendered.contains("anthropic"));
        assert!(rendered.contains("pi /login anthropic"));
    }

    #[test]
    fn non_oauth_entry_is_rejected() {
        let contents = serde_json::json!({
            "anthropic": { "type": "api_key", "key": "sk-ant-key" }
        })
        .to_string();
        let fixture = write_fixture(&contents);
        let Err(error) = AnthropicEntry::read(&fixture.auth_file) else {
            panic!("expected non-oauth rejection");
        };
        assert!(format!("{error}").contains("not OAuth"));
    }

    #[test]
    fn expiring_token_is_detected() {
        // A token expiring inside the refresh window is "expiring"; one comfortably
        // in the future is not.
        let soon = now_ms() + REFRESH_WINDOW_MS / 2;
        let entry = AnthropicEntry {
            access: "a".to_string(),
            refresh: "r".to_string(),
            expires_ms: soon,
            nodejs_schema: true,
            raw: Map::new(),
        };
        assert!(entry.is_expiring(now_ms()));
        let later = AnthropicEntry {
            expires_ms: now_ms() + 60 * 60 * 1000,
            ..entry
        };
        assert!(!later.is_expiring(now_ms()));
    }

    #[test]
    fn write_back_preserves_schema_unknown_fields_and_other_entries() {
        let contents = serde_json::json!({
            "anthropic": {
                "type": "oauth",
                "access": "old-access",
                "refresh": "old-refresh",
                "extra": "keep-me",
                "expires": 0,
            },
            "openai-codex": { "type": "oauth", "access": "keep-codex" }
        })
        .to_string();
        let fixture = write_fixture(&contents);
        let mut entry = AnthropicEntry::read(&fixture.auth_file).expect("read entry");
        entry.access = "new-access".to_string();
        entry.refresh = "new-refresh".to_string();
        entry.expires_ms = far_future_ms();
        entry.sync_raw();
        entry.write_back(&fixture.auth_file).expect("write back");

        let reread: Value =
            serde_json::from_str(&std::fs::read_to_string(&fixture.auth_file).unwrap()).unwrap();
        let anthropic = &reread["anthropic"];
        assert_eq!(anthropic["access"], "new-access");
        assert_eq!(anthropic["refresh"], "new-refresh");
        assert_eq!(anthropic["extra"], "keep-me");
        assert!(anthropic.get("access_token").is_none());
        assert_eq!(reread["openai-codex"]["access"], "keep-codex");
    }

    #[test]
    fn errors_never_contain_token_bytes() {
        // A missing-refresh error must mention the *field*, never the access token.
        let contents = serde_json::json!({
            "anthropic": {
                "type": "oauth",
                "access": "sk-ant-oat-super-secret",
                "expires": far_future_ms(),
            }
        })
        .to_string();
        let fixture = write_fixture(&contents);
        let Err(error) = AnthropicEntry::read(&fixture.auth_file) else {
            panic!("expected missing-refresh error");
        };
        let rendered = format!("{error}");
        assert!(rendered.contains("refresh token"));
        assert!(!rendered.contains("sk-ant-oat-super-secret"));
    }

    #[test]
    fn request_headers_match_claude_code_identity_without_tokens() {
        let headers = request_headers();
        assert_eq!(
            headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
        assert_eq!(headers.get("x-app").map(String::as_str), Some("cli"));
        assert_eq!(
            headers.get("user-agent").map(String::as_str),
            Some("claude-cli/2.1.139 (external, sdk-cli)")
        );
        let beta = headers.get("anthropic-beta").expect("beta header");
        for flag in [
            "claude-code-20250219",
            "oauth-2025-04-20",
            "context-1m-2025-08-07",
            "effort-2025-11-24",
        ] {
            assert!(beta.contains(flag), "missing beta flag {flag}");
        }
        assert!(Uuid::parse_str(headers.get("x-client-request-id").unwrap()).is_ok());
        assert!(Uuid::parse_str(headers.get("X-Claude-Code-Session-Id").unwrap()).is_ok());
        // No token material leaks into the identity headers.
        let rendered = format!("{headers:?}");
        assert!(!rendered.contains("sk-ant"));
        assert!(!rendered.contains("refresh"));
    }

    #[test]
    fn request_headers_use_fresh_ids() {
        let first = request_headers();
        let second = request_headers();
        assert_ne!(
            first.get("x-client-request-id"),
            second.get("x-client-request-id")
        );
        assert_ne!(
            first.get("X-Claude-Code-Session-Id"),
            second.get("X-Claude-Code-Session-Id")
        );
    }

    #[test]
    fn system_identity_is_the_exact_required_line() {
        // The literal Anthropic requires; a drift here would be a 429 at record
        // time. Pinned so the duplicated value can't silently rot.
        assert_eq!(
            CLAUDE_CODE_SYSTEM_IDENTITY,
            "You are Claude Code, Anthropic's official CLI for Claude."
        );
    }
}
