//! On-disk fixture model and writer.
//!
//! A recording is written into the taxonomy from issue #13:
//!
//! ```text
//! fixtures/<dialect>/<scenario>/recordings/<client>/
//!   request.json       captured request: method, path, redacted headers, body
//!   response.headers   captured response status + redacted headers
//!   response.sse       the full SSE byte stream, exactly as received
//!   meta.json          free-form client label, role, versions, model, date, sha
//! ```
//!
//! The writer is dialect-agnostic and hardcodes no specific consumer: the
//! `client` label and `role` live in `meta.json` as free-form data, so the same
//! recorder serves the official-client `authoritative` recordings now and the
//! pi-SDK `subject` recordings later (P6) with no change here.
//!
//! Everything except the final byte-writing is pure data, so the model and the
//! serialization are unit-tested without touching the filesystem (the one test
//! that does write uses a temp dir).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::redact::{Header, redact_body_str, redact_headers};

/// The role a recording plays in the fixture set.
///
/// Generic on purpose (issue #18 "keep the `client`/`role` tagging generic"):
/// an *authoritative* recording comes from an official client and defines the
/// expected wire shape; a *subject* recording comes from the SDK under test and
/// is compared against the authoritative one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Produced by an official client — the reference shape.
    Authoritative,
    /// Produced by the SDK under test — compared against `authoritative`.
    Subject,
}

/// The captured request side of a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    /// Headers as captured, already redacted before serialization.
    pub headers: Vec<HeaderEntry>,
    /// The request body as UTF-8 (request bodies are JSON for every dialect).
    pub body: String,
}

/// The captured response side of a recording (status + headers; the SSE body is
/// written separately as raw bytes to preserve framing exactly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedResponse {
    pub status: u16,
    /// Headers as captured, already redacted before serialization.
    pub headers: Vec<HeaderEntry>,
}

/// A `{ "name", "value" }` header entry — the on-disk form of a [`Header`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

impl From<&Header> for HeaderEntry {
    fn from(h: &Header) -> Self {
        HeaderEntry {
            name: h.name.clone(),
            value: h.value.clone(),
        }
    }
}

/// The `meta.json` payload: free-form client label, role, and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Free-form client label, e.g. `openai-sdk`, `curl`. jig hardcodes none.
    pub client: String,
    /// What part this recording plays (`authoritative` / `subject`).
    pub role: Role,
    /// The dialect slug this recording was captured on (`openai`, …).
    pub dialect: String,
    /// The scenario name (`single-text`, `tool-call`, `tool-result-final`, …).
    pub scenario: String,
    /// Version string of the client that produced the recording, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// The model id exercised, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Capture date (ISO-8601 `YYYY-MM-DD`), supplied by the caller — the core
    /// never reads the clock so it stays deterministic and testable.
    pub captured: String,
    /// Git sha of the recorder that produced the recording.
    pub recorder_sha: String,
}

/// One complete recording: where it lives in the taxonomy plus all four files'
/// content. Build it, then [`write`](Recording::write) it under a `fixtures/`
/// root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub request: CapturedRequest,
    pub response: CapturedResponse,
    /// The full SSE byte stream, exactly as received from the upstream.
    pub response_sse: Vec<u8>,
    pub meta: Meta,
}

impl Recording {
    /// The relative directory this recording is written into, under the
    /// `fixtures/` root: `<dialect>/<scenario>/recordings/<client>`.
    pub fn relative_dir(&self) -> PathBuf {
        Path::new(&self.meta.dialect)
            .join(&self.meta.scenario)
            .join("recordings")
            .join(&self.meta.client)
    }

    /// Serialize `request.json` (pretty, redacted-by-construction).
    pub fn request_json(&self) -> String {
        // The request is already redacted before a Recording is built (see
        // `redacted_request`); serialization cannot reintroduce a secret.
        serde_json::to_string_pretty(&self.request).expect("CapturedRequest serializes")
    }

    /// Serialize `response.headers` (pretty).
    pub fn response_headers_json(&self) -> String {
        serde_json::to_string_pretty(&self.response).expect("CapturedResponse serializes")
    }

    /// Serialize `meta.json` (pretty).
    pub fn meta_json(&self) -> String {
        serde_json::to_string_pretty(&self.meta).expect("Meta serializes")
    }

    /// Write all four files under `fixtures_root/<dialect>/<scenario>/recordings/
    /// <client>/`, creating directories as needed. Existing files are
    /// overwritten so re-recording a scenario refreshes it in place.
    pub fn write(&self, fixtures_root: &Path) -> io::Result<PathBuf> {
        let dir = fixtures_root.join(self.relative_dir());
        std::fs::create_dir_all(&dir)?;

        std::fs::write(dir.join("request.json"), self.request_json())?;
        std::fs::write(dir.join("response.headers"), self.response_headers_json())?;
        std::fs::write(dir.join("response.sse"), &self.response_sse)?;
        std::fs::write(dir.join("meta.json"), self.meta_json())?;

        Ok(dir)
    }
}

/// Build a [`CapturedRequest`] from raw captured pieces, redacting both headers
/// **and** identity in the JSON body. This is the only constructor used by the
/// proxy, so a request can never reach a [`Recording`] with un-redacted headers
/// or a body-carried account/session id (see [`redact_body_str`]).
pub fn redacted_request(
    method: impl Into<String>,
    path: impl Into<String>,
    headers: &[Header],
    body: impl Into<String>,
) -> CapturedRequest {
    CapturedRequest {
        method: method.into(),
        path: path.into(),
        headers: redact_headers(headers)
            .iter()
            .map(HeaderEntry::from)
            .collect(),
        body: redact_body_str(&body.into()),
    }
}

