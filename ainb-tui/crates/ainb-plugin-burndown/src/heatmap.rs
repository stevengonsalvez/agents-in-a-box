// ABOUTME: Pure data layer for the GitHub-style contribution heatmap —
// ABOUTME: day bucketing, intensity levels, cursor movement, per-day model
// ABOUTME: breakdown. No ratatui, no clock reads, so it unit-tests headless.
//! The render side owns only glyphs and colours; every decision that can be
//! wrong (which day a call belongs to, how hot a day is, whether a day is
//! genuinely idle or merely unpriced) lives here where a test can pin it.
//!
//! Two invariants drive the design:
//!
//! * **`today` is injected, never read.** `Local::now()` inside a builder makes
//!   the grid untestable and makes the TUI's "today" drift mid-session.
//! * **A pricing gap is not a day off.** A day with traffic but no published
//!   rate renders [`CellLevel::Unpriced`] under the cost metric instead of
//!   collapsing into the same blank cell as a day you did not work. This is the
//!   same concern that makes `data::usage::sort_by_bucket_desc` rank priced
//!   rows above unpriced ones rather than comparing dollars to tokens.

use std::collections::HashMap;

use chrono::{Datelike, Duration, Local, NaiveDate};

use crate::data::usage::UsageData;

/// Lower bound on the grid width. A zero-column grid has no `first_date`,
/// so callers asking for one get a single week rather than a panic path.
const MIN_WEEKS: usize = 1;

/// Upper bound on the grid width (~10 years). The grid is rebuilt on the
/// render path; an unclamped `weeks` turns a bad config value into an
/// unbounded per-frame allocation.
const MAX_WEEKS: usize = 520;

const DAYS_PER_WEEK: usize = 7;

/// GitHub's canvas width; the grid never reflows to the active period, so a
/// short period lights fewer cells rather than collapsing the grid.
pub const WEEKS_IN_CANVAS: usize = 53;

const MONTH_ABBREVS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Which quantity the heatmap colours by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeatMetric {
    #[default]
    Cost,
    Tokens,
    Calls,
    Sessions,
}

impl HeatMetric {
    /// Selector order, kept in lockstep with [`HeatMetric::next`] so the
    /// visible list and the hotkey cycle cannot drift apart.
    pub const ALL: &'static [HeatMetric] = &[Self::Cost, Self::Tokens, Self::Calls, Self::Sessions];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cost => "cost",
            Self::Tokens => "tokens",
            Self::Calls => "calls",
            Self::Sessions => "sessions",
        }
    }

    /// Cycle order for the metric hotkey. Wraps back to `Cost`.
    pub fn next(self) -> Self {
        match self {
            Self::Cost => Self::Tokens,
            Self::Tokens => Self::Calls,
            Self::Calls => Self::Sessions,
            Self::Sessions => Self::Cost,
        }
    }
}

/// Intensity bucket for one day cell.
///
/// `Unpriced` is deliberately not part of the ordered ramp: it means "we
/// cannot say how hot this day was", which is a different statement from
/// any of `Empty`..`L4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellLevel {
    Empty,
    L1,
    L2,
    L3,
    L4,
    Unpriced,
}

impl CellLevel {
    /// The less-to-more legend ramp. [`CellLevel::Unpriced`] is excluded on
    /// purpose: `✗` means "value unknown", which is not a step on the
    /// intensity scale and gets its own legend entry.
    pub const SCALE: &'static [CellLevel] = &[Self::Empty, Self::L1, Self::L2, Self::L3, Self::L4];

    pub fn glyph(self) -> char {
        match self {
            Self::Empty => '·',
            Self::L1 => '░',
            Self::L2 => '▒',
            Self::L3 => '▓',
            Self::L4 => '█',
            Self::Unpriced => '✗',
        }
    }
}

/// One day's totals plus its resolved intensity.
///
/// `cost_usd` stays `Option` rather than collapsing to `0.0` so the render
/// side can print "n/a" instead of a misleading `$0.00`.
#[derive(Debug, Clone, PartialEq)]
pub struct DayCell {
    pub date:     NaiveDate,
    pub cost_usd: Option<f64>,
    pub tokens:   u64,
    pub calls:    usize,
    pub sessions: usize,
    pub level:    CellLevel,
}

