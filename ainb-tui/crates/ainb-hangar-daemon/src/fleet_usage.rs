//! Bounded, daemon-owned Fleet Usage projection.
//!
//! Provider logs and canonical model-rate parsing stay behind this module. The
//! public RPC receives only aggregates, never paths, transcripts, or calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration as StdDuration, SystemTime};

use ainb_hangar_proto::fleet::{
    FLEET_USAGE_MAX_BREAKDOWN_BUCKETS, FLEET_USAGE_MAX_DAILY_BUCKETS, FleetUsageBucket,
    FleetUsageDailyBucket, FleetUsageModelBucket, FleetUsagePeriod, FleetUsageProjectBucket,
    FleetUsageProviderBucket, FleetUsageSummaryResult, FleetUsageSummaryState,
};
use ainb_plugin_session_reader::scanner::{self, ProviderRoots};
use ainb_plugin_types_sessions::{ProviderCall, TokenBucket, UsageData};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_VERSION: u32 = 1;
const REFRESH_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);

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
    let (start, _) = window(FleetUsagePeriod::Trailing30Days, now);
    let since = SystemTime::UNIX_EPOCH
        + StdDuration::from_millis(u64::try_from(start.timestamp_millis()).unwrap_or_default());
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
    models: HashMap<String, bool>,
    projects: HashMap<String, bool>,
}

fn completeness(calls: &[ProviderCall]) -> CostCompleteness {
    let mut result = CostCompleteness::default();
    for call in calls {
        let priced = call.cost_usd.is_some();
        mark(&mut result.daily, call.timestamp.date_naive(), priced);
        mark(&mut result.models, call.model.clone(), priced);
        mark(&mut result.projects, call.project.clone(), priced);
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
