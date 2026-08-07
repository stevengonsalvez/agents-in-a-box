//! Bounded live provider-quota projection for Fleet.
//!
//! Claude writes authoritative quota windows through its statusline hook and
//! Codex writes them through its stop-hook pull. Hangar reads those small,
//! provider-produced caches, retains only the safe projection, and never leaks
//! auth or statusline files through RPC.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ainb_hangar_proto::fleet::{
    FleetQuotaProvider, FleetQuotaSummaryResult, FleetQuotaWindow, FleetUsageSummaryState,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_VERSION: u32 = 1;
const PROVIDER_CACHE_VERSION: u32 = 1;
const SOURCE_MAX_AGE: Duration = Duration::from_secs(10 * 60);
const LAST_KNOWN_SOURCE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceWindow {
    pct: u8,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceCache {
    version: u32,
    updated_at: String,
    #[serde(default)]
    five_hour: Option<SourceWindow>,
    #[serde(default)]
    seven_day: Option<SourceWindow>,
    #[serde(default)]
    plan_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedQuota {
    version: u32,
    summary: FleetQuotaSummaryResult,
}

#[derive(Default)]
struct State {
    cached: Option<CachedQuota>,
    refreshing: bool,
    last_error: Option<String>,
}

/// Daemon-owned quota cache with a single coalesced reader.
pub struct QuotaService {
    cache_path: PathBuf,
    state: Mutex<State>,
}

impl QuotaService {
    /// Load a durable quota projection, if one exists.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        let cache_path = home.join("hangar").join("fleet-quota.json");
        Self {
            state: Mutex::new(State {
                cached: load_cache(&cache_path),
                ..State::default()
            }),
            cache_path,
        }
    }

    async fn summary(self: &Arc<Self>) -> FleetQuotaSummaryResult {
        let (summary, should_refresh) = {
            let state = self.state.lock().await;
            let summary = state.cached.as_ref().map(|cached| {
                let mut summary = cached.summary.clone();
                if state.refreshing || state.last_error.is_some() {
                    summary.state = FleetUsageSummaryState::Partial;
                    summary.detail = Some(match &state.last_error {
                        Some(error) => format!("using last known quota: {error}"),
                        None => "refreshing quota in background".to_string(),
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
                    Some(detail) => format!("{detail}; refreshing quota in background"),
                    None => "refreshing quota in background".to_string(),
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

    /// Coalesce concurrent source-cache reads into one background worker.
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
            let result = tokio::task::spawn_blocking(read_live_quota)
                .await
                .map_err(|error| format!("quota worker failed: {error}"))
                .and_then(|result| result);
            let mut state = service.state.lock().await;
            state.refreshing = false;
            match result {
                Ok(summary) => {
                    let cached = CachedQuota {
                        version: CACHE_VERSION,
                        summary,
                    };
                    if let Err(error) = write_cache(&service.cache_path, &cached) {
                        state.last_error =
                            Some(format!("could not persist quota snapshot: {error}"));
                    } else {
                        state.cached = Some(cached);
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

static ACTIVE_SERVICE: OnceLock<tokio::sync::RwLock<Option<Arc<QuotaService>>>> = OnceLock::new();

fn active_slot() -> &'static tokio::sync::RwLock<Option<Arc<QuotaService>>> {
    ACTIVE_SERVICE.get_or_init(|| tokio::sync::RwLock::new(None))
}

/// Install quota refresh before Fleet opens its RPC socket.
pub async fn install(home: &Path) {
    let service = Arc::new(QuotaService::new(home));
    service.request_refresh().await;
    service.spawn_poller();
    *active_slot().write().await = Some(service);
}

/// Refresh on hook activity. Repeated hook events join the in-flight reader.
pub async fn request_refresh() {
    if let Some(service) = active_slot().read().await.clone() {
        service.request_refresh().await;
    }
}

/// Return only the daemon-owned quota projection.
pub async fn summary() -> FleetQuotaSummaryResult {
    match active_slot().read().await.clone() {
        Some(service) => service.summary().await,
        None => unavailable("quota service is not initialized".to_string()),
    }
}

fn read_live_quota() -> Result<FleetQuotaSummaryResult, String> {
    let now = Utc::now();
    let cache_dir = dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .ok_or_else(|| "provider cache directory is unavailable".to_string())?
        .join("ainb");

    let claude_path = cache_dir.join("live.json");
    let codex_path = cache_dir.join("codex-live.json");
    let claude = read_provider(&claude_path, "claude", now, SOURCE_MAX_AGE);
    let codex = read_provider(&codex_path, "codex", now, SOURCE_MAX_AGE);
    let fresh = [claude.is_some(), codex.is_some()];
    // No request is made with an expired statusline cache. Retain a bounded
    // last-known projection so a first Fleet launch still explains the gap.
    let providers = [
        claude.or_else(|| read_provider(&claude_path, "claude", now, LAST_KNOWN_SOURCE_MAX_AGE)),
        codex.or_else(|| read_provider(&codex_path, "codex", now, LAST_KNOWN_SOURCE_MAX_AGE)),
    ];
    let available: Vec<_> = providers.iter().filter_map(Clone::clone).collect();
    if available.is_empty() {
        return Err("no fresh provider quota cache is available".to_string());
    }
    let unavailable: Vec<_> = ["Claude", "Codex"]
        .into_iter()
        .zip(providers.iter())
        .filter_map(|(name, provider)| provider.is_none().then_some(name))
        .collect();
    let stale: Vec<_> = ["Claude", "Codex"]
        .into_iter()
        .zip(fresh.into_iter().zip(providers.iter()))
        .filter_map(|(name, (is_fresh, provider))| {
            (!is_fresh && provider.is_some()).then_some(name)
        })
        .collect();
    let mut detail = Vec::new();
    if !stale.is_empty() {
        detail.push(format!("{} quota is last known", stale.join(" and ")));
    }
    if !unavailable.is_empty() {
        detail.push(format!("{} quota unavailable", unavailable.join(" and ")));
    }
    Ok(FleetQuotaSummaryResult {
        state: if fresh.iter().all(|is_fresh| *is_fresh) {
            FleetUsageSummaryState::Ready
        } else {
            FleetUsageSummaryState::Partial
        },
        generated_at: Some(now.timestamp_millis()),
        providers: available,
        detail: (!detail.is_empty()).then(|| detail.join("; ")),
    })
}

fn read_provider(
    path: &Path,
    provider: &str,
    now: DateTime<Utc>,
    max_age: Duration,
) -> Option<FleetQuotaProvider> {
    let source: SourceCache = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if source.version != PROVIDER_CACHE_VERSION {
        return None;
    }
    let updated_at = DateTime::parse_from_rfc3339(&source.updated_at).ok()?.with_timezone(&Utc);
    let age = now.signed_duration_since(updated_at);
    if age.num_seconds() < 0 || age.to_std().ok()? > max_age {
        return None;
    }
    let window = |window: SourceWindow| FleetQuotaWindow {
        used_percent: window.pct.min(100),
        resets_at: window
            .resets_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis()),
        estimated: false,
    };
    Some(FleetQuotaProvider {
        provider: provider.to_string(),
        five_hour: source.five_hour.map(window),
        seven_day: source.seven_day.map(window),
        plan_type: source.plan_type,
        updated_at: Some(updated_at.timestamp_millis()),
    })
}

fn load_cache(path: &Path) -> Option<CachedQuota> {
    let cached: CachedQuota = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (cached.version == CACHE_VERSION).then_some(cached)
}

fn write_cache(path: &Path, cached: &CachedQuota) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::other("quota cache has no parent directory"));
    };
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec(cached).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

fn scanning() -> FleetQuotaSummaryResult {
    FleetQuotaSummaryResult {
        state: FleetUsageSummaryState::Scanning,
        generated_at: None,
        providers: Vec::new(),
        detail: Some("reading provider quota caches".to_string()),
    }
}

fn unavailable(detail: String) -> FleetQuotaSummaryResult {
    FleetQuotaSummaryResult {
        state: FleetUsageSummaryState::Unavailable,
        generated_at: None,
        providers: Vec::new(),
        detail: Some(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_provider_cache_is_not_presented_as_live_quota() {
        let source = SourceCache {
            version: PROVIDER_CACHE_VERSION,
            updated_at: "2026-08-07T10:00:00Z".to_string(),
            five_hour: Some(SourceWindow {
                pct: 42,
                resets_at: None,
            }),
            seven_day: None,
            plan_type: None,
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.json");
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        let now = "2026-08-07T10:11:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(read_provider(&path, "claude", now, SOURCE_MAX_AGE).is_none());
        assert!(read_provider(&path, "claude", now, LAST_KNOWN_SOURCE_MAX_AGE).is_some());
    }
}
