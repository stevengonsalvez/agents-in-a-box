// ABOUTME: High-level cache API: open, clear, info. Fingerprint-aware
// get_or_parse lands in a follow-on commit alongside fingerprint.rs.

//! High-level cache API surface.
//!
//! This commit lands the lifecycle primitives: open/disabled, clear,
//! clear_path, info, default_db_path. The fingerprint-aware
//! `get_or_parse` is added in the next commit together with
//! [`super::fingerprint`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use super::db;

/// Versioned blob encoding for `files.calls_blob`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum BlobFormat {
    /// `bincode::serialize` of `Vec<ProviderCall>` with default options.
    Bincode = 1,
}

impl BlobFormat {
    pub(crate) fn from_i64(v: i64) -> Option<Self> {
        match v {
            1 => Some(Self::Bincode),
            _ => None,
        }
    }
}

/// Cache error surface. `Io`/`Sql` propagate underlying errors so callers can
/// decide whether to log-and-fall-back-to-full-parse or surface to the user.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("cache encode: {0}")]
    Encode(#[from] bincode::Error),
    #[error("cache schema: {0}")]
    Schema(String),
}

/// Snapshot of cache contents for `ainb usage cache info`.
#[derive(Debug, Clone)]
pub struct CacheInfo {
    pub db_path: PathBuf,
    pub size_bytes: u64,
    pub file_count: i64,
    pub oldest_updated_at: Option<i64>,
}

/// Persistent usage cache.
///
/// Internally a `Mutex<Connection>`. Reads are sub-millisecond (single-row
/// PK lookup) so we don't bother with a separate read-only pool.
pub struct Cache {
    pub(crate) inner: CacheInner,
    pub(crate) db_path: PathBuf,
}

pub(crate) enum CacheInner {
    Open(Mutex<Connection>),
    /// Cache disabled — every `get_or_parse` performs a full parse and the
    /// result is *not* persisted. Used by `--no-cache` on the CLI.
    Disabled,
}

impl Cache {
    /// Open (or create) the cache DB at `db_path`. Returns an error on schema
    /// or I/O failure — callers in the parse path should log and fall back to
    /// a `disabled()` cache so analytics still work.
    pub fn open(db_path: PathBuf) -> Result<Self, CacheError> {
        let conn = db::open(&db_path)?;
        Ok(Self {
            inner: CacheInner::Open(Mutex::new(conn)),
            db_path,
        })
    }

    /// Construct a no-op cache. `get_or_parse` always full-parses and never
    /// writes. `db_path` is informational only.
    pub fn disabled() -> Self {
        Self {
            inner: CacheInner::Disabled,
            db_path: PathBuf::new(),
        }
    }

    /// Whether cache writes are enabled.
    pub fn is_enabled(&self) -> bool {
        matches!(self.inner, CacheInner::Open(_))
    }

    /// On-disk path of the cache DB.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Drop all cached rows. Schema row is preserved.
    pub fn clear(&self) -> Result<(), CacheError> {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(());
        };
        let lock = conn.lock().expect("usage_cache mutex poisoned");
        lock.execute("DELETE FROM files", [])?;
        Ok(())
    }

    /// Drop a single cache row by path.
    pub fn clear_path(&self, path: &Path) -> Result<(), CacheError> {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(());
        };
        let lock = conn.lock().expect("usage_cache mutex poisoned");
        let path_str = path.to_string_lossy().into_owned();
        lock.execute("DELETE FROM files WHERE path = ?1", params![path_str])?;
        Ok(())
    }

    /// Return summary stats for the cache (`ainb usage cache info`).
    pub fn info(&self) -> Result<CacheInfo, CacheError> {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(CacheInfo {
                db_path: self.db_path.clone(),
                size_bytes: 0,
                file_count: 0,
                oldest_updated_at: None,
            });
        };
        let lock = conn.lock().expect("usage_cache mutex poisoned");
        let file_count: i64 =
            lock.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let oldest_updated_at: Option<i64> = lock
            .query_row("SELECT MIN(updated_at) FROM files", [], |row| row.get(0))
            .optional()?
            .flatten();
        let size_bytes = std::fs::metadata(&self.db_path).map(|m| m.len()).unwrap_or(0);
        Ok(CacheInfo {
            db_path: self.db_path.clone(),
            size_bytes,
            file_count,
            oldest_updated_at,
        })
    }
}

/// Resolve the default cache DB path (`~/.agents-in-a-box/cache/usage.db`)
/// honoring the `AINB_USAGE_CACHE_DB` env override (used by tests).
pub fn default_db_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AINB_USAGE_CACHE_DB") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("cache").join("usage.db"))
}

/// Internal helper for tests / diagnostics: stamp `updated_at` into a row
/// after manual fixturing. Not part of the public API.
#[allow(dead_code)]
pub(crate) fn stamp_updated_at_now(conn: &Connection, path: &Path) -> Result<(), CacheError> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let path_str = path.to_string_lossy().into_owned();
    conn.execute(
        "UPDATE files SET updated_at = ?1 WHERE path = ?2",
        params![now, path_str],
    )?;
    Ok(())
}
