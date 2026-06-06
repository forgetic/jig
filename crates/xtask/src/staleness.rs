//! Fixture staleness: how old each committed recording's capture is.
//!
//! Issue #19 asks for a way to "surface capture age" and an offline check that
//! "warns when fixtures exceed N days (non-fatal)". This module is that check's
//! pure core: given the `captured` date from each fixture's `meta.json` and a
//! reference "today", it computes an age in days and flags the ones past a
//! threshold. It reads no clock and walks no filesystem — `today` and the meta
//! list are passed in — so it runs under `cargo test` with deterministic input,
//! and `main` supplies the real clock and `fixtures/` walk for the live run.
//!
//! Dates are ISO-8601 `YYYY-MM-DD`, the same form the recorder stamps. To avoid
//! pulling in a date crate (the workspace deliberately stays clock/VCS-free in
//! its libraries), age is computed by converting each date to a day number under
//! the proleptic Gregorian calendar and subtracting.

use serde::Deserialize;

/// The default age past which a fixture is reported stale. Non-fatal: exceeding
/// it is a nudge to re-record, not a build break.
pub const DEFAULT_MAX_AGE_DAYS: i64 = 90;

/// A calendar date as `(year, month, day)`, parsed from an ISO-8601
/// `YYYY-MM-DD` string. Only the fields needed to compute a day delta are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

impl Date {
    /// Parse an ISO-8601 `YYYY-MM-DD` date, rejecting anything that is not a
    /// well-formed calendar date (so a `captured: "unknown"` placeholder, which
    /// the recorder writes when no date was supplied, is surfaced as unparseable
    /// rather than silently treated as fresh).
    pub fn parse(s: &str) -> Option<Date> {
        let mut parts = s.trim().splitn(3, '-');
        let year: i64 = parts.next()?.parse().ok()?;
        let month: u32 = parts.next()?.parse().ok()?;
        let day: u32 = parts.next()?.parse().ok()?;
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    /// This date as a day number (days since the proleptic-Gregorian epoch). Only
    /// differences between two such numbers are meaningful; the absolute origin
    /// is arbitrary but consistent.
    fn day_number(self) -> i64 {
        // Howard Hinnant's days-from-civil algorithm.
        let y = if self.month <= 2 {
            self.year - 1
        } else {
            self.year
        };
        let era = y.div_euclid(400);
        let yoe = y - era * 400; // [0, 399]
        let m = self.month as i64;
        let d = self.day as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
        era * 146097 + doe - 719468
    }

    /// Whole days from `self` to `other` (`other - self`); negative if `other`
    /// precedes `self`.
    pub fn days_until(self, other: Date) -> i64 {
        other.day_number() - self.day_number()
    }
}

/// Whether `year` is a leap year under the proleptic Gregorian calendar.
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in `month` of `year` (`month` is 1-12).
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Just the fields of a fixture's `meta.json` that staleness cares about. Other
/// keys (role, client, model, …) are ignored by serde.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureMeta {
    pub dialect: String,
    pub scenario: String,
    pub client: String,
    pub captured: String,
}

/// Where a fixture lives plus how its capture date evaluated. `age_days` is
/// `None` when `captured` could not be parsed (e.g. the `unknown` placeholder),
/// which is itself reported so a never-stamped fixture does not hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureAge {
    pub dialect: String,
    pub scenario: String,
    pub client: String,
    pub captured: String,
    /// Age in days relative to the reference date, or `None` if unparseable.
    pub age_days: Option<i64>,
    /// `true` when the age exceeds the threshold or the date is unparseable.
    pub stale: bool,
}