/// Build a [`CapturedResponse`] from raw captured pieces, redacting headers in
/// the process (responses can carry `set-cookie`).
pub fn redacted_response(status: u16, headers: &[Header]) -> CapturedResponse {
    CapturedResponse {
        status,
        headers: redact_headers(headers)
            .iter()
            .map(HeaderEntry::from)
            .collect(),
    }
}

/// Whether an SSE byte stream carries the OpenAI/DeepSeek `[DONE]` terminator as
/// its **last** `data:` event — the acceptance signal for a complete
/// chat-completions recording.
///
/// Scans `data:` lines rather than the raw buffer tail so it is robust to a
/// captured chunked-transfer terminator (`0\r\n\r\n`) trailing the final event.
pub fn sse_ends_in_done(sse: &[u8]) -> bool {
    let text = String::from_utf8_lossy(sse);
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .rfind(|payload| !payload.is_empty())
        == Some("[DONE]")
}

/// Parse a captured request/response body as JSON, for callers that want to
/// derive `meta.model` from the request. Returns `None` if the body is not JSON.
pub fn body_as_json(body: &str) -> Option<Value> {
    serde_json::from_str(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;

    fn sample_meta() -> Meta {
        Meta {
            client: "openai-sdk".to_string(),
            role: Role::Authoritative,
            dialect: "openai".to_string(),
            scenario: "single-text".to_string(),
            client_version: Some("1.2.3".to_string()),
            model: Some("gpt-4o-mini".to_string()),
            captured: "2026-06-06".to_string(),
            recorder_sha: "abc1234".to_string(),
        }
    }

    fn sample_recording() -> Recording {
        let req_headers = vec![
            Header::new("Host", "api.openai.com"),
            Header::new("Authorization", "Bearer sk-live-secret"),
            Header::new("Content-Type", "application/json"),
        ];
        let resp_headers = vec![
            Header::new("Content-Type", "text/event-stream"),
            Header::new("Set-Cookie", "sess=secret"),
        ];
        Recording {
            request: redacted_request(
                "POST",
                "/chat/completions",
                &req_headers,
                r#"{"model":"gpt-4o-mini"}"#,
            ),
            response: redacted_response(200, &resp_headers),
            response_sse: b"data: {\"x\":1}\n\ndata: [DONE]\n\n".to_vec(),
            meta: sample_meta(),
        }
    }

    #[test]
    fn relative_dir_follows_the_taxonomy() {
        let rec = sample_recording();
        assert_eq!(
            rec.relative_dir(),
            Path::new("openai/single-text/recordings/openai-sdk")
        );
    }

    #[test]
    fn redacted_request_strips_secret_headers() {
        let rec = sample_recording();
        let auth = rec
            .request
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("authorization"))
            .unwrap();
        assert_eq!(auth.value, REDACTED);
        // Non-secret header survives.
        assert!(
            rec.request
                .headers
                .iter()
                .any(|h| h.name == "Content-Type" && h.value == "application/json")
        );
    }

    #[test]
    fn redacted_response_strips_set_cookie() {
        let rec = sample_recording();
        let cookie = rec
            .response
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("set-cookie"))
            .unwrap();
        assert_eq!(cookie.value, REDACTED);
    }

    #[test]
    fn meta_json_roundtrips_and_uses_lowercase_role() {
        let rec = sample_recording();
        let json = rec.meta_json();
        assert!(json.contains("\"role\": \"authoritative\""));
        let parsed: Meta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, rec.meta);
    }

    #[test]
    fn meta_json_omits_absent_optionals() {
        let mut meta = sample_meta();
        meta.client_version = None;
        meta.model = None;
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("client_version"));
        assert!(!json.contains("\"model\""));
    }

    #[test]
    fn sse_done_detection() {
        assert!(sse_ends_in_done(b"data: {}\n\ndata: [DONE]\n\n"));
        assert!(sse_ends_in_done(b"data: [DONE]"));
        // Robust to a trailing chunked-transfer terminator after the final event.
        assert!(sse_ends_in_done(b"data: {}\n\ndata: [DONE]\n\n0\r\n\r\n"));
        assert!(!sse_ends_in_done(b"data: {}\n\n"));
        // `[DONE]` not as the last data line does not count.
        assert!(!sse_ends_in_done(b"data: [DONE]\n\ndata: {}\n\n"));
        assert!(!sse_ends_in_done(b""));
    }

    #[test]
    fn write_lays_out_all_four_files_and_leaks_no_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = sample_recording();
        let dir = rec.write(tmp.path()).unwrap();

        for name in [
            "request.json",
            "response.headers",
            "response.sse",
            "meta.json",
        ] {
            assert!(dir.join(name).exists(), "missing {name}");
        }

        // The SSE body is preserved byte-for-byte.
        let sse = std::fs::read(dir.join("response.sse")).unwrap();
        assert_eq!(sse, rec.response_sse);

        // No secret material anywhere under the fixtures root.
        for name in ["request.json", "response.headers", "meta.json"] {
            let content = std::fs::read_to_string(dir.join(name)).unwrap();
            assert!(
                !content.contains("sk-live-secret"),
                "secret leaked in {name}"
            );
            assert!(!content.contains("sess=secret"), "cookie leaked in {name}");
        }
    }

    #[test]
    fn body_as_json_parses_or_returns_none() {
        assert_eq!(
            body_as_json(r#"{"model":"x"}"#),
            Some(serde_json::json!({ "model": "x" }))
        );
        assert_eq!(body_as_json("not json"), None);
    }
}
