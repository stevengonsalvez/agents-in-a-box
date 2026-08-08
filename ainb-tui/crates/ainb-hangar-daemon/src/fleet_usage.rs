//! Bounded, daemon-owned Fleet Usage projection.
//!
//! Provider logs and canonical model-rate parsing stay behind this module. The
//! public RPC receives only aggregates, never paths, transcripts, or calls.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration as StdDuration;

use ainb_hangar_proto::fleet::{
    FLEET_USAGE_MAX_BREAKDOWN_BUCKETS, FLEET_USAGE_MAX_DAILY_BUCKETS, FleetUsageBucket,
    FleetUsageDailyBucket, FleetUsageModelBucket, FleetUsagePeriod, FleetUsageProjectBucket,
    FleetUsageProviderBucket, FleetUsageSummaryResult, FleetUsageSummaryState,
};
use ainb_plugin_session_reader::scanner::{self, ProviderRoots};
use ainb_plugin_types_sessions::TokenBucket;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_VERSION: u32 = 1;
const REFRESH_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);

/// Floor between the START of one scan and the start of the next.
///
/// [`UsageService::request_refresh`] already coalesces CONCURRENT callers via
/// `State::refreshing`, but nothing stopped SEQUENTIAL ones: the moment a scan
/// finished, the next queued caller started another. That matters because
/// `attention_ingest` calls this once per hook line (`attention_ingest.rs`, in
/// the per-line loop of `ingest_once`), and each scan re-parses every provider
/// transcript touched in the last 30 days. Hook lines arrive faster than that
/// completes, so the daemon scanned continuously and its RSS sawtoothed.
///
/// Equal to [`REFRESH_INTERVAL`]: the projection refreshes at most as often as
/// its own poll cadence, and a hook can bring a refresh FORWARD to that cadence
/// but never beat it.
///
/// A shorter floor was tried first (5 min) and measured: it fixed the frequency
/// but each scan still peaked the daemon at 2,305 MB, so twelve spikes an hour
/// became four. That per-scan cost is no longer this constant's problem — the
/// scan no longer materialises a `Vec<ProviderCall>` for the whole corpus, it
/// folds each file into the three windows and drops it
/// (`scanner::scan_windows`), which measured 1,747 MB down to ~720 MB.
///
/// The floor still earns its keep even at that price. A ~12 s scan four times an
/// hour is cheap; the same scan once per hook line is not, and hook lines are
/// what `attention_ingest` delivers. This now paces the scan rather than
/// bounding its damage.
const MIN_REFRESH_GAP: StdDuration = REFRESH_INTERVAL;

/// How long a FAILED scan holds the floor.
///
/// [`MIN_REFRESH_GAP`] paces SUCCESSFUL scans. A failure must not buy the same
/// silence: the projection is already stale, the fault is usually transient (an
/// unreadable provider root, a momentary IO error), and holding the full gap
/// leaves the RPC answering `Partial` for a quarter of an hour over something
/// that would have cleared on the next attempt.
///
/// Not zero, though. A scan that fails FAST and retries freely is its own hot
/// loop — the exact failure mode [`MIN_REFRESH_GAP`] exists to prevent — so a
/// failure backs off briefly rather than not at all.
const FAILURE_RETRY_GAP: StdDuration = StdDuration::from_secs(30);

/// [`MIN_REFRESH_GAP`] in ms, overridable with `AINB_FLEET_USAGE_MIN_GAP_MS` so
/// a test can drive several gaps inside its budget. Mirrors the override on the
/// ownership watchdog and `HANGAR_DAEMON_POLL_MS`.
fn min_refresh_gap_ms() -> i64 {
    std::env::var("AINB_FLEET_USAGE_MIN_GAP_MS")
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|ms| *ms >= 0)
        .unwrap_or_else(|| i64::try_from(MIN_REFRESH_GAP.as_millis()).unwrap_or(i64::MAX))
}

/// [`FAILURE_RETRY_GAP`] in ms.
fn failure_retry_gap_ms() -> i64 {
    i64::try_from(FAILURE_RETRY_GAP.as_millis()).unwrap_or(i64::MAX)
}

/// Durable, bounded snapshot. Raw provider calls never leave the scan worker
/// or land in this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSummaries {
    version: u32,
    summaries: Vec<CachedSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSummary {
    period: FleetUsagePeriod,
    summary: FleetUsageSummaryResult,
}

