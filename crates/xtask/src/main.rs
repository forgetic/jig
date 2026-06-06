//! `xtask` binary: the operator entry point for fixture maintenance.
//!
//! Three subcommands, all thin wiring over the pure logic in the library:
//!
//! - **record** — expand the scenario matrix for the given selection and drive
//!   `jig record` once per (dialect, scenario, client). This leg is **manual and
//!   online**: each invocation proxies a real client ↔ real backend exchange, so
//!   it needs a live API key on the client side and network. `cargo test` never
//!   runs it; the matrix expansion it relies on is unit-tested in the library.
//! - **derive** — reduce committed authoritative recordings to the masked
//!   `*.template.json` + `drive-shape.json` conformance artifacts (P2, #14). This
//!   leg is **offline and deterministic**: re-deriving over the same recordings
//!   produces byte-identical artifacts.
//! - **staleness** — walk `fixtures/` offline and report each recording's capture
//!   age, flagging any past the threshold. Non-fatal by default: a nudge to
//!   re-record, not a build break (pass `--fail-on-stale` to gate a CI job on it).
//!
//! # Usage
//!
//! ```sh
//! cargo run -p xtask -- record --all
//! cargo run -p xtask -- record --dialect openai [--scenario single-text] [--client openai-sdk]
//! cargo run -p xtask -- derive [--fixtures-root DIR]
//! cargo run -p xtask -- staleness [--max-age-days 90] [--fail-on-stale]
//! ```

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use xtask::derive::derive_tree;
use xtask::matrix::{RecordInvocation, Selection, known_dialects, plan};
use xtask::staleness::{DEFAULT_MAX_AGE_DAYS, FixtureAge, evaluate};
use xtask::{Provenance, collect_fixture_metas, recorder_sha, resolve_today};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("xtask: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> io::Result<ExitCode> {
    let mut it = args.into_iter();
    match it.next().as_deref() {
        Some("record") => run_record(it.collect()),
        Some("derive") => run_derive(it.collect()),
        Some("staleness") => run_staleness(it.collect()),
        Some("--help" | "-h") | None => {
            print_usage();
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => {
            eprintln!("xtask: unknown subcommand {other:?}");
            print_usage();
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_usage() {
    eprintln!(
        "xtask — jig developer task runner\n\
         \n\
         Usage:\n\
         \x20 xtask record [--all] [--dialect D] [--scenario S] [--client C] \\\n\
         \x20              [--fixtures-root DIR] [--captured YYYY-MM-DD] \\\n\
         \x20              [--recorder-sha SHA] [--upstream-host HOST] [--dry-run]\n\
         \x20 xtask derive [--fixtures-root DIR]\n\
         \x20 xtask staleness [--fixtures-root DIR] [--max-age-days N] \\\n\
         \x20                 [--today YYYY-MM-DD] [--fail-on-stale]\n\
         \n\
         `record` is manual and online (needs a live API key + network);\n\
         `derive` and `staleness` are offline. See docs/how-to/refresh-fixtures.md."
    );
}

/// `record` options: a [`Selection`] over the matrix plus provenance and where to
/// write. `--all` and a bare (unfiltered) selection mean the same thing; `--all`
/// is accepted for intent. `--dry-run` prints the plan without spawning.
struct RecordArgs {
    selection: Selection,
    fixtures_root: String,
    captured: Option<String>,
    recorder_sha: Option<String>,
    upstream_host: Option<String>,
    all: bool,
    dry_run: bool,
}

fn run_record(args: Vec<String>) -> io::Result<ExitCode> {
    let parsed = parse_record_args(args)?;

    // Guard against a bare `xtask record` silently spawning the entire online
    // matrix: require either an explicit filter or the `--all` opt-in.
    if parsed.selection.is_all() && !parsed.all {
        eprintln!(
            "xtask record: refusing to record the whole matrix without `--all`.\n\
             Narrow it with --dialect/--scenario/--client, or pass --all to confirm."
        );
        return Ok(ExitCode::FAILURE);
    }

    let invocations = plan(&parsed.selection);
    if invocations.is_empty() {
        eprintln!(
            "xtask record: selection matched nothing. Known dialects: {}.",
            known_dialects().join(", ")
        );
        return Ok(ExitCode::FAILURE);
    }

    let provenance = Provenance {
        captured: parsed.captured.clone().unwrap_or_else(xtask::today_utc),
        recorder_sha: parsed.recorder_sha.clone().unwrap_or_else(recorder_sha),
        upstream_host: parsed.upstream_host.clone(),
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "xtask record: {} invocation(s) into {} (captured {}, recorder {})",
        invocations.len(),
        parsed.fixtures_root,
        provenance.captured,
        provenance.recorder_sha
    )?;
    for inv in &invocations {
        writeln!(
            out,
            "  - {}/{} via {} [{}]",
            inv.dialect,
            inv.scenario,
            inv.client,
            inv.role.slug()
        )?;
    }

    if parsed.dry_run {
        writeln!(out, "xtask record: --dry-run, not spawning.")?;
        return Ok(ExitCode::SUCCESS);
    }

    let mut failures = 0usize;
    for inv in &invocations {
        writeln!(
            out,
            "\n=== recording {}/{} via {} ===",
            inv.dialect, inv.scenario, inv.client
        )?;
        out.flush()?;
        match spawn_record(inv, &parsed.fixtures_root, &provenance) {
            Ok(true) => {}
            Ok(false) => {
                failures += 1;
                writeln!(out, "xtask record: {} exited non-zero", inv.client)?;
            }
            Err(err) => {
                failures += 1;
                writeln!(out, "xtask record: failed to spawn jig record: {err}")?;
            }
        }
    }

    if failures > 0 {
        eprintln!("xtask record: {failures} invocation(s) failed");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Spawn one `jig record` via `cargo run --bin jig`, returning whether it
/// succeeded. This is the only online, process-spawning step; isolated here so
/// the rest of the crate stays pure and testable.
fn spawn_record(
    inv: &RecordInvocation,
    fixtures_root: &str,
    provenance: &Provenance,
) -> io::Result<bool> {
    let status = Command::new(cargo())
        .args(["run", "--quiet", "--bin", "jig", "--"])
        .args(inv.argv(fixtures_root, provenance))
        .status()?;
    Ok(status.success())
}

/// The cargo executable to re-invoke, honoring `$CARGO` when xtask is itself run
/// under cargo (the usual case), falling back to `cargo` on `PATH`.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn parse_record_args(args: Vec<String>) -> io::Result<RecordArgs> {
    let mut selection = Selection::default();
    let mut fixtures_root = String::from("fixtures");
    let mut captured = None;
    let mut recorder_sha = None;
    let mut upstream_host = None;
    let mut all = false;
    let mut dry_run = false;

    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value = || next_value(&flag, &mut it);
        match flag.as_str() {
            "--all" => all = true,
            "--dry-run" => dry_run = true,
            "--dialect" => selection.dialect = Some(value()?),
            "--scenario" => selection.scenario = Some(value()?),
            "--client" => selection.client = Some(value()?),
            "--fixtures-root" => fixtures_root = value()?,
            "--captured" => captured = Some(value()?),
            "--recorder-sha" => recorder_sha = Some(value()?),
            "--upstream-host" => upstream_host = Some(value()?),
            other => return Err(unknown_flag(other)),
        }
    }

    Ok(RecordArgs {
        selection,
        fixtures_root,
        captured,
        recorder_sha,
        upstream_host,
        all,
        dry_run,
    })
}

/// `derive` — reduce every committed authoritative recording under the fixtures
/// root to its masked `*.template.json` + `drive-shape.json` artifacts. Offline
/// and deterministic; the only flag is where the fixtures live.
fn run_derive(args: Vec<String>) -> io::Result<ExitCode> {
    let mut fixtures_root = PathBuf::from("fixtures");

    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value = || next_value(&flag, &mut it);
        match flag.as_str() {
            "--fixtures-root" => fixtures_root = PathBuf::from(value()?),
            other => return Err(unknown_flag(other)),
        }
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();

    match derive_tree(&fixtures_root) {
        Ok(done) if done.is_empty() => {
            writeln!(
                out,
                "xtask derive: no authoritative recordings under {} — nothing to derive.",
                fixtures_root.display()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(done) => {
            writeln!(out, "xtask derive: {} scenario(s) derived", done.len())?;
            for dir in &done {
                writeln!(
                    out,
                    "  - {}/{} (from {})",
                    dir.dialect,
                    dir.scenario,
                    relative(&dir.recording_dir, &fixtures_root).display()
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            eprintln!("xtask derive: {err}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Render `path` relative to `base` for a tidy log line, falling back to the full
/// path if it is not under `base`.
fn relative<'a>(path: &'a Path, base: &Path) -> &'a Path {
    path.strip_prefix(base).unwrap_or(path)
}

fn run_staleness(args: Vec<String>) -> io::Result<ExitCode> {
    let mut fixtures_root = PathBuf::from("fixtures");
    let mut max_age_days = DEFAULT_MAX_AGE_DAYS;
    let mut today = None;
    let mut fail_on_stale = false;

    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value = || next_value(&flag, &mut it);
        match flag.as_str() {
            "--fixtures-root" => fixtures_root = PathBuf::from(value()?),
            "--max-age-days" => {
                max_age_days = value()?.parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--max-age-days wants an integer",
                    )
                })?
            }
            "--today" => today = Some(value()?),
            "--fail-on-stale" => fail_on_stale = true,
            other => return Err(unknown_flag(other)),
        }
    }

    let metas = collect_fixture_metas(&fixtures_root)?;
    let today = resolve_today(today.as_deref());
    let ages = evaluate(&metas, today, max_age_days);

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if ages.is_empty() {
        writeln!(
            out,
            "xtask staleness: no fixtures under {} — nothing recorded yet.",
            fixtures_root.display()
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    report_ages(&mut out, &ages, max_age_days)?;

    let stale = ages.iter().filter(|a| a.stale).count();
    if stale > 0 && fail_on_stale {
        eprintln!("xtask staleness: {stale} fixture(s) stale and --fail-on-stale set");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn report_ages(out: &mut impl Write, ages: &[FixtureAge], max_age_days: i64) -> io::Result<()> {
    writeln!(
        out,
        "xtask staleness: {} fixture(s), threshold {} days",
        ages.len(),
        max_age_days
    )?;
    for a in ages {
        let age = match a.age_days {
            Some(d) => format!("{d}d"),
            None => format!("unparseable date {:?}", a.captured),
        };
        let marker = if a.stale { "STALE" } else { "ok" };
        writeln!(
            out,
            "  [{}] {}/{} via {} — captured {} ({})",
            marker, a.dialect, a.scenario, a.client, a.captured, age
        )?;
    }
    let stale = ages.iter().filter(|a| a.stale).count();
    if stale > 0 {
        writeln!(
            out,
            "xtask staleness: {stale} fixture(s) past {max_age_days}d — consider `xtask record` to refresh."
        )?;
    } else {
        writeln!(out, "xtask staleness: all fixtures within threshold.")?;
    }
    Ok(())
}

/// Pull the value following a `--flag`, erroring if it is missing.
fn next_value(flag: &str, it: &mut impl Iterator<Item = String>) -> io::Result<String> {
    it.next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{flag} needs a value")))
}

fn unknown_flag(flag: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("unknown flag {flag:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_record_args_collects_a_full_selection() {
        let args = vec![
            "--dialect",
            "openai",
            "--scenario",
            "single-text",
            "--client",
            "openai-sdk",
            "--fixtures-root",
            "fx",
            "--captured",
            "2026-06-06",
            "--recorder-sha",
            "deadbee",
            "--upstream-host",
            "api.deepseek.com",
            "--dry-run",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let parsed = parse_record_args(args).unwrap();
        assert_eq!(parsed.selection.dialect.as_deref(), Some("openai"));
        assert_eq!(parsed.selection.scenario.as_deref(), Some("single-text"));
        assert_eq!(parsed.selection.client.as_deref(), Some("openai-sdk"));
        assert_eq!(parsed.fixtures_root, "fx");
        assert_eq!(parsed.captured.as_deref(), Some("2026-06-06"));
        assert_eq!(parsed.recorder_sha.as_deref(), Some("deadbee"));
        assert_eq!(parsed.upstream_host.as_deref(), Some("api.deepseek.com"));
        assert!(parsed.dry_run);
        assert!(!parsed.all);
    }

    #[test]
    fn parse_record_args_rejects_unknown_flag() {
        let args = vec!["--nope".to_string()];
        assert!(parse_record_args(args).is_err());
    }

    #[test]
    fn parse_record_args_errors_on_missing_value() {
        let args = vec!["--dialect".to_string()];
        assert!(parse_record_args(args).is_err());
    }

    #[test]
    fn report_ages_marks_stale_and_ok_rows() {
        let ages = vec![
            FixtureAge {
                dialect: "openai".to_string(),
                scenario: "single-text".to_string(),
                client: "openai-sdk".to_string(),
                captured: "2026-06-06".to_string(),
                age_days: Some(5),
                stale: false,
            },
            FixtureAge {
                dialect: "anthropic".to_string(),
                scenario: "tool-call".to_string(),
                client: "claude-code".to_string(),
                captured: "unknown".to_string(),
                age_days: None,
                stale: true,
            },
        ];
        let mut buf = Vec::new();
        report_ages(&mut buf, &ages, 90).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("[ok] openai/single-text via openai-sdk"));
        assert!(text.contains("[STALE] anthropic/tool-call via claude-code"));
        assert!(text.contains("unparseable date"));
        assert!(text.contains("1 fixture(s) past 90d"));
    }
}