/// Compute the age and staleness of each fixture relative to `today`.
///
/// A fixture is stale when its capture is older than `max_age_days`, or when its
/// `captured` date cannot be parsed at all (an unstamped fixture is treated as
/// stale so it surfaces in the report). Pure: deterministic in its inputs.
pub fn evaluate(metas: &[FixtureMeta], today: Date, max_age_days: i64) -> Vec<FixtureAge> {
    metas
        .iter()
        .map(|m| {
            let age_days = Date::parse(&m.captured).map(|d| d.days_until(today));
            let stale = match age_days {
                Some(age) => age > max_age_days,
                None => true,
            };
            FixtureAge {
                dialect: m.dialect.clone(),
                scenario: m.scenario.clone(),
                client: m.client.clone(),
                captured: m.captured.clone(),
                age_days,
                stale,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(captured: &str) -> FixtureMeta {
        FixtureMeta {
            dialect: "openai".to_string(),
            scenario: "single-text".to_string(),
            client: "openai-sdk".to_string(),
            captured: captured.to_string(),
        }
    }

    #[test]
    fn parses_valid_dates_and_rejects_garbage() {
        assert_eq!(
            Date::parse("2026-06-06"),
            Some(Date {
                year: 2026,
                month: 6,
                day: 6
            })
        );
        assert_eq!(Date::parse("unknown"), None);
        assert_eq!(Date::parse("2026-13-01"), None); // bad month
        assert_eq!(Date::parse("2026-02-30"), None); // bad day
        assert_eq!(Date::parse("2026-06"), None); // missing day
        // Leap-year boundary: 2024 is a leap year, 2026 is not.
        assert!(Date::parse("2024-02-29").is_some());
        assert_eq!(Date::parse("2026-02-29"), None);
    }

    #[test]
    fn day_deltas_are_correct_across_month_and_year_boundaries() {
        let d = |s: &str| Date::parse(s).unwrap();
        assert_eq!(d("2026-06-06").days_until(d("2026-06-07")), 1);
        assert_eq!(d("2026-06-06").days_until(d("2026-06-06")), 0);
        // Across a month boundary.
        assert_eq!(d("2026-01-31").days_until(d("2026-02-01")), 1);
        // Across a (non-leap) year boundary: 2025-12-31 → 2026-01-01.
        assert_eq!(d("2025-12-31").days_until(d("2026-01-01")), 1);
        // A full common year.
        assert_eq!(d("2025-01-01").days_until(d("2026-01-01")), 365);
        // A full leap year (2024).
        assert_eq!(d("2024-01-01").days_until(d("2025-01-01")), 366);
        // Negative when the target precedes the date.
        assert_eq!(d("2026-06-06").days_until(d("2026-06-01")), -5);
    }

    #[test]
    fn fresh_fixture_is_not_stale() {
        let today = Date::parse("2026-06-06").unwrap();
        let ages = evaluate(&[meta("2026-06-01")], today, DEFAULT_MAX_AGE_DAYS);
        assert_eq!(ages[0].age_days, Some(5));
        assert!(!ages[0].stale);
    }

    #[test]
    fn fixture_past_the_threshold_is_stale() {
        let today = Date::parse("2026-06-06").unwrap();
        // 100 days before today, threshold 90.
        let ages = evaluate(&[meta("2026-02-26")], today, 90);
        assert_eq!(ages[0].age_days, Some(100));
        assert!(ages[0].stale);
    }

    #[test]
    fn exactly_at_the_threshold_is_not_yet_stale() {
        let today = Date::parse("2026-06-06").unwrap();
        let captured = Date {
            year: 2026,
            month: 6,
            day: 6,
        };
        // Pick a date exactly 90 days earlier by walking back via days_until.
        // 2026-03-08 is 90 days before 2026-06-06.
        let ages = evaluate(&[meta("2026-03-08")], today, 90);
        assert_eq!(captured.days_until(today), 0);
        assert_eq!(ages[0].age_days, Some(90));
        assert!(!ages[0].stale, "age == threshold is a nudge, not yet stale");
    }

    #[test]
    fn unparseable_capture_date_is_reported_stale() {
        let today = Date::parse("2026-06-06").unwrap();
        let ages = evaluate(&[meta("unknown")], today, 90);
        assert_eq!(ages[0].age_days, None);
        assert!(ages[0].stale);
    }

    #[test]
    fn fixture_meta_deserializes_ignoring_extra_keys() {
        let json = r#"{
            "client": "openai-sdk",
            "role": "authoritative",
            "dialect": "openai",
            "scenario": "single-text",
            "model": "gpt-4o-mini",
            "captured": "2026-06-06",
            "recorder_sha": "deadbee"
        }"#;
        let meta: FixtureMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.dialect, "openai");
        assert_eq!(meta.scenario, "single-text");
        assert_eq!(meta.client, "openai-sdk");
        assert_eq!(meta.captured, "2026-06-06");
    }
}
