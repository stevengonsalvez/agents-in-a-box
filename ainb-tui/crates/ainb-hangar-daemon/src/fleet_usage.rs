//! Bounded, daemon-owned Fleet Usage projection.
//!
//! Provider logs and canonical model-rate parsing stay behind this module. The
//! public RPC receives only aggregates, never paths, transcripts, or calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration as StdDuration;

use ainb_hangar_proto::fleet::{
    FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS, FLEET_DASHBOARD_MAX_HEATMAP_CELLS,
    FLEET_DASHBOARD_MAX_WEEKLY_BUCKETS, FLEET_USAGE_MAX_BREAKDOWN_BUCKETS,
    FLEET_USAGE_MAX_DAILY_BUCKETS, FleetHeatmapCell, FleetUsageBranchBucket, FleetUsageBucket,
    FleetUsageDailyBucket, FleetUsageDashboardResult, FleetUsageForecast, FleetUsageModelBucket,
    FleetUsageNamedBucket, FleetUsagePeriod, FleetUsageProjectBucket, FleetUsageProviderBucket,
    FleetUsageSessionBucket, FleetUsageSummaryResult, FleetUsageSummaryState,
    FleetUsageWeeklyBucket,
};
use ainb_plugin_session_reader::scanner::{self, ProviderRoots};
use ainb_plugin_types_sessions::{NamedUsage, TokenBucket};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_VERSION: u32 = 2;
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
    #[serde(default)]
    dashboard: Option<FleetUsageDashboardResult>,
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

    async fn dashboard(self: &Arc<Self>) -> FleetUsageDashboardResult {
        let (dashboard, should_refresh) = {
            let state = self.state.lock().await;
            let cached = self.cached.lock().await;
            let dashboard = cached.as_ref().and_then(|cache| {
                let mut d = cache.dashboard.clone()?;
                if state.refreshing || state.last_error.is_some() {
                    d.state = FleetUsageSummaryState::Partial;
                    d.detail = Some(match &state.last_error {
                        Some(error) => format!("using last complete snapshot: {error}"),
                        None => "refreshing usage in background".to_string(),
                    });
                }
                Some(d)
            });
            let generated_at = dashboard.as_ref().and_then(|d| d.generated_at).unwrap_or(0);
            let age_ms = Utc::now().timestamp_millis().saturating_sub(generated_at);
            (
                dashboard,
                generated_at == 0 || age_ms >= REFRESH_INTERVAL.as_millis() as i64,
            )
        };
        if should_refresh {
            self.request_refresh().await;
            if let Some(mut d) = dashboard {
                d.state = FleetUsageSummaryState::Partial;
                d.detail = Some(match d.detail {
                    Some(detail) => format!("{detail}; refreshing usage in background"),
                    None => "refreshing usage in background".to_string(),
                });
                return d;
            }
        }
        dashboard.unwrap_or_else(|| {
            let state = self.state.try_lock().ok();
            match state.and_then(|state| state.last_error.clone()) {
                Some(error) => dashboard_unavailable(error),
                None => dashboard_scanning(),
            }
        })
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

/// Read the cached rich dashboard projection.
pub async fn dashboard() -> FleetUsageDashboardResult {
    match active_slot().read().await.clone() {
        Some(service) => service.dashboard().await,
        None => dashboard_unavailable("usage service is not initialized".to_string()),
    }
}

fn load_cache(path: &Path) -> Option<CachedSummaries> {
    let bytes = std::fs::read(path).ok()?;
    let cached: CachedSummaries = serde_json::from_slice(&bytes).ok()?;
    // Accept OLDER caches, not just the current version. `CachedSummary` is
    // unchanged across the v1 to v2 bump and `dashboard` defaults to None, so a
    // v1 file still answers fleet/usage_summary correctly while the dashboard
    // fills in on the next refresh. Rejecting it would drop the stable endpoint
    // to SCANNING until a cold rescan of the whole corpus finished.
    (cached.version <= CACHE_VERSION).then_some(cached)
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

fn dashboard_scanning() -> FleetUsageDashboardResult {
    FleetUsageDashboardResult {
        state: FleetUsageSummaryState::Scanning,
        generated_at: None,
        start_at: None,
        end_at: None,
        cost_complete: false,
        totals: None,
        weekly: Vec::new(),
        heatmap: Vec::new(),
        forecast: None,
        providers: Vec::new(),
        models: Vec::new(),
        projects: Vec::new(),
        sessions: Vec::new(),
        branches: Vec::new(),
        tools: Vec::new(),
        mcp_servers: Vec::new(),
        shell_commands: Vec::new(),
        detail: Some("building usage dashboard".to_string()),
    }
}

fn dashboard_unavailable(detail: String) -> FleetUsageDashboardResult {
    FleetUsageDashboardResult {
        state: FleetUsageSummaryState::Unavailable,
        generated_at: None,
        start_at: None,
        end_at: None,
        cost_complete: false,
        totals: None,
        weekly: Vec::new(),
        heatmap: Vec::new(),
        forecast: None,
        providers: Vec::new(),
        models: Vec::new(),
        projects: Vec::new(),
        sessions: Vec::new(),
        branches: Vec::new(),
        tools: Vec::new(),
        mcp_servers: Vec::new(),
        shell_commands: Vec::new(),
        detail: Some(detail),
    }
}

/// Every summary window the public contract exposes. `scan_all_summaries`
/// zips this against the scanner's results, so the order is load-bearing.
const PERIODS: [FleetUsagePeriod; 3] = [
    FleetUsagePeriod::Today,
    FleetUsagePeriod::Trailing7Days,
    FleetUsagePeriod::Trailing30Days,
];

/// Number of days in the 53-week dashboard window.
const DASHBOARD_DAYS: i64 = 371;

/// Trailing window the 30-day forecast extrapolates from.
const TRAILING_FORECAST_DAYS: i64 = 7;

/// Every window one scan pass must produce: the three summary periods in
/// [`PERIODS`] order, then the 53-week dashboard.
///
/// `scan_windows` returns one result per window in the order given, and
/// `scan_all_summaries` pops the dashboard off the back and zips the rest
/// against `PERIODS`. Both halves of that depend on this order, and a
/// misalignment would publish 30 days of data under the `Today` label:
/// numerically plausible, and therefore invisible without a test.
///
/// One pass over four windows rather than two scans: `scan_windows` folds each
/// file into every window containing it and releases the calls immediately, so
/// the 371-day window costs more files READ but does not grow peak memory the
/// way materialising a year of `ProviderCall`s would.
fn scan_window_list(now: DateTime<Utc>) -> Vec<scanner::UsageWindow> {
    let mut windows: Vec<scanner::UsageWindow> = PERIODS
        .iter()
        .map(|period| {
            let (start, end) = window(*period, now);
            scanner::UsageWindow { start, end }
        })
        .collect();
    windows.push(dashboard_window(now));
    windows
}

/// The 53-week dashboard window, aligned to UTC midnight like [`window`].
fn dashboard_window(now: DateTime<Utc>) -> scanner::UsageWindow {
    let end_day = now.date_naive().succ_opt().expect("valid UTC date");
    let start_day = end_day - Duration::days(DASHBOARD_DAYS);
    scanner::UsageWindow {
        start: start_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc(),
        end: end_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc(),
    }
}

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
    let windows = scan_window_list(now);
    let mut scanned = scanner::scan_windows(&roots, &windows);
    if scanned.len() != windows.len() {
        return Err("usage scan returned the wrong number of windows".to_string());
    }
    let dashboard = scanned.pop().expect("length checked against a non-empty window list");
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
        dashboard: Some(dashboard_from_window(&dashboard, now)),
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

/// The MCP server a tool call belongs to, for `mcp__<server>__<tool>` names.
///
/// Returns `None` for an ordinary tool, which is what keeps `Read` and `Bash` in
/// the tool list.
///
/// BOTH segments must be present and non-empty. `mcp__github` and
/// `mcp__github__` name no tool, so they are ordinary strings rather than MCP
/// calls: treating them as MCP would delete a real tool from the tool list and
/// invent a server bucket that never made a call.
fn mcp_server_of(tool: &str) -> Option<String> {
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .filter(|(server, tool_name)| !server.is_empty() && !tool_name.is_empty())
        .map(|(server, _)| server.to_string())
}

/// Reduce a shell command line to just its program name.
///
/// The dashboard reports WHICH programs an operator runs, never the arguments
/// they ran them with. Arguments are where absolute paths and credentials live,
/// and this value reaches both the wire and a world-readable cache file.
///
/// Leading `VAR=value` assignments are skipped rather than reported, since an
/// inline `API_KEY=...` would otherwise become the bucket name and defeat the
/// whole point. A basename is taken so `/usr/local/bin/foo` and `foo` agree and
/// no install prefix leaks. Anything unrecognisable degrades to `"other"`
/// rather than falling back to the raw string.
fn program_name(command: &str) -> String {
    command
        .split_whitespace()
        .find(|token| !token.contains('='))
        .and_then(|token| token.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map_or_else(|| "other".to_string(), ToString::to_string)
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
    sort_and_cap_n(rows, FLEET_USAGE_MAX_BREAKDOWN_BUCKETS, bucket);
}

/// Cap a chronologically ASCENDING series, keeping the most recent `cap` rows.
///
/// `Vec::truncate` keeps the front, which on a time series means keeping the
/// oldest rows and discarding the present -- the opposite of what a dashboard
/// wants.
fn keep_newest<T>(rows: &mut Vec<T>, cap: usize) {
    if rows.len() > cap {
        rows.drain(..rows.len() - cap);
    }
}

fn sort_and_cap_n<T>(rows: &mut Vec<T>, cap: usize, bucket: impl Fn(&T) -> &FleetUsageBucket) {
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
    rows.truncate(cap);
}

// ---------------------------------------------------------------------------
// fleet/usage_dashboard projection
// ---------------------------------------------------------------------------

/// Project the 53-week window onto the dashboard contract.
///
/// Every row arrives pre-aggregated and pre-gated from `scanner::scan_windows`,
/// so this only renames, ranks and caps. Crucially, `complete_cost` travels ON
/// each row rather than being recomputed here from a call set: cost coalesces
/// during accumulation, so a `Some` cost never implies every call behind it was
/// priced, and the only place that distinction still exists is inside the
/// scanner's per-window AND-accumulator.
fn dashboard_from_window(
    usage: &scanner::WindowUsage,
    now: DateTime<Utc>,
) -> FleetUsageDashboardResult {
    let scanner::UsageWindow { start, end } = dashboard_window(now);
    let cost_complete = usage.totals_complete_cost;

    // Weekly buckets from the scanner's pre-computed weekly aggregates.
    //
    // Priced-ness is per week, not the whole-window flag: costs sum with None
    // coalescing, so a week containing one unpriced call would otherwise report
    // a partial sum as if it were the whole week's spend.
    let mut weekly: Vec<_> = usage
        .weekly
        .iter()
        .map(|row| FleetUsageWeeklyBucket {
            bucket: bucket_from(&row.bucket, row.complete_cost),
            week_start: row.key.clone(),
        })
        .collect();
    // Ascending by week (scanner keys off a BTreeMap), and a 371-day window
    // touches 54 ISO weeks whenever it does not open on a Monday. Cap from the
    // FRONT so the week we drop is the oldest, never the current one.
    keep_newest(&mut weekly, FLEET_DASHBOARD_MAX_WEEKLY_BUCKETS);

    // Heatmap: one cell per calendar day with call count and cost. The daily
    // rows arrive ascending and already gated, so a day holding an unpriced
    // call renders null rather than a partial sum.
    let mut heatmap: Vec<_> = usage
        .daily
        .iter()
        .map(|row| FleetHeatmapCell {
            date: row.key.clone(),
            call_count: u64::try_from(row.bucket.call_count).unwrap_or(u64::MAX),
            cost_usd: row.complete_cost.then_some(row.bucket.cost_usd).flatten(),
        })
        .collect();
    keep_newest(&mut heatmap, FLEET_DASHBOARD_MAX_HEATMAP_CELLS);

    // Forecast: linear extrapolation from trailing 7 days of daily data.
    //
    // Gated on per-day completeness. Costs coalesce None when a day is summed,
    // so an ungated forecast would quote a confident dollar figure built from a
    // partial sum, counting every unpriced call as free, while the totals beside
    // it correctly render null.
    let (forecast_daily, day_completeness) = forecast_input(&usage.daily);
    let forecast = build_forecast(&forecast_daily, &day_completeness, now);

    // Provider / model / project breakdowns.
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
            // The scanner does not resolve an upstream remote for a project
            // key, so there is nothing honest to put here.
            repo: None,
        })
        .collect();
    sort_and_cap(&mut projects, |row| &row.bucket);

    // Session breakdowns.
    let mut sessions: Vec<_> = usage
        .sessions
        .iter()
        .map(|row| FleetUsageSessionBucket {
            // The BARE session id. provider, project and session_id are
            // already three fields on this struct, so a composite would add
            // nothing a client could use: it cannot even be re-split, since
            // a project label may itself contain a colon.
            session_id: row.session_id.clone(),
            provider: row.provider.as_str().to_string(),
            project: row.project.clone(),
            bucket: bucket_from(&row.bucket, row.complete_cost),
        })
        .collect();
    sessions.sort_by(|a, b| {
        let total = |s: &FleetUsageSessionBucket| {
            s.bucket.input_tokens
                + s.bucket.cache_creation_tokens
                + s.bucket.cache_read_tokens
                + s.bucket.output_tokens
                + s.bucket.reasoning_tokens
        };
        total(b).cmp(&total(a))
    });
    sessions.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    // Branch breakdowns.
    let mut branches: Vec<_> = usage
        .branches
        .iter()
        .map(|row| FleetUsageBranchBucket {
            bucket: bucket_from(&row.bucket, row.complete_cost),
            branch: row.key.clone(),
        })
        .collect();
    branches.sort_by(|a, b| {
        let total = |s: &FleetUsageBranchBucket| {
            s.bucket.input_tokens
                + s.bucket.cache_creation_tokens
                + s.bucket.cache_read_tokens
                + s.bucket.output_tokens
                + s.bucket.reasoning_tokens
        };
        total(b).cmp(&total(a))
    });
    branches.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    // Tool / MCP / shell named-count breakdowns.
    // The scanner deliberately leaves `mcp_servers` empty and ships MCP calls as
    // raw `mcp__<server>__<tool>` entries in `tools`, because attribution is
    // consumer-specific (see the module header on
    // `ainb_plugin_session_reader::scanner`, which points at burndown's
    // `rebuild_activity_and_mcp_columns` as the reference). Doing that split
    // here is this module's job, not the scanner's. Skipping it left the MCP
    // panel permanently blank AND leaked `mcp__github__create_issue` style
    // strings into the tool list, one row per tool instead of one per server.
    let mut plain_tools: Vec<FleetUsageNamedBucket> = Vec::new();
    let mut mcp_counts: HashMap<String, u64> = HashMap::new();
    for row in named_buckets(&usage.tools) {
        match mcp_server_of(&row.name) {
            Some(server) => {
                let slot = mcp_counts.entry(server).or_default();
                *slot = slot.saturating_add(row.call_count);
            }
            None => plain_tools.push(row),
        }
    }
    let mut tools = plain_tools;
    // Tie-break by name so a cap boundary is deterministic across scans.
    tools.sort_by(|a, b| b.call_count.cmp(&a.call_count).then_with(|| a.name.cmp(&b.name)));
    tools.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    // Anything the scanner did populate is merged in rather than replaced, so
    // this keeps working if it ever starts attributing servers itself.
    for row in named_buckets(&usage.mcp_servers) {
        let slot = mcp_counts.entry(row.name).or_default();
        *slot = slot.saturating_add(row.call_count);
    }
    let mut mcp_servers: Vec<_> = mcp_counts
        .into_iter()
        .map(|(name, call_count)| FleetUsageNamedBucket { name, call_count })
        .collect();
    mcp_servers.sort_by(|a, b| b.call_count.cmp(&a.call_count).then_with(|| a.name.cmp(&b.name)));
    mcp_servers.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    // Only the PROGRAM name leaves this module, never the command line. The
    // scanner keys these on the verbatim `input.command`, which routinely
    // carries absolute paths and can carry credentials, and this result is both
    // sent over the wire and written to a world-readable cache file. Shipping a
    // program name matches how `tools` ships a tool name and keeps the promise
    // in this module's header.
    let mut by_program: HashMap<String, u64> = HashMap::new();
    for row in &usage.shell_commands {
        *by_program.entry(program_name(&row.name)).or_default() +=
            u64::try_from(row.calls).unwrap_or(u64::MAX);
    }
    let mut shell_commands: Vec<_> = by_program
        .into_iter()
        .map(|(name, call_count)| FleetUsageNamedBucket { name, call_count })
        .collect();
    // Tie-break by name so a cap boundary is deterministic across scans.
    shell_commands
        .sort_by(|a, b| b.call_count.cmp(&a.call_count).then_with(|| a.name.cmp(&b.name)));
    shell_commands.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    FleetUsageDashboardResult {
        state: FleetUsageSummaryState::Ready,
        generated_at: Some(now.timestamp_millis()),
        start_at: Some(start.timestamp_millis()),
        end_at: Some(end.timestamp_millis()),
        cost_complete,
        totals: Some(bucket_from(&usage.totals, cost_complete)),
        weekly,
        heatmap,
        forecast,
        providers,
        models,
        projects,
        sessions,
        branches,
        tools,
        mcp_servers,
        shell_commands,
        detail: None,
    }
}

fn named_buckets(rows: &[NamedUsage]) -> Vec<FleetUsageNamedBucket> {
    rows.iter()
        .map(|row| FleetUsageNamedBucket {
            name: row.name.clone(),
            call_count: u64::try_from(row.calls).unwrap_or(u64::MAX),
        })
        .collect()
}

/// Split the scanner's daily rows into the two shapes [`build_forecast`] reads.
///
/// The rows already carry their own `complete_cost`; the forecast wants it as a
/// lookup because it samples by date rather than walking the rows in order. A
/// row whose key is not an ISO date cannot be a day and is dropped rather than
/// guessed at.
fn forecast_input(
    daily: &[scanner::UsageRow],
) -> (Vec<(NaiveDate, TokenBucket)>, HashMap<NaiveDate, bool>) {
    let mut buckets = Vec::with_capacity(daily.len());
    let mut completeness = HashMap::with_capacity(daily.len());
    for row in daily {
        let Ok(date) = row.key.parse::<NaiveDate>() else {
            continue;
        };
        buckets.push((date, row.bucket));
        completeness.insert(date, row.complete_cost);
    }
    (buckets, completeness)
}

/// Trailing-7-day linear forecast.
///
/// `day_completeness` gates the cost leg only; the token projection is always
/// exact and does not depend on pricing.
fn build_forecast(
    daily: &[(NaiveDate, TokenBucket)],
    day_completeness: &HashMap<NaiveDate, bool>,
    now: DateTime<Utc>,
) -> Option<FleetUsageForecast> {
    let cutoff = now.date_naive() - Duration::days(TRAILING_FORECAST_DAYS);
    let trailing: Vec<_> = daily.iter().filter(|(date, _)| *date > cutoff).collect();
    let earliest = trailing.iter().map(|(date, _)| *date).min()?;

    // Average over CALENDAR days, not days that happen to have data. The next 30
    // days will contain idle days too, so dividing by active days only would
    // quote an "every day is a working day" rate and overstate the projection --
    // for someone who works two days a week, by roughly 3.5x.
    //
    // Shortening the window to the observed span is ONLY right for an account
    // with no earlier history: a long-time user who happened to be idle for six
    // days and worked today would otherwise be divided by 1 and projected at 30x
    // a single day. Any activity at or before the cutoff proves the account is
    // not new, so the full window is the honest divisor.
    let has_earlier_history = daily.iter().any(|(date, _)| *date <= cutoff);
    let denominator = if has_earlier_history {
        TRAILING_FORECAST_DAYS as u64
    } else {
        let span_days = (now.date_naive() - earliest).num_days() + 1;
        span_days.clamp(1, TRAILING_FORECAST_DAYS) as u64
    };

    let total_tokens: u64 = trailing
        .iter()
        .map(|(_, b)| {
            b.input_tokens
                + b.cache_creation_tokens
                + b.cache_read_tokens
                + b.output_tokens
                + b.reasoning_tokens
        })
        .sum();
    let avg_daily_tokens = total_tokens / denominator;

    // A sampled day whose cost is only partially known makes the whole
    // projection unknowable. The scanner coalesces None when summing a day, so
    // without this gate an unpriced call silently counts as $0 and the forecast
    // reads as confident and cheap.
    let all_sampled_days_priced = trailing
        .iter()
        .all(|(date, _)| day_completeness.get(date).copied().unwrap_or(false));
    let total_cost: Option<f64> = all_sampled_days_priced
        .then(|| trailing.iter().try_fold(0.0_f64, |acc, (_, b)| b.cost_usd.map(|c| acc + c)))
        .flatten();
    let avg_daily_cost = total_cost.map(|c| c / denominator as f64);

    Some(FleetUsageForecast {
        projected_30d_cost_usd: avg_daily_cost.map(|c| c * 30.0),
        // Saturating: a pathological token total must not wrap into a small
        // number and quote a reassuring forecast.
        projected_30d_tokens: avg_daily_tokens.saturating_mul(30),
        avg_daily_cost_usd: avg_daily_cost,
        avg_daily_tokens,
        // Reports the DENOMINATOR, so the client's "avg/day (Nd sample)" label
        // describes the divisor actually used.
        sample_days: u32::try_from(denominator).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // `ProviderCall` is a TEST-only type here. The projection above is fed
    // pre-aggregated windows and never sees a call, which is the whole point of
    // `scanner::scan_windows`; the fixtures below still start from calls so they
    // drive the real accumulator rather than a hand-built aggregate.
    use ainb_plugin_types_sessions::ProviderCall;

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

    /// A `WindowUsage` built by hand, for the summary projection's ranking and
    /// gating tests where the rows themselves are the fixture.
    fn window_of_rows(rows: Vec<scanner::UsageRow>) -> scanner::WindowUsage {
        scanner::WindowUsage {
            totals: TokenBucket {
                input_tokens: rows.iter().map(|r| r.bucket.input_tokens).sum(),
                ..TokenBucket::default()
            },
            totals_complete_cost: rows.iter().all(|r| r.complete_cost),
            daily: Vec::new(),
            weekly: Vec::new(),
            models: rows.clone(),
            projects: rows.clone(),
            providers: rows,
            branches: Vec::new(),
            sessions: Vec::new(),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            shell_commands: Vec::new(),
        }
    }

    /// Project a call set through the REAL windowed accumulator, exactly as
    /// `scan_all_summaries` does for the dashboard. Hand-building a
    /// `WindowUsage` here would let the fixture disagree with what the scanner
    /// actually produces, which is precisely what these tests exist to catch.
    fn dashboard_of(calls: &[ProviderCall], now: DateTime<Utc>) -> FleetUsageDashboardResult {
        dashboard_from_window(&scanner::window_usage(calls, dashboard_window(now)), now)
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
            let summary = summary_from_window(&window_of_rows(Vec::new()), period, now);
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

    /// `scan_all_summaries` pops the LAST scanned window as the dashboard and
    /// zips the remainder against `PERIODS`. Both depend on this list's order,
    /// and getting it wrong would label 371 days of data as `Today`: a wrong
    /// number that still looks like a number, so nothing else would catch it.
    #[test]
    fn the_scan_asks_for_each_summary_period_then_the_dashboard() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let windows = scan_window_list(now);

        assert_eq!(
            windows.len(),
            PERIODS.len() + 1,
            "one window per summary period, plus the dashboard"
        );
        for (period, scanned) in PERIODS.iter().zip(&windows) {
            let (start, end) = window(*period, now);
            assert_eq!(scanned.start, start, "{period:?} start");
            assert_eq!(scanned.end, end, "{period:?} end");
        }
        let dashboard = windows.last().expect("dashboard window is appended last");
        assert_eq!(
            *dashboard,
            dashboard_window(now),
            "the dashboard window must be the one popped off the back"
        );
        assert_eq!(
            (dashboard.end - dashboard.start).num_days(),
            DASHBOARD_DAYS,
            "the dashboard reads 53 weeks, not a summary period"
        );
    }

    /// The 53-week history is served by ANOTHER `UsageWindow` in the same
    /// single pass, never by widening a scan back into a full-corpus call
    /// vector. This module holds no `ProviderCall` outside its fixtures, and
    /// that is what keeps the daemon's peak RSS flat as history grows.
    #[test]
    fn the_dashboard_window_is_scanned_alongside_the_summaries_not_separately() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let windows = scan_window_list(now);
        let dashboard = *windows.last().expect("dashboard window");

        // Every summary window is contained by the dashboard window, which is
        // exactly why one pass can serve both.
        for scanned in &windows[..PERIODS.len()] {
            assert!(
                scanned.start >= dashboard.start && scanned.end <= dashboard.end,
                "a summary window outside the dashboard window would force a second scan"
            );
        }
    }

    /// A bucket whose calls were not all priced must publish no cost at all,
    /// on every breakdown as well as the totals. Publishing the partial sum
    /// would understate spend without saying so.
    #[test]
    fn a_partially_priced_window_publishes_no_cost_anywhere() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let usage = window_of_rows(vec![
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
        let summary = summary_from_window(&window_of_rows(rows), FleetUsagePeriod::Today, now);

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

    #[test]
    fn the_dashboard_projects_every_dimension() {
        use ainb_plugin_types_sessions::Provider;

        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let call_ts = now - Duration::hours(1);
        let call = ProviderCall {
            id: 1,
            provider: Provider::Claude,
            model: "claude-sonnet-4-5".into(),
            session_id: "s1".into(),
            project: "ainb".into(),
            project_path: "/repo".into(),
            timestamp: call_ts,
            input_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 50,
            output_tokens: 200,
            reasoning_tokens: 0,
            cost_usd: Some(0.005),
            tools: vec!["Read".into()],
            bash_commands: vec!["cargo test".into()],
            user_message: "test".into(),
            branch: Some("feat/dash".into()),
        };

        let result = dashboard_of(&[call], now);

        assert_eq!(result.state, FleetUsageSummaryState::Ready);
        assert!(result.generated_at.is_some());
        assert!(result.cost_complete);
        assert!(result.totals.is_some());
        // Heatmap should have the one day.
        assert_eq!(result.heatmap.len(), 1);
        assert_eq!(result.heatmap[0].call_count, 1);
        // Weekly should have the one week.
        assert_eq!(result.weekly.len(), 1);
        assert_eq!(result.weekly[0].week_start, "2026-08-03");
        // Forecast from 1 trailing day.
        assert!(result.forecast.is_some());
        let forecast = result.forecast.unwrap();
        assert_eq!(forecast.sample_days, 1);
        assert!(forecast.avg_daily_cost_usd.is_some());
        // Dimension breakdowns.
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(
            result.sessions[0].session_id, "s1",
            "the bare session id ships, not a composite"
        );
        assert_eq!(result.sessions[0].provider, "claude");
        assert_eq!(result.branches.len(), 1);
        assert_eq!(result.branches[0].branch, "feat/dash");
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "Read");
        assert_eq!(result.shell_commands.len(), 1);
        assert_eq!(
            result.shell_commands[0].name, "cargo",
            "only the program name leaves the daemon"
        );
        assert_eq!(result.providers.len(), 1);
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.projects.len(), 1);
    }

    /// A 371-day window only aligns to exactly 53 ISO weeks when it starts on a
    /// Monday; on the other six days it touches 54, and the extra one is the
    /// CURRENT week. Capping must therefore drop the oldest week, never the
    /// newest -- a dashboard that silently omits this week is worse than one
    /// that omits a week from last year.
    #[test]
    fn weekly_cap_keeps_the_current_week() {
        use ainb_plugin_types_sessions::Provider;
        use chrono::Datelike;

        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        // end_day is tomorrow, so the window opens 371 days before that.
        let start_day = now.date_naive().succ_opt().unwrap() - Duration::days(DASHBOARD_DAYS);
        assert_ne!(
            start_day.weekday(),
            chrono::Weekday::Mon,
            "fixture must exercise the unaligned case"
        );

        // One call per week across the whole window, plus one today. Stepping by
        // 7 from a Friday lands on 2026-07-31 and stops, so today's call must be
        // added explicitly -- without it the current ISO week is never populated
        // and the test proves nothing.
        let mut days: Vec<NaiveDate> = Vec::new();
        let mut day = start_day;
        while day <= now.date_naive() {
            days.push(day);
            day += Duration::days(7);
        }
        if days.last() != Some(&now.date_naive()) {
            days.push(now.date_naive());
        }

        let mut calls: Vec<ProviderCall> = Vec::new();
        let mut id = 0_u64;
        for day in days {
            id += 1;
            calls.push(ProviderCall {
                id,
                provider: Provider::Claude,
                model: "claude-sonnet-4-5".into(),
                session_id: format!("s{id}"),
                project: "ainb".into(),
                project_path: "/repo".into(),
                timestamp: day.and_hms_opt(12, 0, 0).unwrap().and_utc(),
                input_tokens: 10,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                output_tokens: 10,
                reasoning_tokens: 0,
                cost_usd: Some(0.001),
                tools: vec![],
                bash_commands: vec![],
                user_message: String::new(),
                branch: None,
            });
        }

        let result = dashboard_of(&calls, now);

        assert!(
            result.weekly.len() <= FLEET_DASHBOARD_MAX_WEEKLY_BUCKETS,
            "weekly must stay within the wire cap"
        );
        let newest = result.weekly.last().expect("dashboard must report at least one week");
        assert_eq!(
            newest.week_start, "2026-08-03",
            "capping dropped the current week instead of the oldest one"
        );
    }

    /// Cost completeness is per-row, not global. One unpriced call in an
    /// unrelated session must not blank the cost of every OTHER session and
    /// branch -- over a 53-week window a single unknown model would otherwise
    /// wipe cost off the entire dashboard.
    #[test]
    fn one_unpriced_call_only_blanks_its_own_session_and_branch() {
        use ainb_plugin_types_sessions::Provider;

        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let call = |id: u64, session: &str, branch: &str, cost: Option<f64>| ProviderCall {
            id,
            provider: Provider::Claude,
            model: "claude-sonnet-4-5".into(),
            session_id: session.into(),
            project: "ainb".into(),
            project_path: "/repo".into(),
            timestamp: now - Duration::hours(1),
            input_tokens: 10,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 10,
            reasoning_tokens: 0,
            cost_usd: cost,
            tools: vec![],
            bash_commands: vec![],
            user_message: String::new(),
            branch: Some(branch.into()),
        };

        let result = dashboard_of(
            &[
                call(1, "priced", "feat/priced", Some(0.01)),
                call(2, "unpriced", "feat/unpriced", None),
            ],
            now,
        );

        let priced = result
            .sessions
            .iter()
            .find(|s| s.session_id == "priced")
            .expect("priced session present");
        let unpriced = result
            .sessions
            .iter()
            .find(|s| s.session_id == "unpriced")
            .expect("unpriced session present");
        assert_eq!(
            priced.bucket.cost_usd,
            Some(0.01),
            "a fully priced session must keep its cost"
        );
        assert_eq!(
            unpriced.bucket.cost_usd, None,
            "an unpriced session must report no cost, never 0.0"
        );

        let priced_branch = result
            .branches
            .iter()
            .find(|b| b.branch == "feat/priced")
            .expect("priced branch present");
        let unpriced_branch = result
            .branches
            .iter()
            .find(|b| b.branch == "feat/unpriced")
            .expect("unpriced branch present");
        assert_eq!(priced_branch.bucket.cost_usd, Some(0.01));
        assert_eq!(unpriced_branch.bucket.cost_usd, None);

        // The dashboard as a whole is still honestly flagged incomplete.
        assert!(!result.cost_complete);
    }

    /// The forecast answers "what will the next 30 days cost", and those 30 days
    /// include idle days. Averaging only over days that HAVE data would quote a
    /// working-day rate and overstate an intermittent user's projection.
    #[test]
    fn forecast_averages_over_calendar_days_not_active_days() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let day = |offset: i64| now.date_naive() - Duration::days(offset);
        let spend = |cost: f64| TokenBucket {
            input_tokens: 100,
            cost_usd: Some(cost),
            ..TokenBucket::default()
        };

        // Worked 2 days out of a 7-day span: $10 total.
        let priced_days = HashMap::from([(day(6), true), (day(0), true)]);
        let forecast = build_forecast(
            &[(day(6), spend(5.0)), (day(0), spend(5.0))],
            &priced_days,
            now,
        )
        .expect("trailing data yields a forecast");

        assert_eq!(
            forecast.sample_days, 7,
            "the divisor is the calendar span, and is what gets reported"
        );
        assert_eq!(
            forecast.avg_daily_cost_usd,
            Some(10.0 / 7.0),
            "$10 over a 7-day span is a 7-day average, not a 2-day one"
        );
        // Guard the actual regression: the active-day divisor would have said $5/day.
        assert!(
            forecast.avg_daily_cost_usd < Some(5.0),
            "averaging over active days only would overstate the rate"
        );
    }

    /// A brand-new user has no idle days to average in yet; diluting their one
    /// day of history across a week they were never present for would understate
    /// the projection just as badly.
    #[test]
    fn forecast_does_not_dilute_a_single_day_of_history() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let bucket = TokenBucket {
            input_tokens: 100,
            cost_usd: Some(3.0),
            ..TokenBucket::default()
        };

        let priced_days = HashMap::from([(now.date_naive(), true)]);
        let forecast =
            build_forecast(&[(now.date_naive(), bucket)], &priced_days, now).expect("forecast");

        assert_eq!(forecast.sample_days, 1);
        assert_eq!(forecast.avg_daily_cost_usd, Some(3.0));
        assert_eq!(forecast.projected_30d_cost_usd, Some(90.0));
    }

    /// Command ARGUMENTS are where absolute paths and credentials live, and this
    /// result reaches both the wire and a world-readable cache file. Only the
    /// program name may leave the daemon.
    #[test]
    fn shell_commands_ship_the_program_only_never_the_arguments() {
        assert_eq!(program_name("cargo test --workspace"), "cargo");
        // An absolute install prefix must not leak, and must fold together with
        // the bare invocation of the same program.
        assert_eq!(
            program_name("/usr/local/bin/rg --hidden /Users/someone/src"),
            "rg"
        );
        // A leading inline assignment is skipped: reporting it would make the
        // secret itself the bucket name.
        assert_eq!(
            program_name("API_KEY=sk-live-abc123 ./deploy.sh --prod"),
            "deploy.sh"
        );
        assert_eq!(program_name(""), "other");
        assert_eq!(program_name("FOO=1 BAR=2"), "other");

        // Nothing resembling an argument survives the full projection, and
        // variants of one program collapse into a single counted row.
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let call = |id: u64, cmd: &str| ProviderCall {
            id,
            provider: ainb_plugin_types_sessions::Provider::Claude,
            model: "claude-sonnet-4-5".into(),
            session_id: format!("s{id}"),
            project: "ainb".into(),
            project_path: "/repo".into(),
            timestamp: now - Duration::hours(1),
            input_tokens: 1,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: 0,
            cost_usd: Some(0.01),
            tools: vec![],
            bash_commands: vec![cmd.to_string()],
            user_message: String::new(),
            branch: None,
        };
        let result = dashboard_of(
            &[
                call(1, "git commit -m 'secret /Users/someone/private'"),
                call(2, "git push origin main"),
                call(
                    3,
                    "curl -H 'Authorization: Bearer sk-live-xyz' https://api.example.com",
                ),
            ],
            now,
        );

        let git = result.shell_commands.iter().find(|r| r.name == "git").expect("git row");
        assert_eq!(git.call_count, 2, "both git invocations fold into one row");
        for row in &result.shell_commands {
            assert!(
                !row.name.contains(' ') && !row.name.contains('/'),
                "a bare program name cannot carry arguments or paths, got {:?}",
                row.name
            );
        }
        let shipped = format!("{:?}", result.shell_commands);
        for leaked in ["Bearer", "sk-live", "/Users/", "secret", "origin"] {
            assert!(
                !shipped.contains(leaked),
                "{leaked} must never reach the wire"
            );
        }
    }

    /// A week containing one unpriced call must report no cost rather than a
    /// partial sum. The scanner coalesces None when summing, so the partial
    /// figure would otherwise read as the whole week's spend.
    #[test]
    fn one_unpriced_call_only_blanks_its_own_week() {
        use ainb_plugin_types_sessions::Provider;

        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let call = |id: u64, day: NaiveDate, cost: Option<f64>| ProviderCall {
            id,
            provider: Provider::Claude,
            model: "claude-sonnet-4-5".into(),
            session_id: format!("s{id}"),
            project: "ainb".into(),
            project_path: "/repo".into(),
            timestamp: day.and_hms_opt(12, 0, 0).unwrap().and_utc(),
            input_tokens: 10,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 10,
            reasoning_tokens: 0,
            cost_usd: cost,
            tools: vec![],
            bash_commands: vec![],
            user_message: String::new(),
            branch: None,
        };
        // This week (Mon 2026-08-03) is fully priced; last week has one gap.
        let this_week = NaiveDate::from_ymd_opt(2026, 8, 4).unwrap();
        let last_week = NaiveDate::from_ymd_opt(2026, 7, 28).unwrap();
        let result = dashboard_of(
            &[
                call(1, this_week, Some(0.02)),
                call(2, last_week, Some(0.05)),
                call(3, last_week, None),
            ],
            now,
        );

        let priced =
            result.weekly.iter().find(|w| w.week_start == "2026-08-03").expect("this week");
        let partial =
            result.weekly.iter().find(|w| w.week_start == "2026-07-27").expect("last week");
        assert_eq!(
            priced.bucket.cost_usd,
            Some(0.02),
            "a fully priced week keeps its cost"
        );
        assert_eq!(
            partial.bucket.cost_usd, None,
            "a week with an unpriced call must not report the partial sum as its total"
        );
    }

    /// The forecast is the only cost surface that does not flow through
    /// `bucket_from`, so it needs its own gate: quoting a confident dollar
    /// projection built from partial day sums is the exact failure the
    /// never-zero rule exists to prevent.
    #[test]
    fn forecast_reports_no_cost_when_a_sampled_day_is_unpriced() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let day = |offset: i64| now.date_naive() - Duration::days(offset);
        let bucket = |cost: f64| TokenBucket {
            input_tokens: 100,
            cost_usd: Some(cost),
            ..TokenBucket::default()
        };
        let daily = [(day(1), bucket(5.0)), (day(0), bucket(5.0))];

        // Day 0's cost is a partial sum, so the projection is unknowable.
        let gapped = HashMap::from([(day(1), true), (day(0), false)]);
        let forecast = build_forecast(&daily, &gapped, now).expect("forecast");
        assert_eq!(
            forecast.projected_30d_cost_usd, None,
            "an unpriced sampled day must blank the cost projection, not count it as free"
        );
        assert_eq!(forecast.avg_daily_cost_usd, None);
        // Tokens are exact regardless of pricing, so they must still be reported.
        assert!(
            forecast.projected_30d_tokens > 0,
            "the token projection does not depend on price"
        );

        let complete = HashMap::from([(day(1), true), (day(0), true)]);
        let priced = build_forecast(&daily, &complete, now).expect("forecast");
        assert!(
            priced.projected_30d_cost_usd.is_some(),
            "a fully priced sample still forecasts"
        );
    }

    /// A long-time user who was idle all week and worked today must not be
    /// divided by one and projected at 30x a single day. Earlier history is the
    /// evidence that distinguishes them from a genuinely new account.
    #[test]
    fn forecast_does_not_treat_a_long_idle_user_as_brand_new() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let day = |offset: i64| now.date_naive() - Duration::days(offset);
        let spend = |cost: f64| TokenBucket {
            input_tokens: 100,
            cost_usd: Some(cost),
            ..TokenBucket::default()
        };

        // Activity 90 days ago proves the account is not new, then a $7 day today.
        let daily = [(day(90), spend(1.0)), (day(0), spend(7.0))];
        let priced = HashMap::from([(day(90), true), (day(0), true)]);
        let forecast = build_forecast(&daily, &priced, now).expect("forecast");

        assert_eq!(
            forecast.sample_days, 7,
            "an established account uses the full window"
        );
        assert_eq!(
            forecast.avg_daily_cost_usd,
            Some(1.0),
            "$7 over 7 days, not $7 over 1"
        );
        assert_eq!(forecast.projected_30d_cost_usd, Some(30.0));
    }

    /// The scanner ships MCP calls as raw `mcp__<server>__<tool>` entries inside
    /// `tools` and leaves `mcp_servers` empty on purpose, leaving attribution to
    /// each consumer. Without that split here the MCP panel renders permanently
    /// blank AND the tool list is polluted with prefixed names, one row per tool
    /// rather than one per server. Mirrors burndown's
    /// `rebuild_activity_and_mcp_columns_splits_mcp_prefix_from_tools`.
    #[test]
    fn mcp_tools_are_attributed_to_servers_and_leave_the_tool_list() {
        assert_eq!(
            mcp_server_of("mcp__github__create_issue").as_deref(),
            Some("github")
        );
        assert_eq!(
            mcp_server_of("Read"),
            None,
            "an ordinary tool is not an MCP call"
        );
        assert_eq!(
            mcp_server_of("mcp__"),
            None,
            "a prefix with no server is not a server"
        );
        // A well-formed MCP tool name has BOTH segments. Accepting a truncated
        // one would delete an ordinary tool from the tool list and invent a
        // server bucket that never made a call.
        assert_eq!(
            mcp_server_of("mcp__github"),
            None,
            "no tool segment means this is not an MCP call"
        );
        assert_eq!(
            mcp_server_of("mcp__github__"),
            None,
            "an empty tool segment means this is not an MCP call"
        );

        use ainb_plugin_types_sessions::Provider;
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let call = |id: u64, tools: Vec<String>| ProviderCall {
            id,
            provider: Provider::Claude,
            model: "claude-sonnet-4-5".into(),
            session_id: format!("s{id}"),
            project: "ainb".into(),
            project_path: "/repo".into(),
            timestamp: now - Duration::hours(1),
            input_tokens: 1,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: 0,
            cost_usd: Some(0.01),
            tools,
            bash_commands: vec![],
            user_message: String::new(),
            branch: None,
        };
        let usage = UsageData {
            calls: vec![
                call(1, vec!["mcp__github__create_issue".into(), "Read".into()]),
                call(2, vec!["mcp__github__list_prs".into()]),
                call(3, vec!["mcp__context7__query_docs".into(), "Read".into()]),
            ],
            ..UsageData::default()
        };
        let result = dashboard_from_usage(&usage, now);

        let tool_names: Vec<&str> = result.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !tool_names.iter().any(|n| n.starts_with("mcp__")),
            "no mcp__ tool may leak into the tool list: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"Read"),
            "ordinary tools stay: {tool_names:?}"
        );

        let github = result
            .mcp_servers
            .iter()
            .find(|s| s.name == "github")
            .expect("github server row");
        assert_eq!(
            github.call_count, 2,
            "two mcp__github__* tools collapse into one server row"
        );
        assert!(
            result.mcp_servers.iter().any(|s| s.name == "context7"),
            "every distinct server gets a row"
        );
    }

    #[test]
    fn forecast_requires_trailing_data() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        // Empty daily data yields no forecast.
        assert!(build_forecast(&[], &HashMap::new(), now).is_none());
        // Data older than 7 days is excluded.
        let old_date = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let bucket = TokenBucket {
            input_tokens: 100,
            cost_usd: Some(1.0),
            ..TokenBucket::default()
        };
        assert!(build_forecast(&[(old_date, bucket)], &HashMap::new(), now).is_none());
    }
}
