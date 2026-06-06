//! Offline structural conformance for the OpenAI/DeepSeek `/chat/completions`
//! dialect — the T1/T2 acceptance tests for P2 (#14).
//!
//! Data-driven over the committed `fixtures/openai/*` scenarios, captured from a
//! real backend (DeepSeek) and reduced to masked templates by `xtask derive`.
//! These run under the default `cargo test`: **no network, no credentials** —
//! they read committed bytes and compare structures.
//!
//! - **T1** (`render → strip → == response.template`): drive jig with the
//!   scenario's `drive-shape.json`, render it through [`render_openai`], reduce
//!   the rendered stream the same way the template was derived (parse → mask),
//!   and assert it equals the committed `response.template.json`. This is the
//!   core fidelity claim: jig's wire output, stripped of volatile values,
//!   reproduces the real backend's structure exactly.
//! - **T2** (`request.json → strip → == request.template`): take the
//!   authoritative recording's `request.json`, apply the same masking the
//!   template was derived with, and assert it equals `request.template.json`.
//!
//! A failure prints the readable structural diff (which JSON path diverged), so
//! the delta is obvious without eyeballing two large blobs.

use std::path::{Path, PathBuf};

use jig_core::conform::{
    DriveShape, RequestTemplate, ResponseTemplate, strip_rendered_response, strip_request,
    structural_diff,
};
use serde_json::Value;

/// The workspace `fixtures/` root, resolved from this crate's manifest dir
/// (`crates/jig-core`) so the test works regardless of the cwd `cargo test` runs
/// from.
fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures/ exists at the workspace root")
}

/// The OpenAI scenario directories that have a full template set, sorted. Driving
/// the test off the committed tree means adding a scenario needs no test edit.
fn openai_scenarios() -> Vec<PathBuf> {
    let dialect_root = fixtures_root().join("openai");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&dialect_root)
        .expect("fixtures/openai exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("response.template.json").exists())
        .collect();
    dirs.sort();
    assert!(
        !dirs.is_empty(),
        "no OpenAI scenarios with templates under {}",
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
    for scenario in openai_scenarios() {
        let name = scenario.file_name().unwrap().to_string_lossy().into_owned();

        // The committed template and the drive shape jig is driven with.
        let template: ResponseTemplate =
            serde_json::from_value(read_json(&scenario.join("response.template.json")))
                .unwrap_or_else(|e| panic!("{name}: response.template.json shape: {e}"));
        let drive: DriveShape =
            serde_json::from_value(read_json(&scenario.join("drive-shape.json")))
                .unwrap_or_else(|e| panic!("{name}: drive-shape.json shape: {e}"));

        // Render via jig, strip, and compare to the committed template. The
        // header view is reused from the template (jig's in-process server does
        // not reproduce the recorded transport headers — see strip_rendered_response).
        let stripped = strip_rendered_response(&drive, &template.headers)
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
    for scenario in openai_scenarios() {
        let name = scenario.file_name().unwrap().to_string_lossy().into_owned();

        let template: RequestTemplate =
            serde_json::from_value(read_json(&scenario.join("request.template.json")))
                .unwrap_or_else(|e| panic!("{name}: request.template.json shape: {e}"));

        // The authoritative recording's captured request.json (a CapturedRequest).
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

/// Guard the redaction invariant from the test side too: no bearer/secret-shaped
/// string appears anywhere under a committed OpenAI fixture (the recorder redacts
/// at capture time; this is the offline backstop the how-to calls for).
#[test]
fn no_secret_material_under_committed_fixtures() {
    for scenario in openai_scenarios() {
        for path in walk_files(&scenario) {
            // The raw SSE is bytes; everything else is UTF-8 text. Read lossily.
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("Bearer sk-") && !text.contains("sk-live"),
                "possible secret in {}",
                path.display()
            );
            // The redacted bearer must be the stable placeholder, never a value.
            for line in text.lines() {
                if line.to_ascii_lowercase().contains("\"authorization\"") {
                    // The next value on the same logical entry should be REDACTED;
                    // assert no long token leaked on this line.
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