#[derive(Default)]
struct State {
    refreshing: bool,
    last_error: Option<String>,
    /// Earliest epoch-ms at which another scan may START.
    ///
    /// Stored as a deadline rather than as "when the last scan started" because
    /// the two answers differ: a success pushes it out by [`MIN_REFRESH_GAP`],
    /// a failure pulls it back in to [`FAILURE_RETRY_GAP`]. Measured from
    /// start, not finish, so a scan that outruns the gap does not earn an
    /// immediate re-run the instant it lands.
    next_allowed_at: Option<i64>,
}

impl State {
    /// May another scan start at `now`?
    fn may_start(&self, now: i64) -> bool {
        !self.refreshing && self.next_allowed_at.is_none_or(|at| now >= at)
    }

    /// A scan just started: hold the floor for the full pacing gap.
    fn pace_after_start(&mut self, now: i64) {
        self.next_allowed_at = Some(now.saturating_add(min_refresh_gap_ms()));
    }

    /// A scan just failed: let the next attempt come much sooner.
    ///
    /// Takes the EARLIER of the two deadlines, so a failure can only ever bring
    /// the next attempt forward. Without that guard a slow failure would push
    /// the deadline further out than the success path had already set, turning
    /// a fault into extra staleness.
    fn allow_retry_after_failure(&mut self, now: i64) {
        let retry_at = now.saturating_add(failure_retry_gap_ms());
        self.next_allowed_at =
            Some(self.next_allowed_at.map_or(retry_at, |scheduled| scheduled.min(retry_at)));
    }
}

/// One daemon-owned scanner shared by every Fleet connection.
///
/// The service serves its last complete snapshot immediately, runs at most one
/// refresh at a time, and persists only public projections under Hangar home.
pub struct UsageService {
    cache_path: PathBuf,
    cached: Mutex<Option<CachedSummaries>>,
    state: Mutex<State>,
}

impl UsageService {
    /// Load a durable snapshot, if one exists. A caller must invoke
    /// [`Self::request_refresh`] after installation to warm it in the background.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        let cache_path = home.join("hangar").join("fleet-usage.json");
        Self {
            cached: Mutex::new(load_cache(&cache_path)),
            cache_path,
            state: Mutex::new(State::default()),
        }
    }

    async fn summary(self: &Arc<Self>, period: FleetUsagePeriod) -> FleetUsageSummaryResult {
        let (summary, should_refresh) = {
            let state = self.state.lock().await;
            let cached = self.cached.lock().await;
            let summary = cached
                .as_ref()
                .and_then(|cache| cache.summaries.iter().find(|row| row.period == period))
                .map(|row| {
                    let mut summary = row.summary.clone();
                    if state.refreshing || state.last_error.is_some() {
                        summary.state = FleetUsageSummaryState::Partial;
                        summary.detail = Some(match &state.last_error {
                            Some(error) => format!("using last complete snapshot: {error}"),
                            None => "refreshing usage in background".to_string(),
                        });
                    }
                    summary
                });
            let generated_at = summary.as_ref().and_then(|row| row.generated_at).unwrap_or(0);
            let age_ms = Utc::now().timestamp_millis().saturating_sub(generated_at);
            (
                summary,
                generated_at == 0 || age_ms >= REFRESH_INTERVAL.as_millis() as i64,
            )
        };
        if should_refresh {
            self.request_refresh().await;
            if let Some(mut summary) = summary {
                summary.state = FleetUsageSummaryState::Partial;
                summary.detail = Some(match summary.detail {
                    Some(detail) => format!("{detail}; refreshing usage in background"),
                    None => "refreshing usage in background".to_string(),
                });
                return summary;
            }
        }
        summary.unwrap_or_else(|| {
            let state = self.state.try_lock().ok();
            match state.and_then(|state| state.last_error.clone()) {
                Some(error) => unavailable(error),
                None => scanning(),
            }
        })
    }

    /// Coalesce concurrent refresh requests into one background worker, and
    /// pace sequential ones — [`MIN_REFRESH_GAP`] after a success,
    /// [`FAILURE_RETRY_GAP`] after a failure.
    pub async fn request_refresh(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().await;
            let now = Utc::now().timestamp_millis();
            if !state.may_start(now) {
                return;
            }
            state.refreshing = true;
            state.pace_after_start(now);
            state.last_error = None;
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let scanned = tokio::task::spawn_blocking(scan_all_summaries).await;
            let result = scanned
                .map_err(|error| format!("usage worker failed: {error}"))
                .and_then(|result| result);
            let mut state = service.state.lock().await;
            state.refreshing = false;
            let failure = match result {
                Ok(cached) => match write_cache(&service.cache_path, &cached) {
                    Ok(()) => {
                        *service.cached.lock().await = Some(cached);
                        None
                    }
                    Err(error) => Some(format!("could not persist usage snapshot: {error}")),
                },
                Err(error) => Some(error),
            };
            if let Some(error) = failure {
                state.last_error = Some(error);
                state.allow_retry_after_failure(Utc::now().timestamp_millis());
            }
        });
    }

    fn spawn_poller(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(service) = weak.upgrade() else {
                    break;
                };
                service.request_refresh().await;
            }
        });
    }
}

