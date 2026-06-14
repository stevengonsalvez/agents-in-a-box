//! Cron-expression parsing + next-tick calculation (P7.1).
//!
//! A thin, panic-free wrapper over the [`cron`] crate. Two entry points:
//!
//! - [`parse_cron`] validates a cron expression up front (autopilot creation
//!   rejects bad expressions before they reach the database).
//! - [`next_tick_after`] computes the next firing instant strictly after a
//!   given UTC instant — what the scheduler caches in `autopilot.next_tick_at`.
//!
//! # Field arity (POSIX 5-field vs `cron` 6-field)
//!
//! The [`cron`] crate parses **6- or 7-field** expressions, seconds-first
//! (`sec min hour dom mon dow [year]`). Operators (and the reference's Go
//! `robfig/cron` heritage) write the familiar **5-field** POSIX form
//! (`min hour dom mon dow`). [`parse_cron`] bridges the two: a 5-field
//! expression is normalised to 6 fields by prepending a `0` seconds field, so
//! `"0 */6 * * *"` becomes `"0 0 */6 * * *"` and fires on the minute. 6- and
//! 7-field expressions pass through untouched.
//!
//! # Time representation (the storage boundary)
//!
//! The public API here is **chrono-typed** ([`DateTime<Utc>`]) because the
//! [`cron`] crate computes against chrono. The rest of Hangar stores time as
//! **epoch milliseconds (`i64`)** via [`crate::clock::HangarClock::now_ms`].
//! P7.2's storage layer keeps millis; chrono is internal to cron math only.
//! Convert at the boundary with [`millis_to_utc`] / [`utc_to_millis`].
//!
//! # No panics
//!
//! [`cron`]'s parser surfaces a single opaque error kind; this module maps it
//! onto the structured [`CronError`] variants and never `unwrap`s or panics on
//! bad input. DST transitions and leap-second-style inputs are computed in UTC,
//! which has neither a spring-forward gap nor a `:60` second, so the chrono
//! panics those edge cases can trigger in zoned arithmetic never arise.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, TimeZone, Utc};
use cron::Schedule;

/// A validated cron schedule, ready for next-tick queries.
///
/// Constructed only through [`parse_cron`]; wraps the [`cron`] crate's
/// `Schedule` so callers never depend on `cron` types directly.
#[derive(Debug, Clone)]
pub struct CronSchedule {
    schedule: Schedule,
}

impl CronSchedule {
    /// The normalised (6-field) expression this schedule was parsed from.
    #[must_use]
    pub fn source(&self) -> String {
        self.schedule.to_string()
    }
}

/// Why a cron expression could not be parsed or yielded no future tick.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    /// The expression was syntactically invalid: wrong field arity, garbage
    /// tokens, or an unparseable step/range. The `reason` carries a
    /// human-readable explanation.
    #[error("malformed cron expression: {reason}")]
    Malformed {
        /// Human-readable parse-failure detail.
        reason: String,
    },
    /// A field parsed structurally but its value is outside the field's legal
    /// range (e.g. hour `25`, minute `60`, month `13`).
    #[error("cron field `{field}` value `{value}` is out of range")]
    FieldOutOfRange {
        /// The offending field name (`second`, `minute`, `hour`, `day-of-month`,
        /// `month`, `day-of-week`).
        field: String,
        /// The offending value as written in the expression.
        value: String,
    },
    /// The expression is valid but has no firing instant after the queried
    /// time (e.g. a one-shot expression pinned to a past year).
    #[error("cron expression has no future match")]
    NoFutureMatch,
}

/// The inclusive `(min, max)` range each cron field accepts, in 6-field
/// (seconds-first) order. Used to classify a parse failure as
/// [`CronError::FieldOutOfRange`] rather than [`CronError::Malformed`].
const FIELD_RANGES: [(&str, u32, u32); 6] = [
    ("second", 0, 59),
    ("minute", 0, 59),
    ("hour", 0, 23),
    ("day-of-month", 1, 31),
    ("month", 1, 12),
    ("day-of-week", 0, 7), // both 0 and 7 are Sunday
];

