//! Generic Fetcher trait and shared error / record types.

use std::path::{Path, PathBuf};

use ainb_skill_core::Uri;
use serde::{Deserialize, Serialize};

/// Errors raised by fetchers.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("unsupported source type `{0}` for this fetcher")]
    UnsupportedSourceType(String),

    #[error("network access disabled (set AINB_TEST_NETWORK=1 to enable)")]
    NetworkDisabled,

    #[error("URI has no usable ref/path component: {0}")]
    UriShape(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("git: {0}")]
    Git(String),

    #[error("http: {0}")]
    Http(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// One successful fetch. `path` is where the content materialized on
/// disk; `resolved_sha` keys the cache (real commit SHA for git, content
/// digest for http, pseudo-SHA from mtime+path for local).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedRef {
    pub path: PathBuf,
    pub resolved_sha: String,
    /// ISO-8601 UTC timestamp.
    pub fetched_at: String,
}

/// Generic fetcher contract. Implementations decide which `source_type`
/// values they handle and return `FetchError::UnsupportedSourceType`
/// for anything else.
pub trait Fetcher: Send + Sync {
    /// Resolve `uri` into `cache_root`, scoped by `source_name`.
    fn fetch(
        &self,
        uri: &Uri,
        source_name: &str,
        cache_root: &Path,
    ) -> Result<FetchedRef, FetchError>;
}

/// Convenience: ISO-8601 UTC timestamp of "now" suitable for
/// [`FetchedRef::fetched_at`].
pub fn now_utc_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Avoid pulling in chrono just for one timestamp — format manually.
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = gregorian_from_days_since_epoch(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn gregorian_from_days_since_epoch(days: i64) -> (i32, u32, u32) {
    // Convert days since 1970-01-01 (Unix epoch) into (Y, M, D).
    // Algorithm from Howard Hinnant's date library — public domain.
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gregorian_known_dates() {
        // 1970-01-01 → day 0
        assert_eq!(gregorian_from_days_since_epoch(0), (1970, 1, 1));
        // 2000-01-01 → 10957 days after epoch
        assert_eq!(gregorian_from_days_since_epoch(10_957), (2000, 1, 1));
        // 2024-02-29 (leap) → 19782
        assert_eq!(gregorian_from_days_since_epoch(19_782), (2024, 2, 29));
    }

    #[test]
    fn now_utc_iso8601_has_z_suffix() {
        let ts = now_utc_iso8601();
        assert!(ts.ends_with('Z'), "got: {ts}");
        assert_eq!(ts.len(), 20, "got: {ts}");
    }
}