impl DayCell {
    /// `true` when the day saw traffic, regardless of whether it was priced.
    fn is_active(&self) -> bool {
        self.tokens > 0 || self.calls > 0
    }

    /// The day's value under `metric`. Unpriced days read as `0.0` under
    /// `Cost`; callers that care about the distinction check
    /// [`DayCell::is_active`] first.
    fn value(&self, metric: HeatMetric) -> f64 {
        match metric {
            HeatMetric::Cost => self.cost_usd.unwrap_or(0.0),
            HeatMetric::Tokens => self.tokens as f64,
            HeatMetric::Calls => self.calls as f64,
            HeatMetric::Sessions => self.sessions as f64,
        }
    }
}

/// A fixed-width calendar canvas ending on the week that contains `today`.
///
/// The shape is rectangular by construction: `weeks.len()` columns of
/// exactly 7 slots each, Monday-first. Slots outside the covered date span
/// (leading days of the oldest week, trailing days after `today`) are
/// `None`, which keeps the renderer free of any date arithmetic.
#[derive(Debug, Clone)]
pub struct HeatmapGrid {
    pub weeks:     Vec<Vec<Option<DayCell>>>,
    pub metric:    HeatMetric,
    pub max_value: f64,
}

impl HeatmapGrid {
    pub fn build(data: &UsageData, metric: HeatMetric, today: NaiveDate, weeks: usize) -> Self {
        let weeks = weeks.clamp(MIN_WEEKS, MAX_WEEKS);
        let span_days = weeks * DAYS_PER_WEEK;

        // Monday of the oldest column. Saturating at the calendar floor
        // keeps an absurd `today` (or `weeks`) off the panic path; the grid
        // just covers fewer real days.
        let last_monday = monday_of(today);
        let first_date = last_monday
            .checked_sub_signed(Duration::days(((weeks - 1) * DAYS_PER_WEEK) as i64))
            .unwrap_or(NaiveDate::MIN);

        let totals = daily_totals(data, first_date, today);

        let mut cells: Vec<Option<DayCell>> = Vec::with_capacity(span_days);
        for offset in 0..span_days {
            let Some(date) = first_date.checked_add_signed(Duration::days(offset as i64)) else {
                cells.push(None);
                continue;
            };
            if date > today {
                cells.push(None);
                continue;
            }
            let bucket = totals.get(&date);
            cells.push(Some(DayCell {
                date,
                cost_usd: bucket.and_then(|b| b.cost_usd),
                tokens: bucket.map_or(0, |b| b.tokens),
                calls: bucket.map_or(0, |b| b.calls),
                sessions: bucket.map_or(0, |b| b.sessions),
                // Filled in below, once the whole span is known.
                level: CellLevel::Empty,
            }));
        }

        // Unpriced days contribute nothing to the scale: their cost is
        // unknown, not zero, so folding them in either way would be a lie.
        // Stated explicitly rather than relying on `unwrap_or(0.0)`.
        let max_value = cells
            .iter()
            .flatten()
            .filter(|cell| !(metric == HeatMetric::Cost && cell.cost_usd.is_none()))
            .map(|cell| cell.value(metric))
            .fold(0.0_f64, f64::max);

        for cell in cells.iter_mut().flatten() {
            cell.level = level_for(cell, metric, max_value);
        }

        let weeks_grid = cells.chunks(DAYS_PER_WEEK).map(<[Option<DayCell>]>::to_vec).collect();

        Self {
            weeks: weeks_grid,
            metric,
            max_value,
        }
    }

    /// O(1) lookup by date. `None` for any date outside the covered span.
    pub fn cell(&self, date: NaiveDate) -> Option<&DayCell> {
        let offset = (date - self.first_date()).num_days();
        let offset = usize::try_from(offset).ok()?;
        let (col, row) = (offset / DAYS_PER_WEEK, offset % DAYS_PER_WEEK);
        self.weeks.get(col)?.get(row)?.as_ref()
    }

