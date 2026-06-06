// ABOUTME: Live OAuth-window reader with three-tier fallback.
//
// Tier 1 (Cache):    OS-specific cache dir (via `dirs::cache_dir()`,
//                    e.g. `~/.cache/ainb/live.json` on Linux,
//                    `~/Library/Caches/ainb/live.json` on macOS) written
//                    by the `ainb statusline` hook. Authoritative —
//                    Claude Code only exposes rate_limits to statusline
//                    subprocesses.
// Tier 2 (Local):    Native 5-hour-block aggregation over the local
//                    Claude JSONL transcripts (mirrors `ccusage blocks
//                    --active --json` logic). Provides 5h burn but not
//                    the 7d window — that data simply isn't local.
// Tier 3 (None):     Empty `LiveWindow`; render path falls back to a
//                    "wire the statusline" CTA.

use std::time::Duration;

use chrono::{DateTime, Datelike, Timelike, Utc};
use tracing::{debug, trace};

use crate::cli::statusline::{LiveCache, cache_path, read_cache};
use crate::models::usage::ProviderCall;

/// Maximum age of the Tier 1 cache before we consider it stale and fall
/// back to Tier 2.
pub const CACHE_MAX_AGE_SECS: u64 = 120;

/// Default token cap for the Pro 5-hour window when Tier 2 has to
/// estimate burn percentage. Override via `AINB_TOKEN_LIMIT`.
pub const DEFAULT_TOKEN_LIMIT: u64 = 220_000;

/// Where the data came from. Drives the render path's fidelity choices
/// (Tier1 → bars + cost + reset; Tier2 → 5h bar only; None → CTA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Tier1Cache,
    Tier2Local,
    None,
}

/// Snapshot of the user's live OAuth-window state at one point in time.
#[derive(Debug, Clone, Default)]
pub struct LiveWindow {
    pub five_hour_pct: Option<u8>,
    pub seven_day_pct: Option<u8>,
    pub today_cost_usd: Option<f64>,
    /// Time until the soonest active window resets, when known. Currently
    /// unused within ainb-core — the top bar renders the absolute per-window
    /// instants (`five_hour_resets_at` / `seven_day_resets_at`) instead —
    /// but kept on the struct for potential countdown consumers. NB: the
    /// burndown plugin has its own separate `LiveWindow` and does not read
    /// this field.
    pub resets_in: Option<Duration>,
    /// Absolute instant the 5-hour window resets, when known. Drives the
    /// top bar's "↻ <date> <time>" affordance for the 5h quota.
    pub five_hour_resets_at: Option<DateTime<Utc>>,
    /// Absolute instant the 7-day window resets, when known. Drives the
    /// top bar's "↻ <date> <time>" affordance for the weekly quota.
    pub seven_day_resets_at: Option<DateTime<Utc>>,
    pub context_pct: Option<u8>,
    pub model: Option<String>,
    pub source: Source,
}

impl LiveWindow {
    pub fn empty() -> Self {
        Self {
            source: Source::None,
            ..Default::default()
        }
    }
}

impl Default for Source {
    fn default() -> Self {
        Self::None
    }
}

/// Read the current live window using the three-tier cascade.
///
/// In production the call site is the TUI render path on the Burndown
/// view — keep it cheap. Tier 1 is a single small JSON file. Tier 2 only
/// runs when Tier 1 is missing/stale and only walks the *most recent*
/// JSONL files (filtered by mtime).
pub fn current() -> LiveWindow {
    // Tier1 hit is the steady-state happy path — called on every
    // watcher tick (5s) when the statusline is wired. Log it at
    // trace so it doesn't dominate JSONL output. Tier transitions
    // (tier1 miss, tier2 outcomes) are rare and informative, so they
    // stay at debug.
    if let Some(window) = current_tier1() {
        trace!(source = ?window.source, "live_window: tier1 (statusline cache) hit");
        return window;
    }
    debug!("live_window: tier1 miss/stale, falling through to tier2");
    if let Some(window) = current_tier2() {
        debug!(source = ?window.source, "live_window: tier2 (local JSONL) hit");
        return window;
    }
    debug!("live_window: tier2 miss, returning empty (CTA)");
    LiveWindow::empty()
}

