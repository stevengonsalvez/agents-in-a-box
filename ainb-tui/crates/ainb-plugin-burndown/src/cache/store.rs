// ABOUTME: No-op usage-cache stub.
//
// The plugin's persistent SQLite cache was retired in Phase 3: at runtime
// the host loads UsageData via its own host-side cache and pushes the
// snapshot to the plugin via the `burndown.usage_data` event. The plugin
// never opens its own DB. The surface here exists only to keep the
// in-process parser (`parse_usage_for_*`) compiling — every call falls
// through to a full re-parse with no persistence.

//! No-op cache surface. All operations succeed; nothing is persisted.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::data::usage::ProviderCall;

/// Cache error surface (kept for API compatibility — never returned by
/// the no-op implementation).
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache schema: {0}")]
    Schema(String),
}

/// Versioned blob encoding for cached call rows. Retained as a stable
/// constant so callers that referenced `BlobFormat::BincodeV3` still
/// compile; the value is no longer written anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum BlobFormat {
    BincodeV3 = 3,
}

/// Snapshot of cache contents (`ainb usage cache info`). Always reports
/// zero now that the cache is a no-op.
#[derive(Debug, Clone)]
pub struct CacheInfo {
    pub db_path: PathBuf,
    pub size_bytes: u64,
    pub file_count: i64,
    pub oldest_updated_at: Option<i64>,
}

/// No-op cache. Public surface stays compatible with the previous
/// SQLite-backed implementation; every call full-parses and never
/// persists.
pub struct Cache {
    db_path: PathBuf,
}

impl Cache {
    /// Open a "cache" at `db_path`. The path is recorded for `info()`
    /// reporting only — no DB is opened.
    pub fn open(db_path: PathBuf) -> Result<Self, CacheError> {
        Ok(Self { db_path })
    }

    /// Construct a disabled cache. Identical to `open()` in behaviour
    /// post-stub; the distinct constructor exists for callers that want
    /// to make their intent explicit.
    pub fn disabled() -> Self {
        Self {
            db_path: PathBuf::new(),
        }
    }

    /// Reports false — cache is never enabled.
    pub const fn is_enabled(&self) -> bool {
        false
    }

    /// On-disk path of the (notional) cache DB.
    #[allow(dead_code)]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Always full-parse via `parse(path, ParseHint::Full)`.
    pub fn get_or_parse<F>(&self, path: &Path, mut parse: F) -> Vec<ProviderCall>
    where
        F: FnMut(&Path, ParseHint<'_>) -> ParseResult,
    {
        parse(path, ParseHint::Full).calls
    }

    /// No-op: nothing to clear.
    pub fn clear(&self) -> Result<(), CacheError> {
        Ok(())
    }

    /// No-op: nothing to clear.
    #[allow(dead_code)]
    pub fn clear_path(&self, _path: &Path) -> Result<(), CacheError> {
        Ok(())
    }

    /// Always reports an empty cache.
    pub fn info(&self) -> Result<CacheInfo, CacheError> {
        Ok(CacheInfo {
            db_path: self.db_path.clone(),
            size_bytes: 0,
            file_count: 0,
            oldest_updated_at: None,
        })
    }
}

/// Resolve the default cache DB path (`~/.agents-in-a-box/cache/usage.db`)
/// honoring the `AINB_USAGE_CACHE_DB` env override. Retained for the
/// `usage cache info` CLI path's reporting; nothing is written there.
pub fn default_db_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AINB_USAGE_CACHE_DB") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("cache").join("usage.db"))
}

/// Hint passed to user parsers. Always `Full` post-stub.
#[derive(Debug, Clone)]
pub enum ParseHint<'a> {
    /// No cache row, or fingerprint forced a full re-parse.
    Full,
    /// Append-only parse (unused post-stub; retained for API compat).
    Append {
        from_offset: u64,
        existing: &'a [crate::data::usage::ProviderCall],
    },
}

/// Result the caller's parser must produce.
pub struct ParseResult {
    pub calls: Vec<crate::data::usage::ProviderCall>,
    pub end_offset: u64,
}
