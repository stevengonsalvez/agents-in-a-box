//! Calendar-date parsing shared by every hangar client that authors a deadline.
//!
//! `issue.due_date` is stored as epoch **milliseconds at UTC midnight**
//! (migration 0014) — a calendar DAY, not an instant. multica reached the same
//! shape the hard way: its migration 112 converted `TIMESTAMPTZ → DATE` precisely
//! to kill a timezone bug where a deadline typed in one zone read back as the
//! previous day in another. Parsing at UTC midnight in ONE place keeps the TUI
//! wizard, the CLI, and any future client byte-identical.

/// Parse a calendar date (`YYYY-MM-DD`) into epoch milliseconds at UTC midnight.
///
/// The hangar mirror of multica's `util.ParseCalendarDate`: the format is exact
/// (no `2026/08/01`, no `31-12-2026`, no trailing time), and a rejected input is
/// a caller error surfaced verbatim — never silently coerced to "no due date".
///
/// # Errors
///
/// Returns the human-readable reason when `raw` is not exactly `%Y-%m-%d` (which
/// includes an out-of-range month/day such as `2026-13-01`).
pub fn parse_calendar_date_ms(raw: &str) -> Result<i64, String> {
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| format!("expected a YYYY-MM-DD date, got {raw:?}"))?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| format!("invalid time-of-day for date {raw:?}"))?
        .and_utc();
    Ok(dt.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::parse_calendar_date_ms;

    #[test]
    fn parses_at_utc_midnight() {
        assert_eq!(parse_calendar_date_ms("2026-08-01"), Ok(1_785_542_400_000));
        assert_eq!(parse_calendar_date_ms("1970-01-01"), Ok(0));
        // Pre-epoch dates are legal (a negative deadline is nonsense but the
        // parser is not the policy layer).
        assert!(parse_calendar_date_ms("1969-12-31").is_ok_and(|ms| ms < 0));
    }

    #[test]
    fn rejects_non_iso_shapes() {
        for bad in [
            "31-12-2026",
            "2026-13-01",
            "2026/08/01",
            "",
            "2026-08-01T00:00:00",
        ] {
            assert!(
                parse_calendar_date_ms(bad).is_err(),
                "{bad:?} must be rejected, not coerced"
            );
        }
    }
}
