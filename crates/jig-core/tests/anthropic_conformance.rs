//! Offline structural conformance for the Anthropic `/v1/messages` dialect — the
//! T1/T2 acceptance tests for P3 (#15).
//!
//! Data-driven over the committed `fixtures/anthropic/*` scenarios, captured from
//! the real Anthropic backend by driving the official `claude` CLI through the
//! recorder and reduced to masked templates by `xtask derive`. These run under
//! the default `cargo test` — **no network, no credentials**: they read committed
//! bytes and compare structures.
//!
//! - **T1** (`render → strip → == response.template`): drive jig with the
//!   scenario's `drive-shape.json`, render it through [`render_anthropic`], reduce
//!   the rendered stream the same way the template was derived (parse → mask),
//!   and assert it equals the committed `response.template.json`. This is the core
//!   fidelity claim: jig's Anthropic wire output, stripped of volatile values,
//!   reproduces the real backend's structure exactly.
//! - **T2** (`request.json → strip → == request.template`): take the authoritative
//!   recording's `request.json`, apply the same masking the template was derived
//!   with, and assert it equals `request.template.json`.
//!
//! The structure mirrors `openai_conformance.rs` (the P2 harness) — the only
//! dialect-specific input is the `Dialect::Anthropic` passed to the strip step and
//! the `fixtures/anthropic` root. A failure prints the readable structural diff.
//!
//! [`render_anthropic`]: jig_core::render::render_anthropic

use std::path::{Path, PathBuf};

use jig_core::Dialect;
use jig_core::conform::{
    DriveShape, RequestTemplate, ResponseTemplate, strip_rendered_response, strip_request,
    structural_diff,
};
use serde_json::Value;

/// The workspace `fixtures/` root, resolved from this crate's manifest dir so the
/// test works regardless of the cwd `cargo test` runs from.
fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures/ exists at the workspace root")
}

/// The Anthropic scenario directories that have a full template set, sorted.
/// Driving the test off the committed tree means adding a scenario needs no test
/// edit.
fn anthropic_scenarios() -> Vec<PathBuf> {
    let dialect_root = fixtures_root().join("anthropic");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&dialect_root)
        .expect("fixtures/anthropic exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("response.template.json").exists())
        .collect();
    dirs.sort();
    assert!(
        !dirs.is_empty(),
        "no Anthropic scenarios with templates under {}",
        dialect_root.display()
    );
    dirs
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The authoritative `recordings/<client>/` for a scenario (the one whose
/// `meta.json` is `role: authoritative`), for T2's source `request.json`.
fn authoritative_recording(scenario: &Path) -> PathBuf {
    let recordings = scenario.join("recordings");
    let mut clients: Vec<PathBuf> = std::fs::read_dir(&recordings)
        .unwrap_or_else(|e| panic!("read {}: {e}", recordings.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    clients.sort();
    for client in clients {
        let meta = read_json(&client.join("meta.json"));
        if meta.get("role").and_then(Value::as_str) == Some("authoritative") {
            return client;
        }
    }
    panic!("no authoritative recording under {}", recordings.display());
}

/// Assert two templates are structurally equal, printing the readable diff and
/// the scenario/check label on failure.
fn assert_template_eq(label: &str, expected: &Value, actual: &Value) {
    let diff = structural_diff(expected, actual);
    assert!(
        diff.is_empty(),
        "{label}: structural mismatch:\n  {}",
        diff.join("\n  ")
    );
}

#[test]
fn t1_render_strip_equals_response_template() {
    for scenario in anthropic_scenarios() {
        let name = scenario.file_name().unwrap().to_string_lossy().into_owned();

        let template: ResponseTemplate =
            serde_json::from_value(read_json(&scenario.join("response.template.json")))
                .unwrap_or_else(|e| panic!("{name}: response.template.json shape: {e}"));
        let drive: DriveShape =
            serde_json::from_value(read_json(&scenario.join("drive-shape.json")))
                .unwrap_or_else(|e| panic!("{name}: drive-shape.json shape: {e}"));

        // Render via jig (Anthropic dialect), strip, and compare to the committed
        // template. The header view is reused from the template (jig's in-process
        // server does not reproduce the recorded transport headers).
        let stripped = strip_rendered_response(Dialect::Anthropic, &drive, &template.headers)
            .unwrap_or_else(|e| panic!("{name}: jig render did not parse: {e}"));

        assert_template_eq(
            &format!("T1 {name}"),
            &serde_json::to_value(&template).unwrap(),
            &serde_json::to_value(&stripped).unwrap(),
        );
    }
}

#[test]
fn t2_request_strip_equals_request_template() {
    for scenario in anthropic_scenarios() {
        let name = scenario.file_name().unwrap().to_string_lossy().into_owned();

        let template: RequestTemplate =
            serde_json::from_value(read_json(&scenario.join("request.template.json")))
                .unwrap_or_else(|e| panic!("{name}: request.template.json shape: {e}"));

        let recording = authoritative_recording(&scenario);
        let request = read_json(&recording.join("request.json"));
        let method = request["method"].as_str().expect("method");
        let path = request["path"].as_str().expect("path");
        let headers: Vec<(String, String)> = request["headers"]
            .as_array()
            .expect("headers array")
            .iter()
            .map(|h| {
                (
                    h["name"].as_str().unwrap_or_default().to_string(),
                    h["value"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body: Value = serde_json::from_str(request["body"].as_str().expect("body string"))
            .unwrap_or_else(|e| panic!("{name}: request body is not JSON: {e}"));

        let stripped = strip_request(method, path, &headers, &body);

        assert_template_eq(
            &format!("T2 {name}"),
            &serde_json::to_value(&template).unwrap(),
            &serde_json::to_value(&stripped).unwrap(),
        );
    }
}

/// The Anthropic stream terminates on the `message_stop` event, not OpenAI's
/// `[DONE]` sentinel — assert every scenario's template records that, so the
/// framing contract is part of what T1 pins.
#[test]
fn response_templates_record_the_message_stop_terminator() {
    for scenario in anthropic_scenarios() {
        let name = scenario.file_name().unwrap().to_string_lossy().into_owned();
        let template = read_json(&scenario.join("response.template.json"));
        assert_eq!(
            template["terminator"], "message_stop",
            "{name}: Anthropic templates must record the message_stop terminator"
        );
    }
}

/// Guard the redaction invariant from the test side: no credential- or
/// identity-shaped string appears anywhere under a committed Anthropic fixture
/// (the recorder redacts at capture time; this is the offline backstop, matching
/// the OpenAI harness and issue #15's acceptance "no secrets under `fixtures/`").
#[test]
fn no_secret_material_under_committed_fixtures() {
    for scenario in anthropic_scenarios() {
        for path in walk_files(&scenario) {
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);

            // No Anthropic OAuth bearer or API key shape may leak.
            assert!(
                !text.contains("sk-ant-") && !text.contains("oat01"),
                "possible Anthropic credential in {}",
                path.display()
            );
            // Any authorization / x-api-key / session-id entry must be REDACTED,
            // never carry a live value on the same logical line.
            for line in text.lines() {
                let lower = line.to_ascii_lowercase();
                if lower.contains("\"authorization\"") || lower.contains("\"x-api-key\"") {
                    assert!(
                        !line.contains("Bearer ") || line.contains("REDACTED"),
                        "authorization not redacted in {}",
                        path.display()
                    );
                }
            }
        }
    }
}

/// Every file under `dir`, recursively, sorted.
fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}
