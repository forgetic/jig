//! `xtask` — the repo's developer task runner.
//!
//! P5 (#19) turns the record→fixtures loop into a one-command, repeatable
//! operation and adds an offline staleness check. The crate is split so the
//! decision-making is pure and tested while the impure edges (spawning
//! `jig record`, reading the clock, shelling out to git, walking `fixtures/`)
//! stay thin and live in `main`:
//!
//! - [`matrix`] — the declarative scenario matrix and the planner that expands a
//!   `--dialect`/`--scenario`/`--client` selection into concrete record
//!   invocations. Pure.
//! - [`driver`] — the dispatch table turning each invocation into the concrete
//!   command that records it (the right official-client harness, the pi-SDK
//!   subject harness, or a bare `jig record` fallback). Pure.
//! - [`derive`] — reduce committed authoritative recordings to the masked
//!   `*.template.json` + `drive-shape.json` conformance artifacts (P2, #14). The
//!   reduction is a pure function of the recording bytes (so re-deriving is
//!   deterministic and unit-tested offline); only the file walk/write is impure.
//! - [`staleness`] — capture-age computation over fixture `meta.json` dates. Pure.
//!
//! The actual recording is **manual and online** (it needs a live API key on the
//! client it drives — exactly like `jig record`), so nothing in this crate is
//! exercised by `cargo test` against the network; only the pure planning and
//! staleness logic is. See `docs/how-to/refresh-fixtures.md` for the operator
//! procedure and `docs/explanation/record-and-conform.md` for the design.

pub mod derive;
pub mod driver;
pub mod matrix;
pub mod staleness;

use std::path::Path;
use std::process::Command;

use staleness::{Date, FixtureMeta};

/// Run-wide provenance stamped into every recording so a refresh is reproducible
/// and auditable: the capture date and the recorder's git sha go into each
/// fixture's `meta.json`, and an optional upstream-host override redirects the
/// openai dialect at a compatible backend (DeepSeek, a gateway).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Capture date as ISO-8601 `YYYY-MM-DD`, stamped into `meta.captured`.
    pub captured: String,
    /// Short git sha of the recorder, stamped into `meta.recorder_sha`.
    pub recorder_sha: String,
    /// Optional upstream-host override for the openai dialect (e.g. DeepSeek).
    pub upstream_host: Option<String>,
}

/// Today's date in UTC, as an ISO-8601 `YYYY-MM-DD` string.
///
/// Impure (reads the system clock) and so never used from a test — the staleness
/// logic takes an injected reference date instead. Computed from the Unix epoch
/// without a date crate to keep the dependency footprint minimal.
pub fn today_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Inverse of `Date::day_number`, anchored at the Unix epoch (1970-01-01 = day 0)
/// rather than the proleptic origin: Howard Hinnant's civil-from-days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// The recorder's short git sha (`git rev-parse --short HEAD`), or `"unknown"`
/// when git is unavailable or this is not a checkout. Impure.
pub fn recorder_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Walk `fixtures_root` for every `meta.json` and parse the staleness-relevant
/// fields. Impure (filesystem); returns an empty vector if the root is absent so
/// a fresh checkout with no fixtures yet reports "nothing recorded" cleanly.
///
/// Matches the recorder's taxonomy
/// (`<dialect>/<scenario>/recordings/<client>/meta.json`) by recursing into the
/// tree rather than hardcoding depth, so it tolerates layout tweaks.
pub fn collect_fixture_metas(fixtures_root: &Path) -> std::io::Result<Vec<FixtureMeta>> {
    let mut out = Vec::new();
    if fixtures_root.exists() {
        collect_metas_into(fixtures_root, &mut out)?;
    }
    out.sort_by(|a, b| {
        (a.dialect.as_str(), a.scenario.as_str(), a.client.as_str()).cmp(&(
            b.dialect.as_str(),
            b.scenario.as_str(),
            b.client.as_str(),
        ))
    });
    Ok(out)
}

fn collect_metas_into(dir: &Path, out: &mut Vec<FixtureMeta>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_metas_into(&path, out)?;
        } else if path.file_name().and_then(|n| n.to_str()) == Some("meta.json") {
            let text = std::fs::read_to_string(&path)?;
            if let Ok(meta) = serde_json::from_str::<FixtureMeta>(&text) {
                out.push(meta);
            }
        }
    }
    Ok(())
}

/// Parse `today` for the staleness check, falling back to the system clock when
/// no explicit `--today` was given. Returned date is always valid.
pub fn resolve_today(explicit: Option<&str>) -> Date {
    let s = explicit.map(str::to_string).unwrap_or_else(today_utc);
    Date::parse(&s).unwrap_or_else(|| {
        // `today_utc` always produces a valid date; an explicit bad value falls
        // back to the clock rather than panicking.
        Date::parse(&today_utc()).expect("today_utc produces a valid ISO date")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_round_trips_known_dates() {
        // Day 0 is the Unix epoch.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-06-06 — cross-check against the same value `today_utc` would emit.
        let d = Date::parse("2026-06-06").unwrap();
        // Recompute the epoch day number for that date and invert it.
        let epoch = Date::parse("1970-01-01").unwrap();
        let day = epoch.days_until(d);
        assert_eq!(civil_from_days(day), (2026, 6, 6));
    }

    #[test]
    fn resolve_today_uses_explicit_value_when_valid() {
        let d = resolve_today(Some("2026-06-06"));
        assert_eq!(
            d,
            Date {
                year: 2026,
                month: 6,
                day: 6
            }
        );
    }

    #[test]
    fn collect_metas_walks_the_taxonomy_and_skips_non_meta_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("openai/single-text/recordings/openai-sdk");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"client":"openai-sdk","role":"authoritative","dialect":"openai","scenario":"single-text","captured":"2026-06-06","recorder_sha":"deadbee"}"#,
        )
        .unwrap();
        // Non-meta sibling files must be ignored.
        std::fs::write(dir.join("response.sse"), b"data: [DONE]\n\n").unwrap();
        std::fs::write(dir.join("request.json"), b"{}").unwrap();

        let metas = collect_fixture_metas(root).unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].dialect, "openai");
        assert_eq!(metas[0].captured, "2026-06-06");
    }

    #[test]
    fn collect_metas_on_missing_root_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(collect_fixture_metas(&missing).unwrap().is_empty());
    }
}
