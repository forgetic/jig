//! `jig record` — a passthrough recorder.
//!
//! The recorder is the capture substrate the rest of the fixture pipeline
//! derives from (issue #18, part of #13). It stands up a passthrough proxy in
//! front of a real LLM backend, drives an **official** client through it, and
//! writes the interaction to client/role-tagged on-disk fixtures with every
//! secret redacted. It only **captures** — no parsing or template derivation
//! happens here (that is P2).
//!
//! # Shape
//!
//! Mirrors `jig-server`: a single-threaded skein runtime owned by the blocking
//! entry point, so a synchronous caller can drive it. The pieces are split so
//! the network-free parts are unit-testable on their own:
//!
//! - [`redact`] — pure secret redaction over captured headers.
//! - [`fixture`] — the on-disk [`fixture::Recording`] model and writer.
//! - [`route`] — path → dialect → upstream, mirroring the server's route table.
//! - [`proxy`] — the one async, network-touching part: forward + stream-capture.
//! - [`pump`] — the concurrent capture pump for multi-connection clients: the
//!   recorder on its own runtime thread while the caller drives the client.
//!
//! # Usage (manual)
//!
//! Recording against a real backend is manual — it needs a live API key and
//! network — so it is not part of `cargo test`. The [`record_once`] entry point
//! drives one capture; the binary's `record` subcommand wires it to provenance
//! (capture date, recorder git sha, client label/role) and a `fixtures/` root.

use std::io;
use std::path::{Path, PathBuf};

pub mod fixture;
pub mod proxy;
pub mod pump;
pub mod redact;
pub mod route;

pub use fixture::{
    CapturedRequest, CapturedResponse, Meta, Recording, Role, body_as_json, redacted_request,
    redacted_response, sse_ends_in_done,
};
pub use proxy::{ClientRequest, UpstreamResponse, bind, handle_connection, proxy_once};
pub use pump::{CapturePump, Exchange};
pub use redact::{Header, REDACTED, redact_headers};
pub use route::{Route, dialect_slug};

/// Caller-supplied provenance for a recording's `meta.json`.
///
/// The core never reads the clock or shells out to git, so the binary supplies
/// the capture date, recorder sha, client label/role, and (optionally) the
/// client version. The model is filled in from the captured request body when
/// not given explicitly.
#[derive(Debug, Clone)]
pub struct Provenance {
    /// Free-form client label, e.g. `openai-sdk`, `curl`.
    pub client: String,
    /// What part this recording plays.
    pub role: Role,
    /// Scenario name, e.g. `single-text`, `tool-call`, `tool-result-final`.
    pub scenario: String,
    /// Client version, if known.
    pub client_version: Option<String>,
    /// Capture date as ISO-8601 `YYYY-MM-DD`.
    pub captured: String,
    /// Git sha of the recorder.
    pub recorder_sha: String,
}

/// Assemble a [`Recording`] from a captured exchange plus caller provenance,
/// redacting headers and deriving `meta` (dialect from the route, model from the
/// request body when present).
///
/// Pure and synchronous: given the captured pieces it does no I/O, so it is
/// covered by the fixture tests without a network leg.
pub fn build_recording(
    request: &ClientRequest,
    response: &UpstreamResponse,
    route: &Route,
    provenance: &Provenance,
) -> Recording {
    let body = String::from_utf8_lossy(&request.body).into_owned();
    let model = body_as_json(&body)
        .and_then(|v| v.get("model").and_then(|m| m.as_str().map(str::to_string)));

    let meta = Meta {
        client: provenance.client.clone(),
        role: provenance.role,
        dialect: dialect_slug(route.dialect).to_string(),
        scenario: provenance.scenario.clone(),
        client_version: provenance.client_version.clone(),
        model,
        captured: provenance.captured.clone(),
        recorder_sha: provenance.recorder_sha.clone(),
    };

    Recording {
        request: redacted_request(
            request.method.clone(),
            request.path().to_string(),
            &request.headers,
            body,
        ),
        response: redacted_response(response.status, &response.headers),
        response_sse: response.body.clone(),
        meta,
    }
}

