//! Template derivation: turn committed authoritative recordings into the masked
//! `*.template.json` + `drive-shape.json` artifacts the offline conformance
//! tests assert against.
//!
//! This is the offline, deterministic complement to `record`: `record` captures
//! real traffic (online, manual), and `derive` reduces those captures to the
//! committed structural skeletons (offline, repeatable). Re-running it over the
//! same recordings always produces byte-identical artifacts — the P2 (#14)
//! acceptance criterion "re-deriving templates from the same recordings is
//! deterministic" — because every step is a pure function of the recording bytes
//! (the masking policy and template shapes live in `jig_core::conform`).
//!
//! The crate split mirrors the rest of `xtask`: the pure reduction
//! ([`derive_artifacts`], over the three recording byte-strings) is unit-tested
//! offline, while the filesystem walk and writes ([`derive_tree`]) are the thin
//! impure edge in `main`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jig_core::Dialect;
use jig_core::conform::{
    DriveShape, RequestTemplate, ResponseTemplate, derive_drive_shape, derive_request_template,
    derive_response_template,
};
use serde_json::Value;

/// The three derived artifacts for one scenario, ready to serialize and write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifacts {
    pub response_template: ResponseTemplate,
    pub request_template: RequestTemplate,
    pub drive_shape: DriveShape,
}

/// Why deriving artifacts for a recording failed. Derivation is strict: a
/// malformed recording is an operator error worth surfacing, not something to
/// paper over with a default.
#[derive(Debug)]
pub enum DeriveError {
    /// A recording file (`response.sse`, `response.headers`, `request.json`) was
    /// missing or unreadable.
    Io(PathBuf, io::Error),
    /// A recording's `request.json` / `response.headers` was not the expected
    /// JSON shape. The string explains what was wrong.
    Shape(PathBuf, String),
    /// The captured `response.sse` did not parse under its dialect's SSE parser.
    Parse(PathBuf, String),
}

impl std::fmt::Display for DeriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeriveError::Io(path, err) => write!(f, "{}: {err}", path.display()),
            DeriveError::Shape(path, msg) => write!(f, "{}: {msg}", path.display()),
            DeriveError::Parse(path, msg) => write!(f, "{}: {msg}", path.display()),
        }
    }
}

impl std::error::Error for DeriveError {}

/// Reduce the three recording byte-strings of one authoritative recording into
/// the derived [`Artifacts`]. Pure: no I/O, no clock — so it is fully
/// unit-tested and its output is a deterministic function of the inputs.
///
/// `request_json` is the recording's `request.json` (a `CapturedRequest`);
/// `response_headers_json` is its `response.headers` (a `CapturedResponse`);
/// `response_sse` is the raw SSE byte stream.
pub fn derive_artifacts(
    request_json: &str,
    response_headers_json: &str,
    response_sse: &[u8],
    request_path: &Path,
    response_headers_path: &Path,
    response_sse_path: &Path,
) -> Result<Artifacts, DeriveError> {
    let request: Value = serde_json::from_str(request_json)
        .map_err(|e| DeriveError::Shape(request_path.to_owned(), format!("invalid JSON: {e}")))?;
    let method = str_field(&request, "method")
        .ok_or_else(|| DeriveError::Shape(request_path.to_owned(), "missing `method`".into()))?;
    let path = str_field(&request, "path")
        .ok_or_else(|| DeriveError::Shape(request_path.to_owned(), "missing `path`".into()))?;
    let request_headers = header_pairs(&request, request_path)?;
    let body_str = str_field(&request, "body")
        .ok_or_else(|| DeriveError::Shape(request_path.to_owned(), "missing `body`".into()))?;
    let body: Value = serde_json::from_str(&body_str).map_err(|e| {
        DeriveError::Shape(request_path.to_owned(), format!("body is not JSON: {e}"))
    })?;

    let response: Value = serde_json::from_str(response_headers_json).map_err(|e| {
        DeriveError::Shape(
            response_headers_path.to_owned(),
            format!("invalid JSON: {e}"),
        )
    })?;
    let response_headers = header_pairs(&response, response_headers_path)?;

    // The dialect is decided by the request path (the same mapping the recorder
    // routes on), so the response SSE is reduced with the right parser. A path
    // that does not route is an operator error worth surfacing.
    let dialect = Dialect::for_path(&path).ok_or_else(|| {
        DeriveError::Shape(
            request_path.to_owned(),
            format!("path {path:?} does not map to a known dialect"),
        )
    })?;

    let response_template = derive_response_template(dialect, response_sse, &response_headers)
        .map_err(|e| DeriveError::Parse(response_sse_path.to_owned(), e.to_string()))?;
    let drive_shape = derive_drive_shape(dialect, response_sse)
        .map_err(|e| DeriveError::Parse(response_sse_path.to_owned(), e.to_string()))?;
    let request_template = derive_request_template(&method, &path, &request_headers, &body);

    Ok(Artifacts {
        response_template,
        request_template,
        drive_shape,
    })
}