    /// Move a cursor `dx` columns and `dy` rows, clamped to the covered
    /// span. Clamping rather than wrapping keeps arrow-key navigation from
    /// teleporting the cursor across a year when it hits an edge.
    pub fn step(&self, from: NaiveDate, dx: i32, dy: i32) -> NaiveDate {
        let first = self.first_date();
        let last = self.last_date();
        let span = (last - first).num_days();

        let delta = i64::from(dx) * DAYS_PER_WEEK as i64 + i64::from(dy);
        let target = (from - first).num_days().saturating_add(delta);

        first.checked_add_signed(Duration::days(target.clamp(0, span))).unwrap_or(last)
    }

    /// Monday of the oldest column. Not necessarily a day with data.
    pub fn first_date(&self) -> NaiveDate {
        self.weeks
            .first()
            .and_then(|week| week.iter().flatten().next())
            .map_or(NaiveDate::MIN, |cell| cell.date)
    }

    /// The most recent covered day, i.e. the `today` passed to [`Self::build`].
    pub fn last_date(&self) -> NaiveDate {
        self.weeks
            .iter()
            .rev()
            .flat_map(|week| week.iter().rev())
            .flatten()
            .next()
            .map_or(self.first_date(), |cell| cell.date)
    }

    /// One entry per column; `Some(abbrev)` only where a column starts a
    /// month that the previous column did not cover. The oldest column is
    /// always labelled so the axis has an anchor.
    pub fn month_labels(&self) -> Vec<Option<&'static str>> {
        let mut labels = Vec::with_capacity(self.weeks.len());
        let mut previous_month: Option<u32> = None;
        for (col, _) in self.weeks.iter().enumerate() {
            let Some(start) = self
                .first_date()
                .checked_add_signed(Duration::days((col * DAYS_PER_WEEK) as i64))
            else {
                labels.push(None);
                continue;
            };
            let month = start.month();
            let changed = previous_month != Some(month);
            previous_month = Some(month);
            labels.push(if changed { month_abbrev(month) } else { None });
        }
        labels
    }
}

/// Top models for one day, ranked cost-first then tokens, capped at `limit`.
///
/// Mirrors `data::usage::sort_by_bucket_desc`: an unpriced model never
/// outranks a priced one, because a token count compared against a dollar
/// figure wins by five orders of magnitude and empties the panel.
pub fn day_model_breakdown(
    data: &UsageData,
    date: NaiveDate,
    limit: usize,
) -> Vec<(String, Option<f64>, u64)> {
    if limit == 0 {
        return Vec::new();
    }

    let mut per_model: HashMap<&str, (Option<f64>, u64)> = HashMap::new();
    for call in &data.calls {
        if local_day(call.timestamp) != date {
            continue;
        }
        let entry = per_model.entry(call.model.as_str()).or_insert((None, 0));
        entry.0 = match (entry.0, call.cost_usd) {
            (Some(acc), Some(add)) => Some(acc + add),
            (Some(acc), None) => Some(acc),
            (None, other) => other,
        };
        entry.1 = entry.1.saturating_add(call_tokens(call));
    }

    let mut rows: Vec<(String, Option<f64>, u64)> = per_model
        .into_iter()
        .map(|(model, (cost, tokens))| (model.to_string(), cost, tokens))
        .collect();

    rows.sort_by(|a, b| {
        b.1.is_some()
            .cmp(&a.1.is_some())
            .then_with(|| rank_value(b).total_cmp(&rank_value(a)))
            // Name tiebreak so equal rows keep a stable, reproducible order.
            .then_with(|| a.0.cmp(&b.0))
    });
    rows.truncate(limit);
    rows
}

fn rank_value(row: &(String, Option<f64>, u64)) -> f64 {
    row.1.unwrap_or(row.2 as f64)
}

fn call_tokens(call: &crate::data::usage::ProviderCall) -> u64 {
    call.input_tokens
        .saturating_add(call.cache_creation_tokens)
        .saturating_add(call.cache_read_tokens)
        .saturating_add(call.output_tokens)
        .saturating_add(call.reasoning_tokens)
}

/// Day bucketing is local-tz throughout the burndown pipeline (see
/// `data::usage::aggregate_calls`); cached timestamps are UTC, so the
/// conversion happens at every read boundary including this one.
fn local_day(timestamp: chrono::DateTime<chrono::Utc>) -> NaiveDate {
    timestamp.with_timezone(&Local).date_naive()
}

