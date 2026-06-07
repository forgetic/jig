//! Online pi-SDK **recording** harness — captures real `subject` fixtures
//! (issue #17, P6). **Not part of `cargo test`**: every test here is `#[ignore]`d
//! because it drives `pi_agent_rust` against the *real* backends with the
//! credentials in `~/.pi/agent/auth.json`. Run it deliberately:
//!
//! ```sh
//! cargo test -p jig-oracle --test pi_subject_record -- --ignored --nocapture
//! ```
//!
//! For each `(dialect, scenario)` it:
//!   1. binds the passthrough recorder on loopback;
//!   2. resolves the real bearer for the dialect (DeepSeek key / Codex JWT /
//!      refreshed Anthropic OAuth) and builds a pi-SDK provider pointed at the
//!      recorder's `base_url`;
//!   3. drives one completion through the recorder to the real backend on the
//!      SDK's own `asupersync` runtime, while the recorder (on a background tokio
//!      runtime) captures the one routable exchange; and
//!   4. redacts and writes it as a `role: subject` recording under
//!      `fixtures/<dialect>/<scenario>/recordings/pi-sdk/`.
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
use std::sync::mpsc;

use futures::StreamExt;
use jig_record::proxy::{bind, proxy_once};
use jig_record::{Provenance, Role, build_recording};
use pi::provider::Context;
use pi::providers::create_provider;

use support::auth::{default_auth_path, resolve_bearer};
use support::subject::{Dialect, Scenario, context_for, model_entry, stream_options};

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

/// Record one `(dialect, scenario)` cell: bind the recorder, drive the SDK
/// through it, and write the redacted `subject` recording. Returns the captured
/// HTTP status so the caller can flag a non-200 (a finding, not a fixture).
fn record_cell(dialect: Dialect, scenario: Scenario) -> std::io::Result<u16> {
    let auth_file = default_auth_path();

    // Resolve the real bearer FIRST, before standing up the recorder. The
    // anthropic path may refresh a near/expired OAuth token over the network; if
    // that failed *after* the recorder was waiting on a connection, `proxy_once`
    // (which blocks until a routable request arrives) would deadlock and never be
    // joined. Resolving up front turns a credential failure into an immediate,
    // clean error. Each resolution gets its own short-lived asupersync runtime.
    let api_key = {
        let rt = asupersync::runtime::RuntimeBuilder::current_thread()
            .build()
            .expect("build bearer-resolution runtime");
        rt.block_on(async { resolve_bearer(dialect, &auth_file).await })
            .map_err(|e| std::io::Error::other(format!("resolve bearer: {e}")))?
    };

    // The recorder is tokio-based; the SDK is asupersync-based. Run the recorder
    // on its own current-thread tokio runtime in a background OS thread, hand the
    // bound base_url back over a channel, drive the SDK on the asupersync runtime
    // in this thread, then join the recorder thread for the captured exchange.
    let (url_tx, url_rx) = mpsc::channel::<String>();
    let dialect_for_thread = dialect;
    let recorder = std::thread::spawn(move || -> std::io::Result<_> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            let listener = bind().await?;
            let addr = listener.local_addr()?;
            url_tx
                .send(format!("http://{addr}"))
                .expect("send base_url");
            // OpenAI dialect is recorded against DeepSeek (an OpenAI-compatible
            // backend); the others use the dialect default upstream.
            let upstream = match dialect_for_thread {
                Dialect::OpenAi => Some("api.deepseek.com"),
                _ => None,
            };
            proxy_once(&listener, upstream).await
        })
    });

    let base_url = url_rx.recv().expect("recorder bound");

    // Drive the SDK on its own runtime, pointed at the recorder.
    let sdk_rt = asupersync::runtime::RuntimeBuilder::current_thread()
        .build()
        .expect("build pi-SDK runtime");
    let drive_result = sdk_rt.block_on(async {
        let entry = model_entry(
            dialect,
            &base_url,
            Some(api_key.clone()),
            std::collections::HashMap::new(),
        );
        let provider =
            create_provider(&entry, None).map_err(|e| std::io::Error::other(format!("{e:?}")))?;

        let ctx = context_for(dialect, scenario);
        let options = stream_options(dialect, &api_key);
        let ctx_ref: &Context<'_> = &ctx;

        // Drain the stream so the full request/response round-trips through the
        // recorder. Stream errors (e.g. a 4xx from the backend) are tolerated:
        // the recorder still captured the exchange, which is the finding.
        match provider.stream(ctx_ref, &options).await {
            Ok(mut stream) => {
                while let Some(event) = stream.next().await {
                    if let Err(e) = event {
                        eprintln!("  [stream event error, capture still written] {e}");
                        break;
                    }
                }
            }
            Err(e) => eprintln!("  [stream start error, capture still written] {e:?}"),
        }
        Ok::<(), std::io::Error>(())
    });
    if let Err(e) = drive_result {
        eprintln!("  [drive error] {e}");
    }

    // Collect the captured exchange and write it.
    let (request, response, route) = recorder
        .join()
        .expect("recorder thread panicked")
        .map_err(|e| std::io::Error::other(format!("recorder: {e}")))?;

    let status = response.status;
    let provenance = Provenance {
        client: "pi-sdk".to_string(),
        role: Role::Subject,
        scenario: scenario.slug().to_string(),
        // The SDK version under test (the `pi` dep is pinned to =0.1.13 in
        // Cargo.toml); stamped so a refresh records which SDK produced the shape.
        client_version: Some("pi_agent_rust 0.1.13".to_string()),
        captured: today_utc(),
        recorder_sha: recorder_sha(),
    };
    let recording = build_recording(&request, &response, &route, &provenance);
    let dir = recording.write(&fixtures_root())?;
    eprintln!(
        "  wrote {}/{} subject recording -> {} (HTTP {status})",
        dialect.slug(),
        scenario.slug(),
        dir.display()
    );
    Ok(status)
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
    let dialect = match std::env::var("JIG_DIALECT").as_deref() {
        Ok("openai") => Dialect::OpenAi,
        Ok("anthropic") => Dialect::Anthropic,
        Ok("codex") => Dialect::Codex,
        other => panic!("set JIG_DIALECT to openai|anthropic|codex (got {other:?})"),
    };
    let scenario = match std::env::var("JIG_SCENARIO").as_deref() {
        Ok("single-text") => Scenario::SingleText,
        Ok("tool-call") => Scenario::ToolCall,
        Ok("tool-result-final") => Scenario::ToolResultFinal,
        other => {
            panic!("set JIG_SCENARIO to single-text|tool-call|tool-result-final (got {other:?})")
        }
    };
    let status = record_cell(dialect, scenario).expect("record cell");
    assert!(
        (200..300).contains(&status),
        "non-2xx status {status} is a finding, not a fixture"
    );
}