/// Read a string field from a JSON object.
fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Extract the `headers: [{name, value}]` array from a captured request/response
/// JSON into `(name, value)` pairs, preserving order.
fn header_pairs(value: &Value, path: &Path) -> Result<Vec<(String, String)>, DeriveError> {
    let headers = value
        .get("headers")
        .and_then(Value::as_array)
        .ok_or_else(|| DeriveError::Shape(path.to_owned(), "missing `headers` array".into()))?;
    let mut pairs = Vec::with_capacity(headers.len());
    for h in headers {
        let name = str_field(h, "name").ok_or_else(|| {
            DeriveError::Shape(path.to_owned(), "header entry missing `name`".into())
        })?;
        let value = str_field(h, "value").ok_or_else(|| {
            DeriveError::Shape(path.to_owned(), "header entry missing `value`".into())
        })?;
        pairs.push((name, value));
    }
    Ok(pairs)
}

/// Pretty-serialize the three artifacts to their on-disk JSON forms, with a
/// trailing newline each (so the committed files end cleanly and diff well).
pub fn serialize_artifacts(artifacts: &Artifacts) -> (String, String, String) {
    let response = serde_json::to_string_pretty(&artifacts.response_template)
        .expect("ResponseTemplate serializes")
        + "\n";
    let request = serde_json::to_string_pretty(&artifacts.request_template)
        .expect("RequestTemplate serializes")
        + "\n";
    let drive =
        serde_json::to_string_pretty(&artifacts.drive_shape).expect("DriveShape serializes") + "\n";
    (response, request, drive)
}

/// Where one authoritative recording lives and where its templates go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioDir {
    pub dialect: String,
    pub scenario: String,
    /// The authoritative `recordings/<client>/` directory to derive from.
    pub recording_dir: PathBuf,
    /// The scenario root the `*.template.json`/`drive-shape.json` are written to.
    pub scenario_root: PathBuf,
}

/// The recording files derivation reads, relative to a `recordings/<client>/`.
const RESPONSE_SSE: &str = "response.sse";
const RESPONSE_HEADERS: &str = "response.headers";
const REQUEST_JSON: &str = "request.json";
/// The artifact files derivation writes, relative to the scenario root.
const RESPONSE_TEMPLATE: &str = "response.template.json";
const REQUEST_TEMPLATE: &str = "request.template.json";
const DRIVE_SHAPE: &str = "drive-shape.json";

/// Derive and write the artifacts for one scenario directory, returning the
/// paths written. The impure leg: it reads three files and writes three.
pub fn derive_scenario(dir: &ScenarioDir) -> Result<Vec<PathBuf>, DeriveError> {
    let req_path = dir.recording_dir.join(REQUEST_JSON);
    let resp_headers_path = dir.recording_dir.join(RESPONSE_HEADERS);
    let resp_sse_path = dir.recording_dir.join(RESPONSE_SSE);

    let request_json = read(&req_path)?;
    let response_headers_json = read(&resp_headers_path)?;
    let response_sse = read_bytes(&resp_sse_path)?;

    let artifacts = derive_artifacts(
        &request_json,
        &response_headers_json,
        &response_sse,
        &req_path,
        &resp_headers_path,
        &resp_sse_path,
    )?;
    let (response, request, drive) = serialize_artifacts(&artifacts);

    let resp_out = dir.scenario_root.join(RESPONSE_TEMPLATE);
    let req_out = dir.scenario_root.join(REQUEST_TEMPLATE);
    let drive_out = dir.scenario_root.join(DRIVE_SHAPE);
    write(&resp_out, &response)?;
    write(&req_out, &request)?;
    write(&drive_out, &drive)?;

    Ok(vec![resp_out, req_out, drive_out])
}