static ACTIVE_SERVICE: OnceLock<tokio::sync::RwLock<Option<Arc<UsageService>>>> = OnceLock::new();

fn active_slot() -> &'static tokio::sync::RwLock<Option<Arc<UsageService>>> {
    ACTIVE_SERVICE.get_or_init(|| tokio::sync::RwLock::new(None))
}

/// Install the process-wide service before the RPC listener opens.
pub async fn install(home: &Path) {
    let service = Arc::new(UsageService::new(home));
    service.request_refresh().await;
    service.spawn_poller();
    *active_slot().write().await = Some(service);
}

/// Ask the scanner to catch up after a provider hook event.
pub async fn request_refresh() {
    if let Some(service) = active_slot().read().await.clone() {
        service.request_refresh().await;
    }
}

/// Read a cached projection without making RPC clients wait for filesystem work.
pub async fn summary(period: FleetUsagePeriod) -> FleetUsageSummaryResult {
    match active_slot().read().await.clone() {
        Some(service) => service.summary(period).await,
        None => unavailable("usage service is not initialized".to_string()),
    }
}

fn load_cache(path: &Path) -> Option<CachedSummaries> {
    let bytes = std::fs::read(path).ok()?;
    let cached: CachedSummaries = serde_json::from_slice(&bytes).ok()?;
    (cached.version == CACHE_VERSION).then_some(cached)
}

fn write_cache(path: &Path, cached: &CachedSummaries) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::other("usage cache has no parent directory"));
    };
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec(cached).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

fn scanning() -> FleetUsageSummaryResult {
    FleetUsageSummaryResult {
        state: FleetUsageSummaryState::Scanning,
        generated_at: None,
        start_at: None,
        end_at: None,
        totals: None,
        daily: Vec::new(),
        providers: Vec::new(),
        models: Vec::new(),
        projects: Vec::new(),
        detail: Some("building usage snapshot".to_string()),
    }
}

fn unavailable(detail: String) -> FleetUsageSummaryResult {
    FleetUsageSummaryResult {
        state: FleetUsageSummaryState::Unavailable,
        generated_at: None,
        start_at: None,
        end_at: None,
        totals: None,
        daily: Vec::new(),
        providers: Vec::new(),
        models: Vec::new(),
        projects: Vec::new(),
        detail: Some(detail),
    }
}

/// Every window the public contract exposes. `scan_all_summaries` zips this
/// against the scanner's results, so the order is load-bearing.
const PERIODS: [FleetUsagePeriod; 3] = [
    FleetUsagePeriod::Today,
    FleetUsagePeriod::Trailing7Days,
    FleetUsagePeriod::Trailing30Days,
];

/// Scan canonical provider histories once, then derive every public window.
///
/// The scanner owns provider parsing and rate lookup. This service stores only
/// the bounded projections, never provider calls or their local paths.
fn scan_all_summaries() -> Result<CachedSummaries, String> {
    let roots = ProviderRoots::defaults();
    if roots.claude_projects.is_none()
        && roots.codex_sessions.is_none()
        && roots.gemini_sessions.is_none()
        && roots.copilot_sessions.is_none()
        && roots.cursor_sessions.is_none()
    {
        return Err("provider history roots are unavailable".to_string());
    }
    let now = Utc::now();
    let windows: Vec<scanner::UsageWindow> = PERIODS
        .iter()
        .map(|period| {
            let (start, end) = window(*period, now);
            scanner::UsageWindow { start, end }
        })
        .collect();
    let scanned = scanner::scan_windows(&roots, &windows);
    if scanned.len() != PERIODS.len() {
        return Err("usage scan returned the wrong number of windows".to_string());
    }
    Ok(CachedSummaries {
        version: CACHE_VERSION,
        summaries: PERIODS
            .iter()
            .zip(scanned)
            .map(|(period, usage)| CachedSummary {
                period: *period,
                summary: summary_from_window(&usage, *period, now),
            })
            .collect(),
    })
}

