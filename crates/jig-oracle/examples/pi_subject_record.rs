//! xtask-callable pi-SDK **subject** recording harness (issue #19).
//!
//! NOT part of `cargo test` — this is the online, manual leg. It is the
//! counterpart of the `jig-record` `capture`/`codex_capture` examples for the
//! `subject` driver: a single `(dialect, scenario)` cell recorded against the
//! real backend using the credentials in `~/.pi/agent/auth.json`.
//!
//! Before #19 the only operator entry point for subject recordings was
//! `cargo test -p jig-oracle --test pi_subject_record -- --ignored`. That made
//! it impossible for `xtask record` to drive the subject cells as part of the
//! one-shot refresh. This example exposes the *same* capture logic
//! ([`support::subject::record_subject_cell`], which the ignored test also calls)
//! through a normal, xtask-spawnable command:
//!
//!   cargo run -p jig-oracle --example pi_subject_record -- \
//!     --dialect anthropic --scenario tool-call \
//!     --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)"
//!
//! A non-2xx capture is a **finding**, not a fixture: it is still written (with
//! its real status) but the harness exits non-zero so the orchestrator records
//! the failure and `xtask derive` never anchors a template to it.

// Reuse the test-only support modules (the dialect/scenario driving core, the
// real-credential resolution, and the Anthropic subscription workaround) by
// path, so the example and the `#[ignore]`d test share one implementation.
#[path = "../tests/support/mod.rs"]
mod support;

use std::path::PathBuf;

use support::auth::default_auth_path;
use support::subject::{Dialect, Scenario, record_subject_cell};

#[derive(Debug)]
struct Args {
    dialect: Dialect,
    scenario: Scenario,
    fixtures_root: PathBuf,
    captured: String,
    recorder_sha: String,
}

fn parse_args() -> Args {
    let mut dialect = None;
    let mut scenario = None;
    let mut fixtures_root = None;
    let mut captured = None;
    let mut recorder_sha = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dialect" => {
                let slug = it.next().expect("--dialect value");
                dialect = Some(Dialect::parse(&slug).unwrap_or_else(|| {
                    panic!("unknown --dialect {slug:?}; want openai|anthropic|codex")
                }));
            }
            "--scenario" => {
                let slug = it.next().expect("--scenario value");
                scenario = Some(Scenario::parse(&slug).unwrap_or_else(|| {
                    panic!(
                        "unknown --scenario {slug:?}; \
                         want single-text|tool-call|tool-result-final|parallel-tool-calls"
                    )
                }));
            }
            "--fixtures-root" => fixtures_root = it.next().map(PathBuf::from),
            "--captured" => captured = it.next(),
            "--recorder-sha" => recorder_sha = it.next(),
            other => panic!("unknown flag {other:?}"),
        }
    }

    Args {
        dialect: dialect.expect("--dialect required"),
        scenario: scenario.expect("--scenario required"),
        // Default to the workspace fixtures root so a bare invocation Just Works
        // from the repo root, matching the other harnesses.
        fixtures_root: fixtures_root.unwrap_or_else(default_fixtures_root),
        captured: captured.expect("--captured required"),
        recorder_sha: recorder_sha.expect("--recorder-sha required"),
    }
}

/// The workspace `fixtures/` root, resolved from this crate's manifest dir, so
/// the default works regardless of the process's current directory.
fn default_fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn main() {
    let args = parse_args();

    eprintln!(
        "recording pi-SDK subject {}/{} ...",
        args.dialect.slug(),
        args.scenario.slug()
    );
    let status = match record_subject_cell(
        args.dialect,
        args.scenario,
        &args.fixtures_root,
        &args.captured,
        &args.recorder_sha,
        &default_auth_path(),
    ) {
        Ok(status) => status,
        Err(e) => {
            eprintln!("ERROR recording subject cell: {e}");
            std::process::exit(1);
        }
    };

    if (200..300).contains(&status) {
        eprintln!("subject recording OK (HTTP {status})");
    } else {
        eprintln!(
            "WARNING: non-2xx status {status} — capture written as a finding, \
             not a fixture to derive from"
        );
        std::process::exit(1);
    }
}