/// Parse a cron expression into a validated [`CronSchedule`].
///
/// Accepts the 5-field POSIX form (normalised by prepending a `0` seconds
/// field) as well as the 6-/7-field forms the [`cron`] crate parses natively.
/// All values are interpreted in **UTC** (Hangar v1 is UTC-only).
///
/// # Errors
///
/// - [`CronError::FieldOutOfRange`] when the arity is valid but a numeric field
///   value falls outside its legal range (e.g. `"0 25 * * *"`).
/// - [`CronError::Malformed`] for wrong arity, garbage tokens, or any other
///   syntax error.
pub fn parse_cron(expr: &str) -> Result<CronSchedule, CronError> {
    let normalized = normalize_arity(expr);

    match Schedule::from_str(&normalized) {
        Ok(schedule) => Ok(CronSchedule { schedule }),
        Err(err) => {
            // `cron` collapses every failure into one opaque kind, so we
            // re-inspect the (normalised, correctly-arity'd) fields ourselves to
            // tell an out-of-range value apart from structural garbage.
            Err(
                classify_field_out_of_range(&normalized).unwrap_or_else(|| CronError::Malformed {
                    reason: err.to_string(),
                }),
            )
        }
    }
}

/// The next firing instant strictly after `after`, or `None` when the schedule
/// has no future match (a one-shot expression already in the past).
///
/// Computed entirely in UTC, so it is immune to the spring-forward gap and the
/// leap-second `:60` edge that can panic zoned chrono arithmetic.
#[must_use]
pub fn next_tick_after(schedule: &CronSchedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    schedule.schedule.after(&after).next()
}

/// Convert epoch milliseconds (the storage representation) to a UTC instant.
///
/// The boundary helper P7.2's storage layer uses to feed a stored
/// `next_tick_at` / `clock.now_ms()` into the chrono-typed cron math here.
/// Returns `None` if `ms` is out of chrono's representable range.
#[must_use]
pub fn millis_to_utc(ms: i64) -> Option<DateTime<Utc>> {
    match Utc.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(dt) => Some(dt),
        _ => None,
    }
}

/// Convert a UTC instant back to epoch milliseconds (the storage representation).
///
/// The inverse of [`millis_to_utc`]; P7.2 persists the result of
/// [`next_tick_after`] as an `i64` column.
#[must_use]
pub const fn utc_to_millis(dt: DateTime<Utc>) -> i64 {
    dt.timestamp_millis()
}