/// Walk `fixtures_root` for every `<dialect>/<scenario>` that has an
/// authoritative recording, derive each, and return the scenario dirs processed.
///
/// "Authoritative" is the first `recordings/<client>/` whose `meta.json` has
/// `role: "authoritative"`; templates are anchored to the official client, never
/// the `subject` SDK (see the record-and-conform design). A scenario with no
/// authoritative recording is skipped (not an error — a `subject`-only scenario
/// has nothing to anchor a template to).
pub fn derive_tree(fixtures_root: &Path) -> Result<Vec<ScenarioDir>, DeriveError> {
    let mut done = Vec::new();
    for dialect_entry in read_dir_sorted(fixtures_root)? {
        if !dialect_entry.is_dir() {
            continue;
        }
        let dialect = file_name(&dialect_entry);
        for scenario_entry in read_dir_sorted(&dialect_entry)? {
            if !scenario_entry.is_dir() {
                continue;
            }
            let scenario = file_name(&scenario_entry);
            let recordings = scenario_entry.join("recordings");
            let Some(recording_dir) = authoritative_recording(&recordings)? else {
                continue;
            };
            let dir = ScenarioDir {
                dialect: dialect.clone(),
                scenario: scenario.clone(),
                recording_dir,
                scenario_root: scenario_entry.clone(),
            };
            derive_scenario(&dir)?;
            done.push(dir);
        }
    }
    Ok(done)
}

/// Find the authoritative `recordings/<client>/` under a scenario, if any: the
/// first whose `meta.json` declares `role: "authoritative"`. Clients are scanned
/// in sorted order for determinism.
fn authoritative_recording(recordings: &Path) -> Result<Option<PathBuf>, DeriveError> {
    if !recordings.is_dir() {
        return Ok(None);
    }
    for client_dir in read_dir_sorted(recordings)? {
        if !client_dir.is_dir() {
            continue;
        }
        let meta_path = client_dir.join("meta.json");
        let Ok(meta_str) = fs::read_to_string(&meta_path) else {
            continue;
        };
        let meta: Value = serde_json::from_str(&meta_str)
            .map_err(|e| DeriveError::Shape(meta_path.clone(), format!("invalid JSON: {e}")))?;
        if str_field(&meta, "role").as_deref() == Some("authoritative") {
            return Ok(Some(client_dir));
        }
    }
    Ok(None)
}