/// Project one scanned window onto the public contract.
///
/// Every bucket arrives pre-aggregated from the scanner, so this only
/// renames, ranks and caps. `complete_cost` travels alongside each bucket
/// because cost coalesces during accumulation: a `Some` cost does not mean
/// every call behind it was priced (see `scanner::UsageRow`).
fn summary_from_window(
    usage: &scanner::WindowUsage,
    period: FleetUsagePeriod,
    now: DateTime<Utc>,
) -> FleetUsageSummaryResult {
    let (start, end) = window(period, now);

    let mut daily: Vec<_> = usage
        .daily
        .iter()
        .map(|row| FleetUsageDailyBucket {
            date: row.key.clone(),
            bucket: bucket_from(&row.bucket, row.complete_cost),
        })
        .collect();
    daily.truncate(FLEET_USAGE_MAX_DAILY_BUCKETS);

    let mut providers: Vec<_> = usage
        .providers
        .iter()
        .map(|row| FleetUsageProviderBucket {
            bucket: bucket_from(&row.bucket, row.complete_cost),
            provider: row.key.clone(),
        })
        .collect();
    sort_and_cap(&mut providers, |row| &row.bucket);

    let mut models: Vec<_> = usage
        .models
        .iter()
        .map(|row| FleetUsageModelBucket {
            bucket: bucket_from(&row.bucket, row.complete_cost),
            model: row.key.clone(),
        })
        .collect();
    sort_and_cap(&mut models, |row| &row.bucket);

    let mut projects: Vec<_> = usage
        .projects
        .iter()
        .map(|row| FleetUsageProjectBucket {
            bucket: bucket_from(&row.bucket, row.complete_cost),
            project: row.key.clone(),
            repo: None,
        })
        .collect();
    sort_and_cap(&mut projects, |row| &row.bucket);

    FleetUsageSummaryResult {
        state: FleetUsageSummaryState::Ready,
        generated_at: Some(now.timestamp_millis()),
        start_at: Some(start.timestamp_millis()),
        end_at: Some(end.timestamp_millis()),
        totals: Some(bucket_from(&usage.totals, usage.totals_complete_cost)),
        daily,
        providers,
        models,
        projects,
        detail: None,
    }
}

fn window(period: FleetUsagePeriod, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let end_day = now.date_naive().succ_opt().expect("valid UTC date");
    let days = match period {
        FleetUsagePeriod::Today => 1,
        FleetUsagePeriod::Trailing7Days => 7,
        FleetUsagePeriod::Trailing30Days => 30,
    };
    let start_day = end_day - Duration::days(days);
    (
        start_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc(),
        end_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc(),
    )
}

fn bucket_from(bucket: &TokenBucket, complete_cost: bool) -> FleetUsageBucket {
    FleetUsageBucket {
        input_tokens: bucket.input_tokens,
        cache_creation_tokens: bucket.cache_creation_tokens,
        cache_read_tokens: bucket.cache_read_tokens,
        output_tokens: bucket.output_tokens,
        reasoning_tokens: bucket.reasoning_tokens,
        call_count: u64::try_from(bucket.call_count).unwrap_or(u64::MAX),
        session_count: u64::try_from(bucket.session_count).unwrap_or(u64::MAX),
        project_count: u64::try_from(bucket.project_count).unwrap_or(u64::MAX),
        cost_usd: complete_cost.then_some(bucket.cost_usd).flatten(),
    }
}