/// Flat per-day totals for the covered span, lifted from the already
/// local-tz-bucketed `data.daily` rather than rescanning `data.calls`:
/// the grid is rebuilt every frame and `calls` can run to six figures.
struct DayTotals {
    cost_usd: Option<f64>,
    tokens:   u64,
    calls:    usize,
    sessions: usize,
}

fn daily_totals(
    data: &UsageData,
    first: NaiveDate,
    last: NaiveDate,
) -> HashMap<NaiveDate, DayTotals> {
    data.daily
        .iter()
        .filter(|(date, _)| *date >= first && *date <= last)
        .map(|(date, bucket)| {
            (
                *date,
                DayTotals {
                    cost_usd: bucket.cost_usd,
                    tokens:   bucket.total(),
                    calls:    bucket.call_count,
                    sessions: bucket.session_count,
                },
            )
        })
        .collect()
}

fn level_for(cell: &DayCell, metric: HeatMetric, max_value: f64) -> CellLevel {
    if metric == HeatMetric::Cost && cell.cost_usd.is_none() {
        return if cell.is_active() {
            CellLevel::Unpriced
        } else {
            CellLevel::Empty
        };
    }

    let value = cell.value(metric);
    if value <= 0.0 {
        return CellLevel::Empty;
    }
    if max_value <= 0.0 {
        return CellLevel::L1;
    }

    let ratio = value / max_value;
    if ratio <= 0.25 {
        CellLevel::L1
    } else if ratio <= 0.5 {
        CellLevel::L2
    } else if ratio <= 0.75 {
        CellLevel::L3
    } else {
        CellLevel::L4
    }
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    let back = i64::from(date.weekday().num_days_from_monday());
    date.checked_sub_signed(Duration::days(back)).unwrap_or(date)
}

