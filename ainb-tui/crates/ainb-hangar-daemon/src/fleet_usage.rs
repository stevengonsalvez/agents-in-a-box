//! Bounded, daemon-owned Fleet Usage projection.
//!
//! Provider logs and canonical model-rate parsing stay behind this module. The
//! public RPC receives only aggregates, never paths, transcripts, or calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration as StdDuration, SystemTime};

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
use ainb_plugin_types_sessions::{ProviderCall, TokenBucket, UsageData};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_VERSION: u32 = 2;
const REFRESH_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);

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

    /// Coalesce concurrent refresh requests into one background worker.
    pub async fn request_refresh(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().await;
            if state.refreshing {
                return;
            }
            state.refreshing = true;
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
            match result {
                Ok(cached) => {
                    if let Err(error) = write_cache(&service.cache_path, &cached) {
                        state.last_error =
                            Some(format!("could not persist usage snapshot: {error}"));
                    } else {
                        *service.cached.lock().await = Some(cached);
                    }
                }
                Err(error) => state.last_error = Some(error),
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

/// Scan canonical provider histories once, then derive every public window.
///
/// The scanner owns provider parsing and rate lookup. This service stores only
/// the bounded projections, never provider calls or their local paths.
/// Number of days in the 53-week dashboard window.
const DASHBOARD_DAYS: i64 = 371;

/// Trailing window the 30-day forecast extrapolates from.
const TRAILING_FORECAST_DAYS: i64 = 7;

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
    // Widen the scan window to 53 weeks (371 days) so the dashboard can
    // project the full heatmap and weekly history. The summary windows
    // (today / 7d / 30d) are subsets that filter within this data.
    let dashboard_start =
        now.date_naive().succ_opt().expect("valid UTC date") - Duration::days(DASHBOARD_DAYS);
    let since = SystemTime::UNIX_EPOCH
        + StdDuration::from_millis(
            u64::try_from(
                dashboard_start
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight UTC")
                    .and_utc()
                    .timestamp_millis(),
            )
            .unwrap_or_default(),
        );
    let usage = scanner::scan_since(&roots, since);
    Ok(CachedSummaries {
        version: CACHE_VERSION,
        summaries: [
            FleetUsagePeriod::Today,
            FleetUsagePeriod::Trailing7Days,
            FleetUsagePeriod::Trailing30Days,
        ]
        .into_iter()
        .map(|period| CachedSummary {
            period,
            summary: summary_from_usage(&usage, period, now),
        })
        .collect(),
        dashboard: Some(dashboard_from_usage(&usage, now)),
    })
}

fn summary_from_usage(
    usage: &UsageData,
    period: FleetUsagePeriod,
    now: DateTime<Utc>,
) -> FleetUsageSummaryResult {
    let (start, end) = window(period, now);
    let calls: Vec<_> = usage
        .calls
        .iter()
        .filter(|call| call.timestamp >= start && call.timestamp < end)
        .cloned()
        .collect();
    let projected = scanner::aggregate(calls.clone());
    let complete_costs = completeness(&calls);

    let mut daily: Vec<_> = projected
        .daily
        .into_iter()
        .map(|(date, bucket)| FleetUsageDailyBucket {
            date: date.to_string(),
            bucket: bucket_from(
                &bucket,
                complete_costs.daily.get(&date).copied().unwrap_or(true),
            ),
        })
        .collect();
    daily.truncate(FLEET_USAGE_MAX_DAILY_BUCKETS);

    let mut providers: Vec<_> = grouped_usage(&calls, |call| call.provider.as_str().to_string())
        .into_iter()
        .map(|(provider, usage)| FleetUsageProviderBucket {
            bucket: bucket_from(&usage.bucket, usage.complete_cost),
            provider,
        })
        .collect();
    sort_and_cap(&mut providers, |row| &row.bucket);

    let mut models: Vec<_> = projected
        .models
        .into_iter()
        .map(|row| FleetUsageModelBucket {
            bucket: bucket_from(
                &row.bucket,
                complete_costs.models.get(&row.model).copied().unwrap_or(true),
            ),
            model: row.model,
        })
        .collect();
    sort_and_cap(&mut models, |row| &row.bucket);

    let mut projects: Vec<_> = projected
        .projects
        .into_iter()
        .map(|row| FleetUsageProjectBucket {
            bucket: bucket_from(
                &row.bucket,
                complete_costs.projects.get(&row.name).copied().unwrap_or(true),
            ),
            project: row.name,
            repo: row.repo,
        })
        .collect();
    sort_and_cap(&mut projects, |row| &row.bucket);

    FleetUsageSummaryResult {
        state: FleetUsageSummaryState::Ready,
        generated_at: Some(now.timestamp_millis()),
        start_at: Some(start.timestamp_millis()),
        end_at: Some(end.timestamp_millis()),
        totals: Some(bucket_from(
            &projected.grand_total,
            calls.iter().all(|call| call.cost_usd.is_some()),
        )),
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

#[derive(Default)]
struct CostCompleteness {
    daily: HashMap<NaiveDate, bool>,
    weeks: HashMap<NaiveDate, bool>,
    models: HashMap<String, bool>,
    projects: HashMap<String, bool>,
    sessions: HashMap<String, bool>,
    branches: HashMap<String, bool>,
}

/// Composite session key shared by the completeness map and the wire row, so a
/// session's priced-ness is looked up under exactly the key it is reported as.
fn session_key(provider: &str, project: &str, session_id: &str) -> String {
    format!("{provider}:{project}:{session_id}")
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

/// Monday anchoring the ISO week containing `date`, matching how the scanner
/// keys its weekly buckets. The completeness map has to agree with the bucket
/// map or the lookup silently misses.
fn week_anchor(date: NaiveDate) -> NaiveDate {
    use chrono::Datelike;
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

fn completeness(calls: &[ProviderCall]) -> CostCompleteness {
    let mut result = CostCompleteness::default();
    for call in calls {
        let priced = call.cost_usd.is_some();
        let day = call.timestamp.date_naive();
        mark(&mut result.daily, day, priced);
        mark(&mut result.weeks, week_anchor(day), priced);
        mark(&mut result.models, call.model.clone(), priced);
        mark(&mut result.projects, call.project.clone(), priced);
        mark(
            &mut result.sessions,
            session_key(call.provider.as_str(), &call.project, &call.session_id),
            priced,
        );
        // The scanner only buckets non-empty branches; mirror that so a blank
        // branch does not create a phantom completeness entry.
        if let Some(branch) = call.branch.as_deref().filter(|b| !b.is_empty()) {
            mark(&mut result.branches, branch.to_string(), priced);
        }
    }
    result
}

fn mark<K: std::hash::Hash + Eq>(map: &mut HashMap<K, bool>, key: K, priced: bool) {
    map.entry(key).and_modify(|complete| *complete &= priced).or_insert(priced);
}

struct GroupedUsage {
    bucket: TokenBucket,
    complete_cost: bool,
}

fn grouped_usage<F>(calls: &[ProviderCall], key: F) -> HashMap<String, GroupedUsage>
where
    F: Fn(&ProviderCall) -> String,
{
    let mut groups: HashMap<String, Vec<ProviderCall>> = HashMap::new();
    for call in calls {
        groups.entry(key(call)).or_default().push(call.clone());
    }
    groups
        .into_iter()
        .map(|(key, calls)| {
            let complete_cost = calls.iter().all(|call| call.cost_usd.is_some());
            let bucket = scanner::aggregate(calls).grand_total;
            (
                key,
                GroupedUsage {
                    bucket,
                    complete_cost,
                },
            )
        })
        .collect()
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

fn dashboard_from_usage(usage: &UsageData, now: DateTime<Utc>) -> FleetUsageDashboardResult {
    let end_day = now.date_naive().succ_opt().expect("valid UTC date");
    let start_day = end_day - Duration::days(DASHBOARD_DAYS);
    let start = start_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc();
    let end = end_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc();

    let calls: Vec<_> = usage
        .calls
        .iter()
        .filter(|call| call.timestamp >= start && call.timestamp < end)
        .cloned()
        .collect();
    let projected = scanner::aggregate(calls.clone());
    let cost_complete = calls.iter().all(|call| call.cost_usd.is_some());
    let completeness = completeness(&calls);

    // Weekly buckets from scanner's pre-computed weekly aggregates.
    //
    // Priced-ness is per week, not the global flag: the scanner sums costs with
    // None coalescing, so a week containing one unpriced call would otherwise
    // report a partial sum as if it were the whole week's spend.
    let mut weekly: Vec<_> = projected
        .weekly
        .into_iter()
        .map(|(week_start, bucket)| FleetUsageWeeklyBucket {
            bucket: bucket_from(
                &bucket,
                completeness.weeks.get(&week_start).copied().unwrap_or(false),
            ),
            week_start: week_start.to_string(),
        })
        .collect();
    // Ascending by week (scanner keys off a BTreeMap), and a 371-day window
    // touches 54 ISO weeks whenever it does not open on a Monday. Cap from the
    // FRONT so the week we drop is the oldest, never the current one.
    keep_newest(&mut weekly, FLEET_DASHBOARD_MAX_WEEKLY_BUCKETS);

    // Heatmap: one cell per calendar day with call count and cost.
    let mut daily_map: HashMap<NaiveDate, (u64, Option<f64>)> = HashMap::new();
    for call in &calls {
        let day = call.timestamp.date_naive();
        let entry = daily_map.entry(day).or_insert((0, Some(0.0)));
        entry.0 += 1;
        match (entry.1, call.cost_usd) {
            (Some(acc), Some(cost)) => entry.1 = Some(acc + cost),
            _ => entry.1 = None,
        }
    }
    let mut heatmap: Vec<_> = daily_map
        .into_iter()
        .map(|(date, (count, cost))| FleetHeatmapCell {
            date: date.to_string(),
            call_count: count,
            cost_usd: cost,
        })
        .collect();
    heatmap.sort_by_key(|cell| cell.date.clone());
    keep_newest(&mut heatmap, FLEET_DASHBOARD_MAX_HEATMAP_CELLS);

    // Forecast: linear extrapolation from trailing 7 days of daily data.
    //
    // Gated on per-day completeness. The scanner coalesces None when summing a
    // day's cost, so an ungated forecast would quote a confident dollar figure
    // built from a partial sum, counting every unpriced call as free, while the
    // totals beside it correctly render null.
    let forecast = build_forecast(&projected.daily, &completeness.daily, now);

    // Provider / model / project breakdowns (reuse existing helpers).
    let mut providers: Vec<_> = grouped_usage(&calls, |call| call.provider.as_str().to_string())
        .into_iter()
        .map(|(provider, gu)| FleetUsageProviderBucket {
            bucket: bucket_from(&gu.bucket, gu.complete_cost),
            provider,
        })
        .collect();
    sort_and_cap(&mut providers, |row| &row.bucket);

    let mut models: Vec<_> = projected
        .models
        .into_iter()
        .map(|row| FleetUsageModelBucket {
            bucket: bucket_from(
                &row.bucket,
                completeness.models.get(&row.model).copied().unwrap_or(false),
            ),
            model: row.model,
        })
        .collect();
    sort_and_cap(&mut models, |row| &row.bucket);

    let mut projects: Vec<_> = projected
        .projects
        .into_iter()
        .map(|row| FleetUsageProjectBucket {
            bucket: bucket_from(
                &row.bucket,
                completeness.projects.get(&row.name).copied().unwrap_or(false),
            ),
            project: row.name,
            repo: row.repo,
        })
        .collect();
    sort_and_cap(&mut projects, |row| &row.bucket);

    // Session breakdowns.
    let mut sessions: Vec<_> = projected
        .sessions
        .into_iter()
        .map(|row| {
            let key = session_key(row.provider.as_str(), &row.project, &row.session_id);
            let priced = completeness.sessions.get(&key).copied().unwrap_or(false);
            FleetUsageSessionBucket {
                // The BARE session id. provider, project and session_id are
                // already three fields on this struct, so the composite added
                // nothing a client could use: it cannot even be re-split, since
                // a project label may itself contain a colon.
                session_id: row.session_id,
                provider: row.provider.as_str().to_string(),
                project: row.project,
                bucket: bucket_from(&row.bucket, priced),
            }
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
    let mut branches: Vec<_> = projected
        .branches
        .into_iter()
        .map(|row| {
            let priced = completeness.branches.get(&row.branch).copied().unwrap_or(false);
            FleetUsageBranchBucket {
                bucket: bucket_from(&row.bucket, priced),
                branch: row.branch,
            }
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
    let mut tools: Vec<_> = projected
        .tools
        .into_iter()
        .map(|row| FleetUsageNamedBucket {
            name: row.name,
            call_count: u64::try_from(row.calls).unwrap_or(u64::MAX),
        })
        .collect();
    tools.sort_by(|a, b| b.call_count.cmp(&a.call_count));
    tools.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    let mut mcp_servers: Vec<_> = projected
        .mcp_servers
        .into_iter()
        .map(|row| FleetUsageNamedBucket {
            name: row.name,
            call_count: u64::try_from(row.calls).unwrap_or(u64::MAX),
        })
        .collect();
    mcp_servers.sort_by(|a, b| b.call_count.cmp(&a.call_count));
    mcp_servers.truncate(FLEET_DASHBOARD_MAX_DIMENSION_BUCKETS);

    // Only the PROGRAM name leaves this module, never the command line. The
    // scanner keys these on the verbatim `input.command`, which routinely
    // carries absolute paths and can carry credentials, and this result is both
    // sent over the wire and written to a world-readable cache file. Shipping a
    // program name matches how `tools` ships a tool name and keeps the promise
    // in this module's header.
    let mut by_program: HashMap<String, u64> = HashMap::new();
    for row in projected.shell_commands {
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
        totals: Some(bucket_from(&projected.grand_total, cost_complete)),
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

    #[test]
    fn usage_windows_are_bounded_and_utc_aligned() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let (start, end) = window(FleetUsagePeriod::Trailing7Days, now);
        assert_eq!(start.to_rfc3339(), "2026-07-31T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-08-07T00:00:00+00:00");
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
    fn dashboard_from_usage_projects_all_dimensions() {
        use ainb_plugin_types_sessions::{
            BranchUsage, ModelUsage, NamedUsage, ProjectUsage, Provider, SessionUsage,
        };

        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let call_ts = now - Duration::hours(1);
        let bucket = TokenBucket {
            input_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 50,
            output_tokens: 200,
            reasoning_tokens: 0,
            call_count: 1,
            session_count: 1,
            project_count: 1,
            cost_usd: Some(0.005),
        };
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
        let usage = UsageData {
            daily: vec![(call_ts.date_naive(), bucket)],
            weekly: vec![(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap(), bucket)],
            projects: vec![ProjectUsage {
                name: "ainb".into(),
                path: "/repo".into(),
                bucket,
                repo: Some("stevengonsalvez/agents-in-a-box".into()),
            }],
            grand_total: bucket,
            calls: vec![call],
            sessions: vec![SessionUsage {
                provider: Provider::Claude,
                project: "ainb".into(),
                project_path: "/repo".into(),
                session_id: "s1".into(),
                first_timestamp: call_ts,
                last_timestamp: call_ts,
                bucket,
            }],
            models: vec![ModelUsage {
                model: "claude-sonnet-4-5".into(),
                bucket,
            }],
            activities: vec![],
            tools: vec![NamedUsage {
                name: "Read".into(),
                calls: 1,
            }],
            mcp_servers: vec![],
            shell_commands: vec![NamedUsage {
                name: "cargo test".into(),
                calls: 1,
            }],
            branches: vec![BranchUsage {
                branch: "feat/dash".into(),
                bucket,
            }],
            model_project_counts: vec![],
        };

        let result = dashboard_from_usage(&usage, now);

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

        let usage = UsageData {
            calls,
            ..UsageData::default()
        };
        let result = dashboard_from_usage(&usage, now);

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

        let usage = UsageData {
            calls: vec![
                call(1, "priced", "feat/priced", Some(0.01)),
                call(2, "unpriced", "feat/unpriced", None),
            ],
            ..UsageData::default()
        };
        let result = dashboard_from_usage(&usage, now);

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
        let usage = UsageData {
            calls: vec![
                call(1, "git commit -m 'secret /Users/someone/private'"),
                call(2, "git push origin main"),
                call(
                    3,
                    "curl -H 'Authorization: Bearer sk-live-xyz' https://api.example.com",
                ),
            ],
            ..UsageData::default()
        };
        let result = dashboard_from_usage(&usage, now);

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
        let usage = UsageData {
            calls: vec![
                call(1, this_week, Some(0.02)),
                call(2, last_week, Some(0.05)),
                call(3, last_week, None),
            ],
            ..UsageData::default()
        };
        let result = dashboard_from_usage(&usage, now);

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