/// Normalise a 5-field POSIX expression to the 6-field form `cron` parses by
/// prepending a `0` seconds field. Expressions with any other field count are
/// returned trimmed but otherwise unchanged (the parser will reject them).
fn normalize_arity(expr: &str) -> String {
    let trimmed = expr.trim();
    let field_count = trimmed.split_whitespace().count();
    if field_count == 5 {
        format!("0 {trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// Inspect a (correctly-arity'd) expression that `cron` rejected and, if a
/// single numeric field is plainly out of its legal range, return the matching
/// [`CronError::FieldOutOfRange`]. Returns `None` when no such field is found,
/// so the caller falls back to [`CronError::Malformed`].
fn classify_field_out_of_range(normalized: &str) -> Option<CronError> {
    let fields: Vec<&str> = normalized.split_whitespace().collect();
    // Only 6- or 7-field expressions have a well-defined per-field mapping; any
    // other arity is structurally malformed, not out-of-range.
    if fields.len() != 6 && fields.len() != 7 {
        return None;
    }

    for (field_value, (field_name, min, max)) in fields.iter().zip(FIELD_RANGES.iter()) {
        if let Some(err) = check_field_range(field_value, field_name, *min, *max) {
            return Some(err);
        }
    }
    None
}

/// Check a single cron field token against `(min, max)`. Only plain integers
/// and the numeric endpoints of step/range tokens are validated; symbolic and
/// wildcard tokens (`*`, `MON`, `*/5`) are left for the parser to accept.
fn check_field_range(token: &str, field: &str, min: u32, max: u32) -> Option<CronError> {
    // Split off any step (`*/5`, `1-10/2`) and validate the base expression.
    let base = token.split('/').next().unwrap_or(token);

    for part in base.split(',') {
        for endpoint in part.split('-') {
            if let Ok(value) = endpoint.parse::<u32>() {
                if value < min || value > max {
                    return Some(CronError::FieldOutOfRange {
                        field: field.to_string(),
                        value: endpoint.to_string(),
                    });
                }
            }
        }
    }
    None
}

impl fmt::Display for CronSchedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.schedule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).single().expect("valid utc instant")
    }

    // --- parse acceptance ---

    #[test]
    fn parse_cron_accepts_hourly() {
        assert!(parse_cron("0 */6 * * *").is_ok());
    }

    #[test]
    fn parse_cron_accepts_5min() {
        assert!(parse_cron("*/5 * * * *").is_ok());
    }

    #[test]
    fn parse_cron_accepts_daily() {
        assert!(parse_cron("0 9 * * 1-5").is_ok());
    }

    // --- parse rejection ---

    #[test]
    fn parse_cron_rejects_garbage() {
        match parse_cron("not a cron") {
            Err(CronError::Malformed { .. }) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn parse_cron_rejects_wrong_arity() {
        match parse_cron("0 0") {
            Err(CronError::Malformed { .. }) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn parse_cron_rejects_field_oob() {
        // 5-field `0 25 * * *` normalises to `0 0 25 * * *` — hour 25 is OOB.
        match parse_cron("0 25 * * *") {
            Err(CronError::FieldOutOfRange { field, value }) => {
                assert_eq!(field, "hour");
                assert_eq!(value, "25");
            }
            other => panic!("expected FieldOutOfRange, got {other:?}"),
        }
    }

    // --- next-tick math ---

    #[test]
    fn next_tick_6h_from_midnight() {
        let schedule = parse_cron("0 */6 * * *").expect("parses");
        let after = utc(2026, 1, 1, 0, 0, 0);
        assert_eq!(
            next_tick_after(&schedule, after),
            Some(utc(2026, 1, 1, 6, 0, 0))
        );
    }

    #[test]
    fn next_tick_5min_from_t0_plus_30s() {
        let schedule = parse_cron("*/5 * * * *").expect("parses");
        let after = utc(2026, 1, 1, 0, 0, 30);
        assert_eq!(
            next_tick_after(&schedule, after),
            Some(utc(2026, 1, 1, 0, 5, 0))
        );
    }

    #[test]
    fn next_tick_dst_spring_forward() {
        // US/Eastern springs forward 2026-03-08 02:00 local (== 07:00 UTC), a
        // gap that panics naive zoned arithmetic. We compute in UTC, which has
        // no gap: an hourly schedule must roll cleanly 06:30Z -> 07:00Z with no
        // skip and no panic.
        let schedule = parse_cron("0 * * * *").expect("parses");
        let after = utc(2026, 3, 8, 6, 30, 0);
        assert_eq!(
            next_tick_after(&schedule, after),
            Some(utc(2026, 3, 8, 7, 0, 0))
        );
    }

    #[test]
    fn next_tick_leap_second_safe() {
        // 2016-12-31T23:59:60Z (the real leap second) cannot be expressed by
        // chrono's `with_ymd_and_hms`; the closest representable instant is
        // 23:59:59. An hourly schedule from there must clamp forward to the next
        // valid tick (2017-01-01T00:00:00Z) without panicking.
        let schedule = parse_cron("0 * * * *").expect("parses");
        let after = utc(2016, 12, 31, 23, 59, 59);
        assert_eq!(
            next_tick_after(&schedule, after),
            Some(utc(2017, 1, 1, 0, 0, 0))
        );
    }

    #[test]
    fn next_tick_dec_31_rolls_year() {
        // `0 0 1 1 *` = midnight on Jan 1. From 2026-12-31 the next tick rolls
        // into the following year.
        let schedule = parse_cron("0 0 1 1 *").expect("parses");
        let after = utc(2026, 12, 31, 0, 0, 0);
        assert_eq!(
            next_tick_after(&schedule, after),
            Some(utc(2027, 1, 1, 0, 0, 0))
        );
    }

    // --- error trait surface ---

    #[test]
    fn cron_error_impls_std_error_and_display() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        let malformed = CronError::Malformed {
            reason: "bad".into(),
        };
        let oob = CronError::FieldOutOfRange {
            field: "hour".into(),
            value: "25".into(),
        };
        let none = CronError::NoFutureMatch;
        assert_error(&malformed);
        assert_error(&oob);
        assert_error(&none);
        assert!(!malformed.to_string().is_empty());
        assert!(!oob.to_string().is_empty());
        assert!(!none.to_string().is_empty());
        // Debug surface exists too.
        assert!(!format!("{malformed:?}").is_empty());
    }

    // --- storage-boundary bridge helpers ---

    #[test]
    fn millis_utc_roundtrip() {
        let dt = utc(2026, 1, 1, 0, 5, 0);
        let ms = utc_to_millis(dt);
        assert_eq!(millis_to_utc(ms), Some(dt));
    }

    #[test]
    fn millis_to_utc_feeds_next_tick() {
        // The P7.2 boundary path: clock.now_ms() -> DateTime<Utc> -> next tick.
        let schedule = parse_cron("*/5 * * * *").expect("parses");
        let now_ms = utc_to_millis(utc(2026, 1, 1, 0, 0, 30));
        let after = millis_to_utc(now_ms).expect("in range");
        assert_eq!(
            next_tick_after(&schedule, after),
            Some(utc(2026, 1, 1, 0, 5, 0))
        );
    }
}