/// Tier 1: read the statusline cache if fresh.
fn current_tier1() -> Option<LiveWindow> {
    let path = cache_path()?;
    let cache = read_cache(&path)?;
    if !cache_is_fresh(&cache) {
        return None;
    }
    Some(window_from_cache(cache))
}

fn cache_is_fresh(cache: &LiveCache) -> bool {
    let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&cache.updated_at) else {
        return false;
    };
    let age = Utc::now().signed_duration_since(updated.with_timezone(&Utc)).num_seconds();
    age >= 0 && (age as u64) <= CACHE_MAX_AGE_SECS
}

fn window_from_cache(cache: LiveCache) -> LiveWindow {
    // Pick the soonest reset (5h is almost always the visible one, but
    // honour 7d if 5h is missing).
    let five_hour_resets_at = cache
        .five_hour
        .as_ref()
        .and_then(|w| w.resets_at.as_deref())
        .and_then(instant_from_iso8601);
    let seven_day_resets_at = cache
        .seven_day
        .as_ref()
        .and_then(|w| w.resets_at.as_deref())
        .and_then(instant_from_iso8601);

    // Soonest reset, for the countdown-style `resets_in` consumers.
    let resets_in = cache
        .five_hour
        .as_ref()
        .and_then(|w| w.resets_at.as_deref())
        .and_then(duration_until_iso8601)
        .or_else(|| {
            cache
                .seven_day
                .as_ref()
                .and_then(|w| w.resets_at.as_deref())
                .and_then(duration_until_iso8601)
        });

    LiveWindow {
        five_hour_pct: cache.five_hour.as_ref().map(|w| w.pct),
        seven_day_pct: cache.seven_day.as_ref().map(|w| w.pct),
        today_cost_usd: cache.today_cost_usd,
        resets_in,
        five_hour_resets_at,
        seven_day_resets_at,
        context_pct: cache.context_pct,
        model: cache.model,
        source: Source::Tier1Cache,
    }
}

/// Parse an ISO8601 `resets_at` into an absolute UTC instant. Returns
/// `None` when the timestamp is unparseable. Unlike
/// [`duration_until_iso8601`], a past instant is preserved so callers can
/// decide how to render it — though the Tier 1 freshness gate (≤120s)
/// means the cache's reset instants are effectively always in the future.
pub fn instant_from_iso8601(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&Utc))
}

/// Convert an ISO8601 `resets_at` into a `Duration` from now. Returns
/// `None` if the timestamp is unparseable or already in the past.
pub fn duration_until_iso8601(s: &str) -> Option<Duration> {
    let parsed = DateTime::parse_from_rfc3339(s).ok()?;
    let secs = parsed.with_timezone(&Utc).signed_duration_since(Utc::now()).num_seconds();
    if secs <= 0 {
        None
    } else {
        Some(Duration::from_secs(secs as u64))
    }
}

/// Tier 2: native 5-hour-block aggregation over the local Claude JSONL
/// transcripts. Returns `None` when the user has no local JSONL data.
fn current_tier2() -> Option<LiveWindow> {
    let calls = collect_recent_claude_calls();
    if calls.is_empty() {
        return None;
    }
    let block = current_active_block(&calls, Utc::now())?;
    let limit = token_limit_from_env();
    let used = block.total_tokens;
    let pct = ((used as f64 / limit as f64) * 100.0).clamp(0.0, 100.0).round() as u8;

    Some(LiveWindow {
        five_hour_pct: Some(pct),
        seven_day_pct: None,
        today_cost_usd: block.total_cost_usd,
        resets_in: Some(time_until_block_end(&block, Utc::now())),
        // Checked add so a corrupt/extreme block start can never panic the
        // render path — on overflow it degrades to "no reset shown".
        five_hour_resets_at: block.start.checked_add_signed(chrono::Duration::hours(5)),
        // Tier 2 has only local data — the weekly window isn't derivable.
        seven_day_resets_at: None,
        context_pct: None,
        model: None,
        source: Source::Tier2Local,
    })
}

