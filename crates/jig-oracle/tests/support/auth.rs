//! Real-credential resolution for the pi-SDK **recording** harness (issue #17).
//!
//! The online recording harness (`tests/pi_subject_record.rs`, `#[ignore]`)
//! drives the SDK against the real backends, so it needs the real bearer per
//! dialect from the shared `~/.pi/agent/auth.json`:
//!
//! - **OpenAI/DeepSeek** — the `deepseek` `api_key`, sent as a standard bearer.
//! - **Codex** — the `openai-codex` OAuth `access` JWT (which carries the
//!   `chatgpt_account_id` claim the SDK's codex provider extracts itself). Bearer
//!   resolution only; no special headers.
//! - **Anthropic** — resolved through the duplicated subscription workaround
//!   ([`crate::anthropic_oauth`]), which also refreshes a near-expiry token.
//!
//! These functions touch the real credential file (and, for anthropic, the
//! network on refresh), so they are used **only** by the manual recording leg —
//! never by `cargo test`. The parsing here is the offline-testable part; the
//! resolved values are secrets and never logged.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::anthropic_oauth::{AnthropicOAuth, AnthropicOAuthError};
use super::subject::Dialect;

/// The default shared auth file (`~/.pi/agent/auth.json`).
pub fn default_auth_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".pi/agent/auth.json")
}

/// Why resolving a dialect bearer from the auth file failed. Never carries token
/// material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError(pub String);

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AuthError {}

impl From<AnthropicOAuthError> for AuthError {
    fn from(error: AnthropicOAuthError) -> Self {
        AuthError(error.0)
    }
}

/// Read the `deepseek` API key from the auth file. Offline + pure (no refresh).
pub fn deepseek_api_key(auth_file: &Path) -> Result<String, AuthError> {
    let root = read_auth(auth_file)?;
    let entry = root
        .get("deepseek")
        .and_then(Value::as_object)
        .ok_or_else(|| AuthError("no `deepseek` entry in auth file".to_string()))?;
    entry
        .get("key")
        .and_then(Value::as_str)
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| AuthError("`deepseek` entry is missing its `key`".to_string()))
}

/// Read the `openai-codex` OAuth access JWT from the auth file. Offline + pure:
/// the SDK's codex provider extracts the `chatgpt_account_id` claim from the JWT
/// itself, so only the bearer is resolved here (no account header, no refresh).
pub fn codex_bearer(auth_file: &Path) -> Result<String, AuthError> {
    let root = read_auth(auth_file)?;
    let entry = root
        .get("openai-codex")
        .and_then(Value::as_object)
        .ok_or_else(|| AuthError("no `openai-codex` entry in auth file".to_string()))?;
    // Accept both schema spellings, mirroring the anthropic reader.
    entry
        .get("access")
        .or_else(|| entry.get("access_token"))
        .and_then(Value::as_str)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AuthError("`openai-codex` entry is missing its access token".to_string()))
}

/// Resolve the bearer for `dialect` against the real auth file, refreshing the
/// anthropic token in place when it is near expiry. Async because the anthropic
/// path may hit the network; the others are immediate.
pub async fn resolve_bearer(dialect: Dialect, auth_file: &Path) -> Result<String, AuthError> {
    match dialect {
        Dialect::OpenAi => deepseek_api_key(auth_file),
        Dialect::Codex => codex_bearer(auth_file),
        Dialect::Anthropic => Ok(AnthropicOAuth::new(auth_file).resolve_bearer().await?),
    }
}

fn read_auth(path: &Path) -> Result<Value, AuthError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| AuthError(format!("reading {}: {error}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|error| AuthError(format!("parsing {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        path: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn fixture(contents: Value) -> Fixture {
        let id = NEXT.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("jig-auth-test-{}-{id}.json", std::process::id()));
        std::fs::write(&path, contents.to_string()).unwrap();
        Fixture { path }
    }

    #[test]
    fn reads_deepseek_api_key() {
        let fx = fixture(serde_json::json!({
            "deepseek": { "type": "api_key", "key": "sk-deepseek-123" }
        }));
        assert_eq!(deepseek_api_key(&fx.path).unwrap(), "sk-deepseek-123");
    }

    #[test]
    fn missing_deepseek_is_an_error() {
        let fx = fixture(serde_json::json!({ "anthropic": {} }));
        assert!(deepseek_api_key(&fx.path).is_err());
    }

    #[test]
    fn reads_codex_access_jwt_in_both_schemas() {
        let node = fixture(serde_json::json!({
            "openai-codex": { "type": "oauth", "access": "jwt.node.token", "accountId": "acct" }
        }));
        assert_eq!(codex_bearer(&node.path).unwrap(), "jwt.node.token");
        let rust = fixture(serde_json::json!({
            "openai-codex": { "type": "o_auth", "access_token": "jwt.rust.token" }
        }));
        assert_eq!(codex_bearer(&rust.path).unwrap(), "jwt.rust.token");
    }

    #[test]
    fn errors_do_not_leak_token_material() {
        // A malformed codex entry must not echo any neighbouring token.
        let fx = fixture(serde_json::json!({
            "openai-codex": { "type": "oauth" },
            "deepseek": { "key": "sk-deepseek-super-secret" }
        }));
        let err = codex_bearer(&fx.path).unwrap_err();
        assert!(!format!("{err}").contains("sk-deepseek-super-secret"));
    }
}