fn sort_and_cap<T>(rows: &mut Vec<T>, bucket: impl Fn(&T) -> &FleetUsageBucket) {
    rows.sort_by(|left, right| {
        let left_total = bucket(left).input_tokens
            + bucket(left).cache_creation_tokens
            + bucket(left).cache_read_tokens
            + bucket(left).output_tokens
            + bucket(left).reasoning_tokens;
        let right_total = bucket(right).input_tokens
            + bucket(right).cache_creation_tokens
            + bucket(right).cache_read_tokens
            + bucket(right).output_tokens
            + bucket(right).reasoning_tokens;
        right_total.cmp(&left_total)
    });
    rows.truncate(FLEET_USAGE_MAX_BREAKDOWN_BUCKETS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_windows_are_bounded_and_utc_aligned() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let (start, end) = window(FleetUsagePeriod::Trailing7Days, now);
        assert_eq!(start.to_rfc3339(), "2026-07-31T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-08-07T00:00:00+00:00");
    }

    /// The floor may equal the poll cadence but must never EXCEED it: a longer
    /// floor would throttle the poller itself, so the projection would go stale
    /// on its own schedule and the `Partial` state would become permanent.
    #[test]
    fn the_refresh_floor_never_outlasts_the_poll_interval() {
        assert!(
            MIN_REFRESH_GAP <= REFRESH_INTERVAL,
            "a floor longer than the poll interval starves the refresh it is meant to pace"
        );
        assert_eq!(
            min_refresh_gap_ms(),
            15 * 60 * 1000,
            "floor tracks the 15-minute cadence"
        );
    }

    /// The gap is measured from scan START. Measuring from finish would let a
    /// scan that outruns the gap earn an immediate re-run the moment it lands,
    /// which is the back-to-back behaviour being removed.
    #[tokio::test]
    async fn a_second_request_inside_the_gap_does_not_start_another_scan() {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(UsageService::new(dir.path()));

        // Simulate a scan that already started "now" and finished immediately.
        {
            let mut state = service.state.lock().await;
            state.pace_after_start(Utc::now().timestamp_millis());
            state.refreshing = false;
        }

        service.request_refresh().await;

        let state = service.state.lock().await;
        assert!(
            !state.refreshing,
            "a request inside the floor must be dropped, not queued behind the last one"
        );
    }

    /// A transient scan failure must not buy the full pacing gap of silence.
    /// Before this, one unreadable provider root locked out every refresh —
    /// including the poller — for 15 minutes, so the RPC answered `Partial`
    /// for a quarter hour over a fault that the next attempt would have
    /// cleared.
    #[tokio::test]
    async fn a_failed_scan_is_retryable_long_before_the_pacing_gap() {
        let started = 1_000_000_000_000i64;
        let mut state = State {
            refreshing: true,
            ..State::default()
        };
        state.pace_after_start(started);

        // The scan lands as a failure a moment later.
        let failed_at = started + 250;
        state.refreshing = false;
        state.allow_retry_after_failure(failed_at);

        assert!(
            !state.may_start(failed_at),
            "a failure must still back off briefly, or a fast-failing scan hot-loops"
        );
        assert!(
            state.may_start(failed_at + failure_retry_gap_ms()),
            "a failed scan must be retryable once the short backoff elapses"
        );
        assert!(
            failed_at + failure_retry_gap_ms() < started + min_refresh_gap_ms(),
            "the retry must land well before the pacing gap it replaced"
        );
    }

    /// The mirror of the above: SUCCESS must still be paced. A fix that made
    /// failures retryable by clearing the schedule outright would also let a
    /// successful scan re-run immediately, reinstating the storm.
    #[tokio::test]
    async fn a_successful_scan_still_holds_the_full_gap() {
        let started = 1_000_000_000_000i64;
        let mut state = State::default();
        state.pace_after_start(started);
        state.refreshing = false;

        assert!(
            !state.may_start(started + failure_retry_gap_ms()),
            "not after the short gap"
        );
        assert!(
            !state.may_start(started + min_refresh_gap_ms() - 1),
            "not one ms early"
        );
        assert!(
            state.may_start(started + min_refresh_gap_ms()),
            "yes once the gap elapses"
        );
    }

    /// A failure may only bring the next attempt FORWARD. A scan that fails
    /// slowly must not push the deadline past what the success path already
    /// scheduled, or a fault would cost extra staleness instead of less.
    #[tokio::test]
    async fn a_slow_failure_never_delays_the_next_attempt() {
        let started = 1_000_000_000_000i64;
        let mut state = State::default();
        state.pace_after_start(started);
        let scheduled = state.next_allowed_at.expect("paced");

        // Fails after almost the whole gap has already elapsed.
        state.allow_retry_after_failure(started + min_refresh_gap_ms() - 1);

        assert_eq!(
            state.next_allowed_at,
            Some(scheduled),
            "a late failure must not push the deadline out"
        );
    }

    /// The backoff only helps if it is much shorter than the gap it replaces.
    #[test]
    fn the_failure_backoff_is_shorter_than_the_pacing_gap() {
        assert!(
            FAILURE_RETRY_GAP < MIN_REFRESH_GAP,
            "a failure backoff at or above the pacing gap would fix nothing"
        );
    }

    fn row(key: &str, input: u64, cost: Option<f64>, complete_cost: bool) -> scanner::UsageRow {
        scanner::UsageRow {
            key: key.to_string(),
            bucket: TokenBucket {
                input_tokens: input,
                cost_usd: cost,
                ..TokenBucket::default()
            },
            complete_cost,
        }
    }

    fn window_usage(rows: Vec<scanner::UsageRow>) -> scanner::WindowUsage {
        scanner::WindowUsage {
            totals: TokenBucket {
                input_tokens: rows.iter().map(|r| r.bucket.input_tokens).sum(),
                ..TokenBucket::default()
            },
            totals_complete_cost: rows.iter().all(|r| r.complete_cost),
            daily: Vec::new(),
            models: rows.clone(),
            projects: rows.clone(),
            providers: rows,
        }
    }

    /// Each period must be stamped with its OWN bounds. `scan_all_summaries`
    /// zips a window list against `PERIODS`, so a misalignment would publish
    /// 30 days of data under the `Today` label — numerically plausible and
    /// therefore invisible without this pin.
    #[test]
    fn a_summary_is_stamped_with_its_own_period_bounds() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        for period in [
            FleetUsagePeriod::Today,
            FleetUsagePeriod::Trailing7Days,
            FleetUsagePeriod::Trailing30Days,
        ] {
            let (start, end) = window(period, now);
            let summary = summary_from_window(&window_usage(Vec::new()), period, now);
            assert_eq!(
                summary.start_at,
                Some(start.timestamp_millis()),
                "{period:?} start"
            );
            assert_eq!(
                summary.end_at,
                Some(end.timestamp_millis()),
                "{period:?} end"
            );
        }
    }

    /// A bucket whose calls were not all priced must publish no cost at all,
    /// on every breakdown as well as the totals. Publishing the partial sum
    /// would understate spend without saying so.
    #[test]
    fn a_partially_priced_window_publishes_no_cost_anywhere() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let usage = window_usage(vec![
            row("priced", 10, Some(1.0), true),
            row("unpriced", 20, Some(2.0), false),
        ]);
        let summary = summary_from_window(&usage, FleetUsagePeriod::Today, now);

        assert_eq!(summary.totals.as_ref().unwrap().cost_usd, None, "totals");
        assert_eq!(
            summary.totals.as_ref().unwrap().input_tokens,
            30,
            "tokens survive"
        );
        for model in &summary.models {
            let expected = (model.model == "priced").then_some(1.0);
            assert_eq!(model.bucket.cost_usd, expected, "model {}", model.model);
        }
        for provider in &summary.providers {
            let expected = (provider.provider == "priced").then_some(1.0);
            assert_eq!(
                provider.bucket.cost_usd, expected,
                "provider {}",
                provider.provider
            );
        }
    }

    /// Breakdowns are ranked and capped so one busy host cannot make the RPC
    /// response unbounded.
    #[test]
    fn breakdowns_are_ranked_and_capped() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let rows: Vec<_> = (0..(FLEET_USAGE_MAX_BREAKDOWN_BUCKETS as u64 + 5))
            .map(|i| row(&format!("k{i}"), i, Some(1.0), true))
            .collect();
        let summary = summary_from_window(&window_usage(rows), FleetUsagePeriod::Today, now);

        assert_eq!(
            summary.models.len(),
            FLEET_USAGE_MAX_BREAKDOWN_BUCKETS,
            "capped"
        );
        let tokens: Vec<u64> = summary.models.iter().map(|m| m.bucket.input_tokens).collect();
        let mut sorted = tokens.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(
            tokens, sorted,
            "rows must be ranked before the cap drops the tail"
        );
        assert!(
            tokens.iter().all(|t| *t >= 5),
            "the cap must drop the SMALLEST rows, not an arbitrary slice"
        );
    }

    #[test]
    fn unknown_cost_never_becomes_zero() {
        let bucket = TokenBucket {
            input_tokens: 10,
            cost_usd: Some(1.25),
            ..TokenBucket::default()
        };
        assert_eq!(bucket_from(&bucket, false).cost_usd, None);
        assert_eq!(bucket_from(&bucket, false).input_tokens, 10);
    }
}