fn token_limit_from_env() -> u64 {
    std::env::var("AINB_TOKEN_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TOKEN_LIMIT)
}

/// 5-hour block descriptor (ccusage parity).
#[derive(Debug, Clone)]
pub struct ActiveBlock {
    pub start: DateTime<Utc>,
    pub last_entry: DateTime<Utc>,
    pub total_tokens: u64,
    pub total_cost_usd: Option<f64>,
}

/// Resolve the currently-active 5-hour block from a list of provider
/// calls. ccusage rules:
///   - Block start is the floor-to-UTC-hour of the first entry.
///   - The block ends when either:
///       (a) `now - last_entry >= 5h`, OR
///       (b) `entry.timestamp - block_start > 5h` (next call rolls over)
///
/// We walk calls in timestamp order, advancing block boundaries as
/// needed, and return the *currently-active* block (the one containing
/// `now`). Returns `None` when no block is currently active.
pub fn current_active_block(calls: &[ProviderCall], now: DateTime<Utc>) -> Option<ActiveBlock> {
    let mut sorted: Vec<&ProviderCall> = calls.iter().collect();
    sorted.sort_by_key(|c| c.timestamp);

    let mut block: Option<ActiveBlock> = None;
    for call in sorted {
        match block.as_mut() {
            None => {
                block = Some(ActiveBlock {
                    start: floor_to_hour(call.timestamp),
                    last_entry: call.timestamp,
                    total_tokens: token_total(call),
                    total_cost_usd: call.cost_usd,
                });
            }
            Some(b) => {
                let rolls_over = call.timestamp.signed_duration_since(b.start).num_hours() > 5;
                let stale = call.timestamp.signed_duration_since(b.last_entry).num_hours() >= 5;
                if rolls_over || stale {
                    *b = ActiveBlock {
                        start: floor_to_hour(call.timestamp),
                        last_entry: call.timestamp,
                        total_tokens: token_total(call),
                        total_cost_usd: call.cost_usd,
                    };
                } else {
                    b.last_entry = call.timestamp;
                    b.total_tokens = b.total_tokens.saturating_add(token_total(call));
                    b.total_cost_usd = match (b.total_cost_usd, call.cost_usd) {
                        (Some(a), Some(c)) => Some(a + c),
                        (Some(a), None) => Some(a),
                        (None, c) => c,
                    };
                }
            }
        }
    }

    let block = block?;
    // Block is active iff `now` falls within (start, start + 5h] AND
    // we've seen activity in the last 5h. Checked add so a corrupt/extreme
    // block start can never panic the render path; on overflow block.start
    // is already near the representable max, so `now` is always within it.
    let now_in_block = now > block.start
        && match block.start.checked_add_signed(chrono::Duration::hours(5)) {
            Some(block_end) => now <= block_end,
            None => true,
        };
    let recent_activity = now.signed_duration_since(block.last_entry).num_hours() < 5;
    if now_in_block && recent_activity {
        Some(block)
    } else {
        None
    }
}

fn token_total(c: &ProviderCall) -> u64 {
    c.input_tokens
        .saturating_add(c.output_tokens)
        .saturating_add(c.reasoning_tokens)
        .saturating_add(c.cache_creation_tokens)
        .saturating_add(c.cache_read_tokens)
}

fn floor_to_hour(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(t)
}

fn time_until_block_end(block: &ActiveBlock, now: DateTime<Utc>) -> Duration {
    // Checked add so a corrupt/extreme block start can never panic the
    // render path; overflow falls back to ZERO (no countdown shown).
    let Some(end) = block.start.checked_add_signed(chrono::Duration::hours(5)) else {
        return Duration::ZERO;
    };
    let secs = end.signed_duration_since(now).num_seconds();
    if secs <= 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(secs as u64)
    }
}