fn read(path: &Path) -> Result<String, DeriveError> {
    fs::read_to_string(path).map_err(|e| DeriveError::Io(path.to_owned(), e))
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, DeriveError> {
    fs::read(path).map_err(|e| DeriveError::Io(path.to_owned(), e))
}

fn write(path: &Path, content: &str) -> Result<(), DeriveError> {
    fs::write(path, content).map_err(|e| DeriveError::Io(path.to_owned(), e))
}

/// Directory entries, sorted by path for deterministic traversal.
fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, DeriveError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| DeriveError::Io(dir.to_owned(), e))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    Ok(entries)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jig_core::conform::MASK;

    // A minimal real-shaped chat-completions capture (chunk frames omitted; the
    // SSE parser handles either).
    const SSE: &str = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"created\":1780783218,\"model\":\"deepseek-v4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}],\"usage\":null}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"created\":1780783218,\"model\":\"deepseek-v4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}],\"usage\":null}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"created\":1780783218,\"model\":\"deepseek-v4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":1,\"total_tokens\":10}}\n\n",
        "data: [DONE]\n\n",
    );

    const REQUEST_JSON: &str = r#"{
        "method": "POST",
        "path": "/chat/completions",
        "headers": [
            { "name": "Content-Type", "value": "application/json" },
            { "name": "Authorization", "value": "REDACTED" }
        ],
        "body": "{\"model\":\"deepseek-chat\",\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
    }"#;

    const RESPONSE_HEADERS: &str = r#"{
        "status": 200,
        "headers": [
            { "name": "Content-Type", "value": "text/event-stream; charset=utf-8" },
            { "name": "Date", "value": "Sat, 06 Jun 2026 22:00:18 GMT" },
            { "name": "Transfer-Encoding", "value": "chunked" }
        ]
    }"#;

    fn artifacts() -> Artifacts {
        derive_artifacts(
            REQUEST_JSON,
            RESPONSE_HEADERS,
            SSE.as_bytes(),
            Path::new("request.json"),
            Path::new("response.headers"),
            Path::new("response.sse"),
        )
        .unwrap()
    }

    #[test]
    fn derives_masked_response_template() {
        let a = artifacts();
        // Framing invariant kept; volatile body masked; terminator asserted.
        assert_eq!(a.response_template.terminator, "[DONE]");
        assert_eq!(a.response_template.reply["turns"][0]["Text"], MASK);
        assert_eq!(a.response_template.reply["usage"]["prompt_tokens"], MASK);
        assert_eq!(a.response_template.reply["stop"], "Stop");
        // content-type kept, date masked, transfer-encoding dropped.
        let names: Vec<&str> = a
            .response_template
            .headers
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert!(names.contains(&"content-type"));
        assert!(names.contains(&"date"));
        assert!(!names.contains(&"transfer-encoding"));
    }

    #[test]
    fn drive_shape_keeps_real_content() {
        let a = artifacts();
        // The drive shape is the un-masked canonical reply jig is driven with.
        let json = serde_json::to_value(&a.drive_shape).unwrap();
        assert_eq!(json["reply"]["turns"][0]["Text"], "hello");
    }

    #[test]
    fn request_template_keeps_model_and_path() {
        let a = artifacts();
        assert_eq!(a.request_template.method, "POST");
        assert_eq!(a.request_template.path, "/chat/completions");
        assert_eq!(a.request_template.body["model"], "deepseek-chat");
    }

    #[test]
    fn derivation_is_deterministic() {
        let (r1, q1, d1) = serialize_artifacts(&artifacts());
        let (r2, q2, d2) = serialize_artifacts(&artifacts());
        assert_eq!((r1, q1, d1), (r2, q2, d2));
    }

    #[test]
    fn malformed_request_json_is_an_error() {
        let err = derive_artifacts(
            "not json",
            RESPONSE_HEADERS,
            SSE.as_bytes(),
            Path::new("request.json"),
            Path::new("response.headers"),
            Path::new("response.sse"),
        );
        assert!(err.is_err());
    }

    #[test]
    fn derive_scenario_writes_three_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = tmp.path().join("openai/single-text/recordings/openai-sdk");
        fs::create_dir_all(&rec).unwrap();
        fs::write(rec.join("request.json"), REQUEST_JSON).unwrap();
        fs::write(rec.join("response.headers"), RESPONSE_HEADERS).unwrap();
        fs::write(rec.join("response.sse"), SSE).unwrap();
        fs::write(
            rec.join("meta.json"),
            r#"{"role":"authoritative","client":"openai-sdk"}"#,
        )
        .unwrap();

        let scenario_root = tmp.path().join("openai/single-text");
        let dir = ScenarioDir {
            dialect: "openai".to_string(),
            scenario: "single-text".to_string(),
            recording_dir: rec,
            scenario_root: scenario_root.clone(),
        };
        let written = derive_scenario(&dir).unwrap();
        assert_eq!(written.len(), 3);
        assert!(scenario_root.join("response.template.json").exists());
        assert!(scenario_root.join("request.template.json").exists());
        assert!(scenario_root.join("drive-shape.json").exists());
    }

    #[test]
    fn derive_tree_skips_scenarios_without_an_authoritative_recording() {
        let tmp = tempfile::tempdir().unwrap();
        // A subject-only scenario: no authoritative recording → skipped.
        let rec = tmp.path().join("openai/subject-only/recordings/pi-sdk");
        fs::create_dir_all(&rec).unwrap();
        fs::write(rec.join("meta.json"), r#"{"role":"subject"}"#).unwrap();

        let done = derive_tree(tmp.path()).unwrap();
        assert!(done.is_empty());
    }
}
