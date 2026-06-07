//! Offline pi-SDK **subject** conformance — T3 (request validation) and T4
//! (cross-driver response consistency) for P6 (#17).
//!
//! Data-driven over every `fixtures/<dialect>/<scenario>/recordings/pi-sdk/`
//! recording captured from the **real** backends by the (manual, online) harness
//! in `crates/jig-oracle/tests/pi_subject_record.rs`. These tests are
//! **offline** — no network, no credentials — and run in the default
//! `cargo test`: they read committed bytes and compare structures against the
//! **authoritative** templates the official-client recordings produced (P2/P3/P4,
//! #14/#15/#16).
//!
//! - **T3 — request validation.** Reduce the subject `request.json` body to its
//!   request *grammar* and assert it is **conformant** with the authoritative
//!   `request.template.json` grammar: every JSON key / value-type / array-element
//!   shape the pi SDK sends must appear in the official client's request. The two
//!   requests are not equal — the official client sends its whole prompt and tool
//!   catalogue — so T3 compares the **wire grammar**, not content or size. A
//!   divergence is a reviewed SDK finding (issue #17: "divergence = candidate SDK
//!   bug"); the committed subject set is expected to be clean, so a finding fails
//!   the test with the offending JSON path.
//! - **T4 — cross-driver response consistency (best-effort).** Parse the subject
//!   `response.sse` under the dialect and mask it the way a response template is
//!   derived, then assert its canonical `reply` grammar matches the authoritative
//!   `response.template.json`'s `reply`. Both drivers, fed the same scenario,
//!   should yield the same masked reply skeleton (e.g. single-text → one masked
//!   `Text` turn, `stop: Stop`). Subject scenarios with no authoritative
//!   counterpart, or whose reply shape legitimately differs (the model is free to
//!   answer differently), are skipped rather than forced.
//!
//! On top of the data-driven T3/T4 checks, [`subject_matrix_is_complete`] is a
//! **full-matrix guard**: it asserts every required `(dialect, scenario)` subject
//! cell (the three tool-shape scenarios across all three dialects) is captured, or
//! is an explicitly reviewed-unavailable cell. This is the issue #17 requirement
//! that a missing subject recording *fails* the build instead of being silently
//! tolerated by the older "at least one exists" check.
//!
//! A failure prints the readable JSON path that diverged, never two large blobs.

use std::path::{Path, PathBuf};