/// Pull recent Claude calls (last 5h) without re-parsing the entire
/// history. Goes through `collect_recent_claude_calls_within` which
/// mtime-filters the JSONL files and keeps only entries inside the
/// active 5-hour window — avoids the full-history walk that
/// `parse_usage_for` does on every render.
///
/// `MTIME_GRACE_HOURS` widens the mtime filter to forgive small clock
/// drift between the JSONL writer (Claude Code) and the filesystem.
const MTIME_GRACE_HOURS: i64 = 1;

fn collect_recent_claude_calls() -> Vec<ProviderCall> {
    let cutoff = Utc::now() - chrono::Duration::hours(5);
    let grace = chrono::Duration::hours(MTIME_GRACE_HOURS);
    crate::models::usage::collect_recent_claude_calls_within(cutoff, grace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(year: i32, mo: u32, d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, mo, d, h, m, 0).unwrap()
    }

    fn call_at(ts: DateTime<Utc>, tokens: u64) -> ProviderCall {
        ProviderCall {
            id: ts.timestamp() as u64,
            provider: "claude".to_string(),
            model: "test".to_string(),
            session_id: "s".to_string(),
            project: "p".to_string(),
            project_path: "/p".to_string(),
            timestamp: ts,
            input_tokens: tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cost_usd: Some(0.01),
            tools: Vec::new(),
            bash_commands: Vec::new(),
            user_message: String::new(),
            branch: None,
        }
    }

    #[test]
    fn empty_calls_returns_no_active_block() {
        let now = t(2026, 5, 7, 10, 0);
        assert!(current_active_block(&[], now).is_none());
    }

    #[test]
    fn single_recent_call_yields_active_block() {
        let now = t(2026, 5, 7, 10, 30);
        let calls = vec![call_at(t(2026, 5, 7, 10, 0), 1000)];
        let block = current_active_block(&calls, now).expect("block should be active");
        assert_eq!(block.total_tokens, 1000);
        assert_eq!(block.start, t(2026, 5, 7, 10, 0));
    }

    #[test]
    fn old_call_yields_no_active_block() {
        // Last entry was 6h ago; should be inactive.
        let now = t(2026, 5, 7, 16, 0);
        let calls = vec![call_at(t(2026, 5, 7, 10, 0), 1000)];
        assert!(current_active_block(&calls, now).is_none());
    }

    #[test]
    fn block_rolls_over_after_5h_window() {
        // Call at 10:00 starts block; call at 16:00 (>5h later) starts new block.
        let now = t(2026, 5, 7, 16, 30);
        let calls = vec![
            call_at(t(2026, 5, 7, 10, 0), 1000),
            call_at(t(2026, 5, 7, 16, 0), 500),
        ];
        let block = current_active_block(&calls, now).expect("active block");
        assert_eq!(block.total_tokens, 500, "only second call counts");
        assert_eq!(block.start, t(2026, 5, 7, 16, 0));
    }

    #[test]
    fn calls_within_5h_aggregate() {
        let now = t(2026, 5, 7, 12, 0);
        let calls = vec![
            call_at(t(2026, 5, 7, 10, 0), 500),
            call_at(t(2026, 5, 7, 10, 30), 700),
            call_at(t(2026, 5, 7, 11, 30), 800),
        ];
        let block = current_active_block(&calls, now).expect("active block");
        assert_eq!(block.total_tokens, 2000);
    }

    #[test]
    fn duration_until_iso8601_handles_future_and_past() {
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let d = duration_until_iso8601(&future).expect("future");
        assert!(d.as_secs() > 0);

        let past = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert!(duration_until_iso8601(&past).is_none());

        assert!(duration_until_iso8601("not a date").is_none());
    }

    #[test]
    fn cache_is_fresh_within_window() {
        let cache = LiveCache {
            version: crate::cli::statusline::CACHE_SCHEMA_VERSION,
            updated_at: Utc::now().to_rfc3339(),
            five_hour: None,
            seven_day: None,
            today_cost_usd: None,
            context_pct: None,
            model: None,
        };
        assert!(cache_is_fresh(&cache));
    }

    #[test]
    fn cache_is_stale_after_window() {
        let old =
            (Utc::now() - chrono::Duration::seconds(CACHE_MAX_AGE_SECS as i64 + 30)).to_rfc3339();
        let cache = LiveCache {
            version: crate::cli::statusline::CACHE_SCHEMA_VERSION,
            updated_at: old,
            five_hour: None,
            seven_day: None,
            today_cost_usd: None,
            context_pct: None,
            model: None,
        };
        assert!(!cache_is_fresh(&cache));
    }

    // Env-var tests share process state — fold both checks into one
    // serial test to avoid races with other tests on the same var.
    #[test]
    fn token_limit_env_var_round_trip() {
        // Default (no var set)
        std::env::remove_var("AINB_TOKEN_LIMIT");
        assert_eq!(token_limit_from_env(), DEFAULT_TOKEN_LIMIT);

        // Valid value
        std::env::set_var("AINB_TOKEN_LIMIT", "42");
        assert_eq!(token_limit_from_env(), 42);

        // Invalid value falls back
        std::env::set_var("AINB_TOKEN_LIMIT", "not a number");
        assert_eq!(token_limit_from_env(), DEFAULT_TOKEN_LIMIT);

        // Zero value is treated as unset
        std::env::set_var("AINB_TOKEN_LIMIT", "0");
        assert_eq!(token_limit_from_env(), DEFAULT_TOKEN_LIMIT);

        std::env::remove_var("AINB_TOKEN_LIMIT");
    }

    #[test]
    fn window_from_cache_populates_all_optional_fields() {
        use crate::cli::statusline::RateWindow;
        let future = (Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let cache = LiveCache {
            version: crate::cli::statusline::CACHE_SCHEMA_VERSION,
            updated_at: Utc::now().to_rfc3339(),
            five_hour: Some(RateWindow {
                pct: 30,
                resets_at: Some(future),
            }),
            seven_day: Some(RateWindow {
                pct: 5,
                resets_at: None,
            }),
            today_cost_usd: Some(2.5),
            context_pct: Some(40),
            model: Some("Opus".to_string()),
        };
        let w = window_from_cache(cache);
        assert_eq!(w.source, Source::Tier1Cache);
        assert_eq!(w.five_hour_pct, Some(30));
        assert_eq!(w.seven_day_pct, Some(5));
        assert_eq!(w.today_cost_usd, Some(2.5));
        assert_eq!(w.context_pct, Some(40));
        assert_eq!(w.model.as_deref(), Some("Opus"));
        assert!(w.resets_in.unwrap().as_secs() > 0);
        // 5h had a resets_at → absolute instant in the future; 7d didn't.
        let five = w.five_hour_resets_at.expect("5h reset instant");
        assert!(five > Utc::now());
        assert!(w.seven_day_resets_at.is_none());
    }

    #[test]
    fn instant_from_iso8601_parses_and_rejects_garbage() {
        let ts = "2026-06-08T05:00:00Z";
        let parsed = instant_from_iso8601(ts).expect("valid rfc3339");
        assert_eq!(parsed, Utc.with_ymd_and_hms(2026, 6, 8, 5, 0, 0).unwrap());
        assert!(instant_from_iso8601("not a date").is_none());
    }

    #[test]
    fn time_until_block_end_does_not_panic_on_max_instant() {
        // A corrupt/extreme block start at the top of chrono's range would
        // overflow `+ 5h`; the checked add must fall back to ZERO, not panic
        // the render path.
        let start = DateTime::<Utc>::from_naive_utc_and_offset(chrono::NaiveDateTime::MAX, Utc);
        let block = ActiveBlock {
            start,
            last_entry: start,
            total_tokens: 0,
            total_cost_usd: None,
        };
        assert_eq!(time_until_block_end(&block, Utc::now()), Duration::ZERO);
        // current_active_block walks calls, but the same near-max instant
        // must not panic its block-end check either.
        let call = call_at(start, 1000);
        let _ = current_active_block(&[call], Utc::now());
    }
}
