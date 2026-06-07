//! Online pi-SDK **recording** harness — captures real `subject` fixtures
//! (issue #17, P6). **Not part of `cargo test`**: every test here is `#[ignore]`d
//! because it drives `pi_agent_rust` against the *real* backends with the
//! credentials in `~/.pi/agent/auth.json`. Run it deliberately:
//!
//! ```sh
//! cargo test -p jig-oracle --test pi_subject_record -- --ignored --nocapture
//! ```
//!
//! The actual capture logic lives in [`support::subject::record_subject_cell`]
//! so the one-shot `xtask record` path (via the
//! `examples/pi_subject_record.rs` example, issue #19) and this `--ignored` test
//! run **identical** code: for each `(dialect, scenario)` it binds the
//! passthrough recorder, resolves the real dialect bearer, drives one pi-SDK
//! completion through the recorder to the real backend, and writes the redacted
//! `role: subject` recording under `fixtures/<dialect>/<scenario>/recordings/pi-sdk/`.
//!
//! The recorder redacts every bearer / identity value at capture time, so the
//! committed `subject` recordings are safe. After recording, `xtask derive` is
//! **not** re-run for these (templates are anchored to the *authoritative*
//! client, never the subject); the offline T3/T4 conformance in
//! `crates/jig-core/tests/pi_sdk_conformance.rs` validates them against the
//! authoritative templates.
//!
//! A failed pi-SDK recording (e.g. a 4xx) is a **finding**, not a jig fixture: it
//! is surfaced (the capture is written with its real status and the harness
//! prints a warning) but never derived from.

mod support;

use std::path::PathBuf;

use support::auth::default_auth_path;
use support::subject::{Dialect, Scenario, record_subject_cell};

/// Today's date in `YYYY-MM-DD`, for `meta.captured`. Computed from the system
/// clock — fine here because this harness is manual and online, never in the
/// deterministic offline suite.
fn today_utc() -> String {
    // Days since the unix epoch → civil date (Howard Hinnant's algorithm), so we
    // need no date crate. UTC is sufficient for a capture-date stamp.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Civil (year, month, day) from a count of days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The recorder's git sha, for `meta.recorder_sha`.
fn recorder_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The workspace `fixtures/` root, resolved from this crate's manifest dir.
fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Record one `(dialect, scenario)` cell with provenance stamped from the local
/// clock + git, delegating the capture to the shared core. Returns the captured
/// HTTP status so the caller can flag a non-200 (a finding, not a fixture).
fn record_cell(dialect: Dialect, scenario: Scenario) -> std::io::Result<u16> {
    record_subject_cell(
        dialect,
        scenario,
        &fixtures_root(),
        &today_utc(),
        &recorder_sha(),
        &default_auth_path(),
    )
}

/// Record every `(dialect, scenario)` cell. `#[ignore]`d: online, real creds.
#[test]
#[ignore = "online: drives the real backends with credentials from ~/.pi/agent/auth.json"]
fn record_all_subject_fixtures() {
    let mut findings = Vec::new();
    for dialect in Dialect::all() {
        for scenario in Scenario::all() {
            eprintln!("recording {}/{} ...", dialect.slug(), scenario.slug());
            match record_cell(dialect, scenario) {
                Ok(status) if (200..300).contains(&status) => {}
                Ok(status) => findings.push(format!(
                    "{}/{}: HTTP {status} (finding — not a fixture to derive from)",
                    dialect.slug(),
                    scenario.slug()
                )),
                Err(e) => findings.push(format!(
                    "{}/{}: harness error: {e}",
                    dialect.slug(),
                    scenario.slug()
                )),
            }
        }
    }
    if !findings.is_empty() {
        eprintln!("\n=== pi-SDK recording findings ===");
        for f in &findings {
            eprintln!("  - {f}");
        }
    }
}

/// Record a single cell selected by env vars `JIG_DIALECT` / `JIG_SCENARIO`, for
/// refreshing one fixture without re-recording the whole matrix. `#[ignore]`d.
#[test]
#[ignore = "online: set JIG_DIALECT and JIG_SCENARIO, drives a real backend"]
fn record_one_subject_fixture() {
    let dialect = match std::env::var("JIG_DIALECT").ok().as_deref() {
        Some(slug) => Dialect::parse(slug)
            .unwrap_or_else(|| panic!("set JIG_DIALECT to openai|anthropic|codex (got {slug:?})")),
        None => panic!("set JIG_DIALECT to openai|anthropic|codex"),
    };
    let scenario = match std::env::var("JIG_SCENARIO").ok().as_deref() {
        Some(slug) => Scenario::parse(slug).unwrap_or_else(|| {
            panic!(
                "set JIG_SCENARIO to \
                 single-text|tool-call|tool-result-final|parallel-tool-calls (got {slug:?})"
            )
        }),
        None => panic!(
            "set JIG_SCENARIO to single-text|tool-call|tool-result-final|parallel-tool-calls"
        ),
    };
    let status = record_cell(dialect, scenario).expect("record cell");
    assert!(
        (200..300).contains(&status),
        "non-2xx status {status} is a finding, not a fixture"
    );
}