fn month_abbrev(month: u32) -> Option<&'static str> {
    let index = usize::try_from(month.checked_sub(1)?).ok()?;
    MONTH_ABBREVS.get(index).copied()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::data::usage::TokenBucket;
    use crate::test_support::ProviderCallBuilder;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap_or(NaiveDate::MIN)
    }

    /// A bucket with `tokens` split across input, `calls` calls, `sessions`
    /// sessions, and an optional price.
    fn bucket(tokens: u64, calls: usize, sessions: usize, cost: Option<f64>) -> TokenBucket {
        TokenBucket {
            input_tokens: tokens,
            call_count: calls,
            session_count: sessions,
            cost_usd: cost,
            ..TokenBucket::default()
        }
    }

    fn data_with_days(days: &[(NaiveDate, TokenBucket)]) -> UsageData {
        UsageData {
            daily: days.to_vec(),
            ..UsageData::default()
        }
    }

    #[test]
    fn heatmap_zero_day_is_empty_and_max_day_is_l4() {
        let today = date(2026, 3, 12);
        let data = data_with_days(&[
            (date(2026, 3, 9), bucket(0, 0, 0, Some(0.0))),
            (date(2026, 3, 10), bucket(1_000, 4, 1, Some(2.50))),
            (date(2026, 3, 11), bucket(9_000, 40, 3, Some(10.0))),
        ]);

        let grid = HeatmapGrid::build(&data, HeatMetric::Cost, today, 4);

        assert_eq!(grid.max_value, 10.0);
        let zero = grid.cell(date(2026, 3, 9));
        assert_eq!(zero.map(|c| c.level), Some(CellLevel::Empty));
        let hottest = grid.cell(date(2026, 3, 11));
        assert_eq!(hottest.map(|c| c.level), Some(CellLevel::L4));
        // 2.50 / 10.00 = 25% -> the top of the L1 bucket.
        assert_eq!(
            grid.cell(date(2026, 3, 10)).map(|c| c.level),
            Some(CellLevel::L1)
        );
        // A day with no row at all is still a real (idle) cell.
        assert_eq!(
            grid.cell(date(2026, 3, 8)).map(|c| c.level),
            Some(CellLevel::Empty)
        );
    }

    #[test]
    fn heatmap_unpriced_day_is_marked_under_cost_only() {
        let today = date(2026, 3, 12);
        let data = data_with_days(&[
            (date(2026, 3, 10), bucket(500_000, 30, 2, None)),
            (date(2026, 3, 11), bucket(1_000, 5, 1, Some(4.0))),
        ]);

        let cost = HeatmapGrid::build(&data, HeatMetric::Cost, today, 4);
        assert_eq!(
            cost.cell(date(2026, 3, 10)).map(|c| c.level),
            Some(CellLevel::Unpriced),
            "an active day with no published rate must not render as a day off"
        );
        assert_eq!(
            cost.max_value, 4.0,
            "the unpriced day must not participate in the cost scale"
        );

        let tokens = HeatmapGrid::build(&data, HeatMetric::Tokens, today, 4);
        assert_eq!(
            tokens.cell(date(2026, 3, 10)).map(|c| c.level),
            Some(CellLevel::L4),
            "under a non-cost metric the same day is just the busiest cell"
        );
        assert_eq!(tokens.max_value, 500_000.0);
    }

    #[test]
    fn heatmap_grid_is_rectangular_and_stops_at_today() {
        // 2026-03-12 is a Thursday: rows 4..6 of the last column are future.
        let today = date(2026, 3, 12);
        let grid = HeatmapGrid::build(&UsageData::default(), HeatMetric::Cost, today, 6);

        assert_eq!(grid.weeks.len(), 6);
        for week in &grid.weeks {
            assert_eq!(week.len(), 7, "every column must have exactly 7 slots");
        }

        let last = grid.weeks.last().map(Vec::as_slice).unwrap_or_default();
        let present: Vec<bool> = last.iter().map(Option::is_some).collect();
        assert_eq!(
            present,
            vec![true, true, true, true, false, false, false],
            "slots after today are None"
        );

        assert_eq!(grid.last_date(), today);
        assert_eq!(grid.first_date(), date(2026, 2, 2));
        assert_eq!(
            grid.first_date().weekday(),
            chrono::Weekday::Mon,
            "column 0 starts on a Monday"
        );
        assert!(
            grid.cell(date(2026, 3, 13)).is_none(),
            "tomorrow is not covered"
        );
        assert!(grid.cell(date(2026, 2, 1)).is_none(), "before the span");
    }

    #[test]
    fn heatmap_step_moves_by_week_and_day_and_clamps() {
        let today = date(2026, 3, 12);
        let grid = HeatmapGrid::build(&UsageData::default(), HeatMetric::Cost, today, 6);
        let first = grid.first_date();

        let mid = date(2026, 3, 4);
        assert_eq!(grid.step(mid, 0, 1), date(2026, 3, 5));
        assert_eq!(grid.step(mid, 0, -1), date(2026, 3, 3));
        assert_eq!(grid.step(mid, -1, 0), date(2026, 2, 25));
        assert_eq!(grid.step(mid, 1, 0), date(2026, 3, 11));

        assert_eq!(grid.step(first, -1, 0), first, "clamps at the oldest edge");
        assert_eq!(grid.step(first, 0, -99), first);
        assert_eq!(grid.step(today, 1, 0), today, "clamps at today");
        assert_eq!(grid.step(today, 0, 99), today);
        // A cursor that starts out of range is pulled back inside.
        assert_eq!(grid.step(date(2030, 1, 1), 0, 0), today);
        assert_eq!(grid.step(date(2020, 1, 1), 0, 0), first);
    }

    #[test]
    fn heatmap_month_labels_mark_only_month_starts() {
        let today = date(2026, 3, 12);
        let grid = HeatmapGrid::build(&UsageData::default(), HeatMetric::Cost, today, 6);

        // Columns start 2026-02-02, 02-09, 02-16, 02-23, 03-02, 03-09.
        assert_eq!(
            grid.month_labels(),
            vec![Some("Feb"), None, None, None, Some("Mar"), None]
        );
    }

    #[test]
    fn heatmap_day_breakdown_ranks_priced_above_unpriced() {
        let stamp = Utc.with_ymd_and_hms(2026, 3, 10, 12, 0, 0).single().unwrap_or_else(Utc::now);
        let day = local_day(stamp);

        let data = UsageData {
            calls: vec![
                ProviderCallBuilder::new()
                    .with_model("mystery-model")
                    .with_timestamp(stamp)
                    .with_input_tokens(500_000_000)
                    .build(),
                ProviderCallBuilder::new()
                    .with_model("claude-sonnet-4-5")
                    .with_timestamp(stamp)
                    .with_input_tokens(1_000)
                    .with_cost(0.01)
                    .build(),
                // A different day must not leak into the breakdown.
                ProviderCallBuilder::new()
                    .with_model("other-day-model")
                    .with_timestamp(stamp - Duration::days(3))
                    .with_input_tokens(9_000)
                    .with_cost(99.0)
                    .build(),
            ],
            ..UsageData::default()
        };

        let rows = day_model_breakdown(&data, day, 5);
        assert_eq!(rows.len(), 2, "only the requested day's models");
        assert_eq!(
            rows.first().map(|r| r.0.as_str()),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(rows.get(1).map(|r| r.0.as_str()), Some("mystery-model"));
        assert_eq!(rows.get(1).and_then(|r| r.1), None);
        assert_eq!(rows.get(1).map(|r| r.2), Some(500_000_000));

        assert!(day_model_breakdown(&data, day, 0).is_empty());
        assert_eq!(day_model_breakdown(&data, day, 1).len(), 1);
    }

    #[test]
    fn heatmap_empty_data_does_not_panic() {
        let today = date(2026, 3, 12);
        let data = UsageData::default();

        for metric in [
            HeatMetric::Cost,
            HeatMetric::Tokens,
            HeatMetric::Calls,
            HeatMetric::Sessions,
        ] {
            let grid = HeatmapGrid::build(&data, metric, today, 0);
            assert_eq!(grid.weeks.len(), 1, "weeks is clamped up to one column");
            assert_eq!(grid.max_value, 0.0);
            assert!(
                grid.weeks.iter().flatten().flatten().all(|cell| cell.level == CellLevel::Empty)
            );
            assert_eq!(grid.last_date(), today);
            assert_eq!(grid.step(today, -5, -5), grid.first_date());
            assert!(day_model_breakdown(&data, today, 5).is_empty());
        }
    }

    #[test]
    fn heatmap_metric_cycles_and_labels() {
        assert_eq!(HeatMetric::default(), HeatMetric::Cost);
        let mut metric = HeatMetric::Cost;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(metric.label());
            metric = metric.next();
        }
        assert_eq!(seen, vec!["cost", "tokens", "calls", "sessions"]);
        assert_eq!(metric, HeatMetric::Cost, "the cycle wraps");
    }

    #[test]
    fn heatmap_metric_all_matches_next_cycle() {
        let mut metric = HeatMetric::default();
        let cycled: Vec<HeatMetric> = (0..HeatMetric::ALL.len())
            .map(|_| {
                let current = metric;
                metric = metric.next();
                current
            })
            .collect();
        assert_eq!(
            cycled,
            HeatMetric::ALL.to_vec(),
            "the selector order and the hotkey cycle must not drift apart"
        );
        assert_eq!(metric, HeatMetric::default(), "one full lap returns home");
    }

    #[test]
    fn heatmap_scale_is_the_ramp_without_unpriced() {
        assert_eq!(
            CellLevel::SCALE,
            &[
                CellLevel::Empty,
                CellLevel::L1,
                CellLevel::L2,
                CellLevel::L3,
                CellLevel::L4
            ]
        );
        assert!(
            !CellLevel::SCALE.contains(&CellLevel::Unpriced),
            "'unknown' is not a step on a less-to-more scale"
        );
    }

    #[test]
    fn heatmap_canvas_width_covers_a_full_year() {
        assert_eq!(WEEKS_IN_CANVAS, 53);
        let today = date(2026, 3, 12);
        let grid = HeatmapGrid::build(
            &UsageData::default(),
            HeatMetric::Cost,
            today,
            WEEKS_IN_CANVAS,
        );
        assert_eq!(grid.weeks.len(), WEEKS_IN_CANVAS);
        assert!(
            (grid.last_date() - grid.first_date()).num_days() >= 365,
            "the fixed canvas always spans at least a year"
        );
    }

    #[test]
    fn heatmap_cell_glyphs_are_distinct() {
        let glyphs: Vec<char> = [
            CellLevel::Empty,
            CellLevel::L1,
            CellLevel::L2,
            CellLevel::L3,
            CellLevel::L4,
            CellLevel::Unpriced,
        ]
        .into_iter()
        .map(CellLevel::glyph)
        .collect();
        assert_eq!(glyphs, vec!['·', '░', '▒', '▓', '█', '✗']);
    }
}
