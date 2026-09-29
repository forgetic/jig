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
//! Layered like `jig-server` (design `docs/explanation/sans-io-shell.md` §5.1
//! and §5.7), so each part is tested on its own and an embedder can take only
//! what it needs:
//!
//! - [`redact`], [`fixture`], [`route`] and [`proxy`]: pure data. Redaction,
//!   the on-disk [`fixture::Recording`], path → dialect → upstream, and the
//!   request and response as captured.
//! - [`relay`], the pure core: client requests in; upstream fetches, relayed
//!   bytes and captures out.
//! - [`io`], its I/O step: steploop's HTTP/1 server and client planners on one
//!   reactor. Pure as well.
//! - [`host`], its embedder: captures to a channel, log lines to stderr.
//! - [`Recorder`], the three on one OS thread, which a synchronous caller
//!   drives: `base_url`, `next_capture`, `stop`.
//!
//! # Usage (manual)
//!
//! Recording against a real backend is manual — it needs a live API key and
//! network — so it is not part of `cargo test`; the tests stand local
//! upstreams in for the real ones. [`record_once`] drives one capture; the
//! binary's `record` subcommand wires it to provenance (capture date,
//! recorder git sha, client label/role) and a `fixtures/` root. The capture
//! examples run a [`Recorder`] in [`Mode::Pump`] while they drive a client.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod fixture;
mod handle;
pub mod host;
pub mod io;
pub mod proxy;
pub mod redact;
pub mod relay;
pub mod route;

pub use fixture::{
    CapturedRequest, CapturedResponse, Meta, Recording, Role, body_as_json, redacted_request,
    redacted_response, sse_ends_in_done,
};
pub use handle::{Recorder, RecorderConfig};
pub use host::RecorderHost;
pub use io::RecorderIo;
pub use proxy::{ClientRequest, UpstreamResponse};
pub use redact::{Header, REDACTED, redact_headers};
pub use relay::{Exchange, Mode, RecorderCore, UpstreamOverride};
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
/// Starts a [`Recorder`] in [`Mode::Once`], prints its loopback `base_url` to
/// `out` so the caller can point a client at it, and waits for the first
/// routable exchange: preflights are answered with `204` and don't count.
/// That exchange is forwarded upstream over HTTPS while its response streams
/// back, then redacted, built and written under `fixtures_root`. If it fails,
/// so does this, and the recorder's log on stderr says why.
///
/// Driven manually against a real backend, never from `cargo test`.
pub fn record_once(
    fixtures_root: &Path,
    provenance: &Provenance,
    upstream_host_override: Option<&str>,
    mut out: impl std::io::Write,
) -> std::io::Result<PathBuf> {
    let recorder = Recorder::start(RecorderConfig {
        mode: Mode::Once,
        upstream_host: upstream_host_override.map(str::to_string),
        ..RecorderConfig::default()
    })?;
    writeln!(out, "{}", recorder.base_url())?;
    out.flush()?;

    // However long the operator takes to drive the client.
    let (request, response, route) = recorder.next_capture(Duration::MAX)?;
    let recording = build_recording(&request, &response, &route, provenance);
    recording.write(fixtures_root)
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