/// One end-to-end capture, returning the path the recording was written to.
///
/// Binds the proxy, prints the loopback `base_url` to `out` so the caller can
/// point a client at it, accepts exactly one request, forwards it to the
/// upstream over HTTPS while streaming the response back, then redacts, builds,
/// and writes the recording under `fixtures_root`.
///
/// This is the async, network-touching entry point — driven manually against a
/// real backend, never from `cargo test`. Like every socket-touching future it
/// must run inside a skein task; `cx` is that task's capability context.
pub async fn record_once(
    cx: &skein::cx::Cx,
    fixtures_root: &Path,
    provenance: &Provenance,
    upstream_host_override: Option<&str>,
    mut out: impl io::Write,
) -> io::Result<PathBuf> {
    let listener = bind(cx).await?;
    let addr = listener.local_addr()?;
    writeln!(out, "http://{addr}")?;
    out.flush()?;

    let (request, response, route) = proxy_once(cx, &listener, upstream_host_override).await?;
    let recording = build_recording(&request, &response, &route, provenance);
    recording.write(fixtures_root)
}

/// Blocking wrapper around [`record_once`] for synchronous callers (the binary).
///
/// Owns the single-threaded skein runtime so the `jig` binary stays
/// runtime-free — the same division of labor as `jig-server`, which hides its
/// runtime behind `FakeLlm`. The spawned task needs `'static` captures, so the
/// borrowed parameters are cloned into it.
pub fn record_once_blocking(
    fixtures_root: &Path,
    provenance: &Provenance,
    upstream_host_override: Option<&str>,
    out: impl io::Write + Send + 'static,
) -> io::Result<PathBuf> {
    let fixtures_root = fixtures_root.to_path_buf();
    let provenance = provenance.clone();
    let upstream_host_override = upstream_host_override.map(str::to_string);
    jig_runtime::block_on(move |cx| async move {
        record_once(
            &cx,
            &fixtures_root,
            &provenance,
            upstream_host_override.as_deref(),
            out,
        )
        .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jig_core::Dialect;

    fn provenance() -> Provenance {
        Provenance {
            client: "openai-sdk".to_string(),
            role: Role::Authoritative,
            scenario: "single-text".to_string(),
            client_version: Some("1.0.0".to_string()),
            captured: "2026-06-06".to_string(),
            recorder_sha: "deadbee".to_string(),
        }
    }

    #[test]
    fn build_recording_derives_dialect_and_model_and_redacts() {
        let request = ClientRequest {
            method: "POST".to_string(),
            target: "/chat/completions?x=1".to_string(),
            headers: vec![
                Header::new("Authorization", "Bearer sk-secret"),
                Header::new("Content-Type", "application/json"),
            ],
            body: br#"{"model":"gpt-4o-mini","stream":true}"#.to_vec(),
        };
        let response = UpstreamResponse {
            status: 200,
            headers: vec![Header::new("Content-Type", "text/event-stream")],
            body: b"data: [DONE]\n\n".to_vec(),
        };
        let route = Route::resolve("/chat/completions").unwrap();

        let rec = build_recording(&request, &response, &route, &provenance());

        // Dialect comes from the route; model from the body.
        assert_eq!(rec.meta.dialect, "openai");
        assert_eq!(rec.meta.model.as_deref(), Some("gpt-4o-mini"));
        // Path has the query stripped.
        assert_eq!(rec.request.path, "/chat/completions");
        // Auth header is redacted in the captured request.
        assert!(
            rec.request
                .headers
                .iter()
                .any(|h| h.name == "Authorization" && h.value == REDACTED)
        );
        // SSE body is preserved and recognized as complete.
        assert!(sse_ends_in_done(&rec.response_sse));
        assert_eq!(route.dialect, Dialect::OpenAi);
    }

    #[test]
    fn build_recording_tolerates_a_non_json_body() {
        let request = ClientRequest {
            method: "POST".to_string(),
            target: "/v1/messages".to_string(),
            headers: vec![],
            body: b"not json".to_vec(),
        };
        let response = UpstreamResponse {
            status: 200,
            headers: vec![],
            body: vec![],
        };
        let route = Route::resolve("/v1/messages").unwrap();
        let rec = build_recording(&request, &response, &route, &provenance());
        assert_eq!(rec.meta.dialect, "anthropic");
        assert_eq!(rec.meta.model, None);
    }
}