use jig_core::Dialect;
use jig_core::conform::{
    ResponseTemplate, derive_response_template, grammar_findings, request_grammar, structural_diff,
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

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// One pi-SDK subject recording to check: which dialect/scenario it is and where
/// its files and the scenario's authoritative templates live.
struct SubjectCase {
    dialect: Dialect,
    label: String,
    scenario_root: PathBuf,
    subject_dir: PathBuf,
}

/// Every committed `recordings/pi-sdk/` recording across all dialects, sorted.
/// Driving the tests off the committed tree means a new subject recording needs
/// no test edit. Empty is allowed only before the first capture lands — guarded
/// by [`subject_cases_exist`] so a regression that drops them all is caught.
fn subject_cases() -> Vec<SubjectCase> {
    let mut cases = Vec::new();
    let root = fixtures_root();
    let mut dialect_dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("fixtures/ exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dialect_dirs.sort();

    for dialect_dir in dialect_dirs {
        let Some(dialect) = Dialect::for_path(dialect_route(&dialect_dir)) else {
            continue;
        };
        let mut scenario_dirs: Vec<PathBuf> = std::fs::read_dir(&dialect_dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dialect_dir.display()))
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        scenario_dirs.sort();

        for scenario_root in scenario_dirs {
            let subject_dir = scenario_root.join("recordings/pi-sdk");
            if !subject_dir.join("request.json").exists() {
                continue;
            }
            let scenario = scenario_root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            cases.push(SubjectCase {
                dialect,
                label: format!("{}/{scenario}", dialect_slug(&dialect_dir)),
                scenario_root,
                subject_dir,
            });
        }
    }
    cases
}

/// The **expected** pi-SDK subject matrix: every `(dialect, scenario)` cell for
/// which a `recordings/pi-sdk/` recording is *required* to exist.
///
/// Issue #17 asks T3/T4 to "fail when a required subject recording cell is
/// absent" so the missing Anthropic subject set can no longer be overlooked by
/// the [`subject_cases_exist`] some-exist check. This constant is that required
/// list. It is the **three tool-shape scenarios** (`single-text`, `tool-call`,
/// `tool-result-final`) across **all three dialects** — exactly the scenarios the
/// online subject harness (`crates/jig-oracle/tests/support/subject.rs`) can
/// drive deterministically.
///
/// `thinking-text` is deliberately **not** here even though the `anthropic` and
/// `codex` dialects have authoritative `thinking-text` templates: the subject
/// driver omits it because jig steers the model only via the prompt and forcing a
/// reasoning turn out of the SDK is not reliable (see the `Scenario` doc comment
/// in `subject.rs`). Adding a subject `thinking-text` capture later is a one-line
/// addition here.
const EXPECTED_SUBJECT_MATRIX: &[(&str, &str)] = &[
    ("openai", "single-text"),
    ("openai", "tool-call"),
    ("openai", "tool-result-final"),
    ("anthropic", "single-text"),
    ("anthropic", "tool-call"),
    ("anthropic", "tool-result-final"),
    ("codex", "single-text"),
    ("codex", "tool-call"),
    ("codex", "tool-result-final"),
];

/// **Reviewed missing subject cells**: required cells from
/// [`EXPECTED_SUBJECT_MATRIX`] that are *currently uncaptured* for a reviewed,
/// external reason, plus that reason. This is the "explicit reviewed skip list
/// for cells that are unavailable" issue #17 calls for: the matrix-completeness
/// guard treats these as a known, accepted gap rather than a hard failure, while
/// every *other* missing cell fails the build.
///
/// The list is self-cleaning: [`subject_matrix_is_complete`] fails if an entry
/// here is actually *present* on disk (a stale skip), and fails if an entry names
/// a cell not in [`EXPECTED_SUBJECT_MATRIX`] (a typo or drift). So the moment the
/// real Anthropic captures land, this list must be emptied or the build breaks —
/// the gap cannot rot silently.
///
/// Each entry is `((dialect, scenario), why-unavailable)`.
const REVIEWED_MISSING_SUBJECTS: &[((&str, &str), &str)] = &[
    // The Anthropic subject cells require a live Claude **subscription OAuth**
    // token. At capture time the stored access token had expired and the
    // `console.anthropic.com/v1/oauth/token` refresh endpoint was returning
    // `429 rate_limit_error` for every refresh attempt, so no fresh bearer could
    // be obtained to drive the real `/v1/messages` backend. This is the exact
    // blocker issue #17 anticipated ("the stored Anthropic OAuth access token was
    // expired and the token-refresh endpoint was rate-limiting refresh
    // attempts"). Re-record with
    //   JIG_DIALECT=anthropic JIG_SCENARIO=<scenario> \
    //     cargo test -p jig-oracle --test pi_subject_record \
    //     record_one_subject_fixture -- --ignored --nocapture --exact
    // once the refresh rate limit clears, then delete these three entries.
    (
        ("anthropic", "single-text"),
        "Anthropic subscription OAuth refresh rate-limited (HTTP 429) and stored token expired; cannot reach the real backend to capture",
    ),
    (
        ("anthropic", "tool-call"),
        "Anthropic subscription OAuth refresh rate-limited (HTTP 429) and stored token expired; cannot reach the real backend to capture",
    ),
    (
        ("anthropic", "tool-result-final"),
        "Anthropic subscription OAuth refresh rate-limited (HTTP 429) and stored token expired; cannot reach the real backend to capture",
    ),
];

/// Whether a pi-SDK subject recording exists on disk for `(dialect, scenario)`:
/// the presence of its `request.json` is the same liveness signal
/// [`subject_cases`] uses to admit a case.
fn subject_recording_exists(dialect: &str, scenario: &str) -> bool {
    fixtures_root()
        .join(dialect)
        .join(scenario)
        .join("recordings/pi-sdk/request.json")
        .exists()
}

/// The fixture-tree dialect-dir name (`openai`/`anthropic`/`codex`).
fn dialect_slug(dialect_dir: &Path) -> String {
    dialect_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Map a top-level fixtures dialect directory to the route its dialect parses on,
/// so `Dialect::for_path` can resolve it (the tree slug and the route differ).
fn dialect_route(dialect_dir: &Path) -> &'static str {
    match dialect_slug(dialect_dir).as_str() {
        "openai" => "/chat/completions",
        "anthropic" => "/v1/messages",
        "codex" => "/backend-api/codex/responses",
        _ => "/unknown",
    }
}

/// The captured request body of a recording's `request.json`, parsed from the
/// `body` string field (a `CapturedRequest`).
fn request_body(dir: &Path) -> Value {
    let request = read_json(&dir.join("request.json"));
    let body = request["body"].as_str().expect("request body string");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{}: body not JSON: {e}", dir.display()))
}

#[test]
fn subject_cases_exist() {
    // The committed pi-SDK subject recordings are the input to T3/T4. If they all
    // vanish (a bad merge, a botched refresh), the data-driven tests would
    // silently pass with zero cases — so assert at least one exists.
    let cases = subject_cases();
    assert!(
        !cases.is_empty(),
        "no fixtures/*/*/recordings/pi-sdk recordings found; record them with \
         `cargo test -p jig-oracle --test pi_subject_record -- --ignored`"
    );
}

/// **Full-matrix guard (issue #17).** Every required subject cell in
/// [`EXPECTED_SUBJECT_MATRIX`] must either be present on disk or be an explicitly
/// reviewed-unavailable cell in [`REVIEWED_MISSING_SUBJECTS`]. This is the test
/// that "fails when an expected subject recording cell is absent" — closing the
/// gap where the missing Anthropic subject set was silently tolerated because the
/// data-driven T3/T4 tests only ran over whatever happened to be committed.
///
/// Three failure modes, each with an actionable message:
///   1. a required cell is **missing** and **not reviewed** → capture it (or, if
///      genuinely unavailable, add a reviewed entry with the reason);
///   2. a reviewed-missing cell is **actually present** → delete the now-stale
///      skip entry so the cell rejoins the real T3/T4 gate; and
///   3. a reviewed-missing entry names a cell **outside** the expected matrix →
///      a typo/drift; fix the entry.
#[test]
fn subject_matrix_is_complete() {
    let is_reviewed_missing =
        |cell: (&str, &str)| REVIEWED_MISSING_SUBJECTS.iter().any(|(c, _)| *c == cell);

    // (3) The reviewed-missing list must only name cells that are actually
    // required; otherwise the skip list silently drifts from the matrix.
    for ((dialect, scenario), _why) in REVIEWED_MISSING_SUBJECTS {
        assert!(
            EXPECTED_SUBJECT_MATRIX.contains(&(*dialect, *scenario)),
            "REVIEWED_MISSING_SUBJECTS names {dialect}/{scenario}, which is not in \
             EXPECTED_SUBJECT_MATRIX — remove the stale entry or add the cell to the matrix"
        );
    }

    // (2) A reviewed-missing cell that is now present is a stale skip: drop it so
    // the cell is held to the real T3/T4 conformance gate again.
    let stale: Vec<String> = REVIEWED_MISSING_SUBJECTS
        .iter()
        .filter(|((dialect, scenario), _)| subject_recording_exists(dialect, scenario))
        .map(|((dialect, scenario), _)| format!("{dialect}/{scenario}"))
        .collect();
    assert!(
        stale.is_empty(),
        "these cells are now captured but still listed in REVIEWED_MISSING_SUBJECTS — \
         delete the stale skip entries so they rejoin the T3/T4 gate:\n  {}",
        stale.join("\n  ")
    );

    // (1) The core guard: every required cell is present, or reviewed-unavailable.
    let missing: Vec<String> = EXPECTED_SUBJECT_MATRIX
        .iter()
        .filter(|(dialect, scenario)| {
            !subject_recording_exists(dialect, scenario)
                && !is_reviewed_missing((dialect, scenario))
        })
        .map(|(dialect, scenario)| format!("{dialect}/{scenario}"))
        .collect();
    assert!(
        missing.is_empty(),
        "required pi-SDK subject recordings are missing and not reviewed (issue #17 full-matrix \
         guard). Capture each with `JIG_DIALECT=<d> JIG_SCENARIO=<s> cargo test -p jig-oracle \
         --test pi_subject_record record_one_subject_fixture -- --ignored --exact`, or add a \
         reviewed entry to REVIEWED_MISSING_SUBJECTS if the cell is genuinely unavailable:\n  {}",
        missing.join("\n  ")
    );

    // Surface the accepted gaps in test output so they stay visible rather than
    // silently skipped.
    let reviewed_gaps: Vec<String> = REVIEWED_MISSING_SUBJECTS
        .iter()
        .map(|((dialect, scenario), why)| format!("{dialect}/{scenario}: {why}"))
        .collect();
    if !reviewed_gaps.is_empty() {
        eprintln!("subject matrix reviewed-unavailable cells (issue #17):");
        for gap in &reviewed_gaps {
            eprintln!("  - {gap}");
        }
    }
}

/// **Reviewed T3 findings**: cross-driver request-grammar divergences that a
/// human has inspected and judged **benign** — a spec-valid field the pi SDK
/// sends that the *one* official-client capture happened not to, **not** an SDK
/// bug. Each entry is `(label-suffix-or-"*", json-path, why-benign)`. Keeping
/// this as a small, commented allowlist is exactly the "explicit, reviewed list
/// of SDK findings" issue #17 asks for: T3 stays a real gate (an *unreviewed*
/// divergence fails the build) while not flagging known-good optional fields.
///
/// A label suffix of `"*"` applies to every dialect/scenario; otherwise it must
/// equal the case label (`"openai/single-text"`).
const REVIEWED_T3_FINDINGS: &[(&str, &str, &str)] = &[
    // The pi SDK always sets an explicit `max_tokens` (here 4096) on the
    // chat-completions request; it is a valid, optional OpenAI field that the
    // recorded DeepSeek-SDK sample simply omitted. Spec-compliant, not a bug.
    (
        "*",
        "max_tokens",
        "valid optional OpenAI/chat-completions field the SDK always sets; the authoritative sample omitted it",
    ),
    // On an assistant *tool-call* message the pi SDK serializes `content` as an
    // empty string (`""`), whereas the official OpenAI/DeepSeek client serializes
    // it as JSON `null`. Both are accepted by the backend (the subject capture is
    // HTTP 200) and both mean "no text alongside the tool call". A cosmetic wire
    // divergence in the assistant-tool-call encoding — reviewed, benign. The path
    // is the grammar-collapsed distinct-element index, not the literal position.
    (
        "openai/tool-result-final",
        "messages[1].content",
        "assistant tool-call `content` is \"\" (SDK) vs null (official); both accepted, semantically identical",
    ),
    // On the Codex responses request the pi SDK sets `reasoning.summary` (e.g.
    // "auto") in addition to `reasoning.effort`; the recorded `codex exec` sample
    // sent only `effort`. `summary` is a documented, optional Codex responses
    // field (it controls whether a reasoning summary is streamed), accepted by
    // the backend (HTTP 200). Reviewed, benign — a config difference, not a bug.
    (
        "codex/single-text",
        "reasoning.summary",
        "optional Codex responses `reasoning.summary` field the SDK sets; the authoritative sample omitted it",
    ),
    (
        "codex/tool-call",
        "reasoning.summary",
        "optional Codex responses `reasoning.summary` field the SDK sets; the authoritative sample omitted it",
    ),
    (
        "codex/tool-result-final",
        "reasoning.summary",
        "optional Codex responses `reasoning.summary` field the SDK sets; the authoritative sample omitted it",
    ),
];

/// Whether a finding at `path` for `label` is a reviewed-benign divergence.
fn is_reviewed(label: &str, path: &str) -> bool {
    REVIEWED_T3_FINDINGS
        .iter()
        .any(|(scope, p, _)| (*scope == "*" || *scope == label) && *p == path)
}

#[test]
fn t3_subject_request_grammar_conforms_to_authoritative() {
    let mut reviewed_seen = Vec::new();
    for case in subject_cases() {
        let template_path = case.scenario_root.join("request.template.json");
        if !template_path.exists() {
            // No authoritative request template for this scenario → nothing to
            // validate the subject grammar against. (Should not happen for the
            // shared scenarios, but skip rather than panic.)
            continue;
        }

        let subject = request_body(&case.subject_dir);
        let authoritative = read_json(&template_path)["body"].clone();

        // Partition findings into reviewed-benign (allowlisted) and unexpected.
        let (reviewed, unexpected): (Vec<_>, Vec<_>) = grammar_findings(&subject, &authoritative)
            .into_iter()
            .partition(|f| is_reviewed(&case.label, &f.path));
        for f in &reviewed {
            reviewed_seen.push(format!("{}: {}", case.label, f.path));
        }

        assert!(
            unexpected.is_empty(),
            "T3 {}: pi-SDK request grammar diverges from the authoritative contract \
             (unreviewed — a candidate SDK bug; add to REVIEWED_T3_FINDINGS only after review):\n  {}",
            case.label,
            unexpected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }
    // Surface the reviewed findings so they stay visible in test output rather
    // than silently suppressed.
    if !reviewed_seen.is_empty() {
        eprintln!("T3 reviewed (benign) findings:");
        for f in &reviewed_seen {
            eprintln!("  - {f}");
        }
    }
}

#[test]
fn t4_subject_response_reply_matches_authoritative() {
    for case in subject_cases() {
        let template_path = case.scenario_root.join("response.template.json");
        if !template_path.exists() {
            continue;
        }

        // Skip a subject response that did not complete (a finding captured by the
        // harness as a non-2xx), and the multi-turn final scenario where the
        // model's free-form reply (text vs. another tool call) is not a contract.
        let status = read_json(&case.subject_dir.join("response.headers"))["status"]
            .as_u64()
            .unwrap_or(0);
        if !(200..300).contains(&status) {
            continue;
        }

        let sse = std::fs::read(case.subject_dir.join("response.sse")).unwrap();
        let subject_template = match derive_response_template(case.dialect, &sse, &[]) {
            Ok(t) => t,
            // A subject stream that does not parse is a finding surfaced elsewhere
            // (the raw bytes are committed); T4 is best-effort, so skip it here.
            Err(_) => continue,
        };

        let authoritative: ResponseTemplate = serde_json::from_value(read_json(&template_path))
            .expect("authoritative template shape");

        // Compare the canonical reply *grammar* (turn kinds + masked content +
        // stop), not the headers (different backends) — cross-driver consistency.
        let subj_reply = request_grammar(&subject_template.reply);
        let auth_reply = request_grammar(&authoritative.reply);
        let diff = structural_diff(&auth_reply, &subj_reply);

        // Best-effort: a reply-shape difference is reported but only fails for the
        // single-text scenario, where both drivers must yield one masked Text turn.
        let scenario_is_single_text = case.label.ends_with("/single-text");
        if scenario_is_single_text {
            assert!(
                diff.is_empty(),
                "T4 {}: subject reply grammar differs from authoritative:\n  {}",
                case.label,
                diff.join("\n  ")
            );
        } else if !diff.is_empty() {
            // Non-fatal cross-driver note for the tool scenarios (the model is free
            // to answer differently); printed for the operator, not a failure.
            eprintln!(
                "T4 {} (best-effort, non-fatal): reply grammar differs:\n  {}",
                case.label,
                diff.join("\n  ")
            );
        }
    }
}

/// Backstop the redaction invariant from the test side: no bearer/secret-shaped
/// string under any committed pi-SDK subject recording.
#[test]
fn no_secret_material_under_subject_recordings() {
    for case in subject_cases() {
        for name in [
            "request.json",
            "response.headers",
            "meta.json",
            "response.sse",
        ] {
            let path = case.subject_dir.join(name);
            if !path.exists() {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("Bearer sk-")
                    && !text.contains("sk-ant-oat")
                    && !text.contains("sk-live"),
                "possible secret in {}",
                path.display()
            );
            for line in text.lines() {
                if line.to_ascii_lowercase().contains("\"authorization\"") {
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
