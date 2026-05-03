// ABOUTME: High-level cache API: open, clear, info. Fingerprint-aware
// get_or_parse lands in a follow-on commit alongside fingerprint.rs.

//! High-level cache API surface.
//!
//! Lifecycle primitives: open / disabled / clear / `clear_path` / info /
//! `default_db_path`, plus the fingerprint-aware `get_or_parse` hot path
//! that drives the analytics scan.

use std::fs::File;
use std::io::Seek;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::models::usage::ProviderCall;

use super::db;
use super::fingerprint::{FileFingerprint, FingerprintAction, classify, verify_append_safe};

/// Versioned blob encoding for `files.calls_blob`. Discriminant must match
/// `db::BLOB_FORMAT_BINCODE_V1` — bump both together when `ProviderCall`
/// layout changes, and update the deserializer match below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum BlobFormat {
    /// Current `bincode::serialize` of `Vec<ProviderCall>` with default
    /// options. Version 2 replaced version 1 when `ProviderCall.branch`
    /// (Option<String>) was added in PR-D.
    Bincode = 2,
}

impl BlobFormat {
    pub(crate) const fn from_i64(v: i64) -> Option<Self> {
        match v {
            // 1 was the pre-branch layout. Rows tagged 1 are stale on
            // upgrade and intentionally skipped — the next scan re-parses
            // and rewrites them at version 2.
            2 => Some(Self::Bincode),
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
    pub const fn is_enabled(&self) -> bool {
        matches!(self.inner, CacheInner::Open(_))
    }

    /// On-disk path of the cache DB.
    #[allow(dead_code)]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Get cached calls for `path` if the file is unchanged; otherwise call
    /// `parse` with the appropriate [`ParseHint`] and persist the result.
    ///
    /// On any cache error we fall back to a full parse with `ParseHint::Full`
    /// — the cache is never allowed to break analytics.
    pub fn get_or_parse<F>(&self, path: &Path, mut parse: F) -> Vec<ProviderCall>
    where
        F: FnMut(&Path, ParseHint<'_>) -> ParseResult,
    {
        match self.try_get_or_parse(path, &mut parse) {
            Ok(calls) => calls,
            Err(err) => {
                tracing::warn!(
                    "usage_cache fallback to full re-parse for {:?}: {err}",
                    path
                );
                parse(path, ParseHint::Full).calls
            }
        }
    }

    fn try_get_or_parse<F>(
        &self,
        path: &Path,
        parse: &mut F,
    ) -> Result<Vec<ProviderCall>, CacheError>
    where
        F: FnMut(&Path, ParseHint<'_>) -> ParseResult,
    {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(parse(path, ParseHint::Full).calls);
        };

        let prior = {
            let lock = conn.lock().unwrap_or_else(|e| {
            tracing::warn!("usage_cache mutex poisoned; recovering");
            e.into_inner()
        });
            load_row(&lock, path)?
        };

        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if prior.is_some() {
                    let lock = conn.lock().unwrap_or_else(|e| {
            tracing::warn!("usage_cache mutex poisoned; recovering");
            e.into_inner()
        });
                    let _ = lock
                        .execute("DELETE FROM files WHERE path = ?1", params![path.to_string_lossy().into_owned()]);
                }
                return Ok(Vec::new());
            }
            Err(err) => return Err(CacheError::Io(err)),
        };
        let meta = file.metadata().map_err(CacheError::Io)?;
        let current = FileFingerprint::from_open_file(&mut file, &meta)?;

        let (action, prior_calls, prior_suffix, _prior_offset) = match prior {
            Some(row) => {
                let action = classify(&row.fingerprint, &current, row.last_offset);
                (
                    action,
                    Some(row.calls),
                    Some(row.fingerprint.suffix_blake3),
                    row.last_offset,
                )
            }
            None => (FingerprintAction::FullReparse, None, None, 0),
        };

        let result = match action {
            FingerprintAction::Unchanged => {
                return Ok(prior_calls.unwrap_or_default());
            }
            FingerprintAction::Append { from_offset } => {
                let safe = match prior_suffix {
                    Some(suffix) => verify_append_safe(&mut file, from_offset, &suffix)?,
                    None => false,
                };
                file.rewind().map_err(CacheError::Io)?;
                drop(file);
                if safe {
                    let existing = prior_calls.as_deref().unwrap_or(&[]);
                    parse(
                        path,
                        ParseHint::Append {
                            from_offset,
                            existing,
                        },
                    )
                } else {
                    parse(path, ParseHint::Full)
                }
            }
            FingerprintAction::FullReparse => {
                drop(file);
                parse(path, ParseHint::Full)
            }
        };

        let row = StoredRow {
            fingerprint: current,
            last_offset: result.end_offset,
            calls: result.calls,
        };
        if let Err(err) = self.write_row(path, &row) {
            tracing::warn!("usage_cache row write failed for {:?}: {err}", path);
        }
        Ok(row.calls)
    }

    fn write_row(&self, path: &Path, row: &StoredRow) -> Result<(), CacheError> {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(());
        };
        let blob = bincode::serialize(&row.calls)?;
        let lock = conn.lock().unwrap_or_else(|e| {
            tracing::warn!("usage_cache mutex poisoned; recovering");
            e.into_inner()
        });
        upsert_row(&lock, path, row, &blob)?;
        Ok(())
    }

    /// Drop all cached rows. Schema row is preserved.
    pub fn clear(&self) -> Result<(), CacheError> {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(());
        };
        let lock = conn.lock().unwrap_or_else(|e| {
            tracing::warn!("usage_cache mutex poisoned; recovering");
            e.into_inner()
        });
        lock.execute("DELETE FROM files", [])?;
        Ok(())
    }

    /// Drop a single cache row by path.
    #[allow(dead_code)]
    pub fn clear_path(&self, path: &Path) -> Result<(), CacheError> {
        let CacheInner::Open(conn) = &self.inner else {
            return Ok(());
        };
        let lock = conn.lock().unwrap_or_else(|e| {
            tracing::warn!("usage_cache mutex poisoned; recovering");
            e.into_inner()
        });
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
        let lock = conn.lock().unwrap_or_else(|e| {
            tracing::warn!("usage_cache mutex poisoned; recovering");
            e.into_inner()
        });
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

/// Hint passed to user parsers. The parser gets to decide what to do — the
/// cache merely tells it what's safe.
#[derive(Debug, Clone)]
pub enum ParseHint<'a> {
    /// No cache row, or fingerprint forced a full re-parse.
    Full,
    /// Append-only parse: skip the first `from_offset` bytes; existing calls
    /// from the prior run are in `existing` (parser must extend, not
    /// replace).
    Append {
        from_offset: u64,
        existing: &'a [crate::models::usage::ProviderCall],
    },
}

/// Result the caller's parser must produce: the complete `Vec<ProviderCall>`
/// for the file plus the offset at which it stopped reading.
pub struct ParseResult {
    pub calls: Vec<crate::models::usage::ProviderCall>,
    /// Byte offset in the file where parsing stopped. Almost always
    /// `metadata().len()`; used to drive the next append-only parse.
    pub end_offset: u64,
}

#[derive(Debug)]
struct StoredRow {
    fingerprint: FileFingerprint,
    last_offset: u64,
    calls: Vec<crate::models::usage::ProviderCall>,
}

#[derive(Debug)]
struct LoadedRow {
    fingerprint: FileFingerprint,
    last_offset: u64,
    calls: Vec<crate::models::usage::ProviderCall>,
}

fn load_row(conn: &Connection, path: &Path) -> Result<Option<LoadedRow>, CacheError> {
    let path_str = path.to_string_lossy().into_owned();
    let row = conn
        .query_row(
            "SELECT size_bytes, mtime_nanos, last_offset, suffix_blake3, calls_blob, blob_format
             FROM files WHERE path = ?1",
            params![path_str],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;

    let Some((size_i, mtime, offset, suffix, blob, fmt)) = row else {
        return Ok(None);
    };

    let Some(format) = BlobFormat::from_i64(fmt) else {
        return Ok(None);
    };
    let calls: Vec<crate::models::usage::ProviderCall> = match format {
        BlobFormat::Bincode => bincode::deserialize(&blob)?,
    };
    let fingerprint = FileFingerprint::from_columns(
        u64::try_from(size_i).unwrap_or(0),
        mtime,
        &suffix,
    )?;

    Ok(Some(LoadedRow {
        fingerprint,
        last_offset: u64::try_from(offset).unwrap_or(0),
        calls,
    }))
}

fn upsert_row(
    conn: &Connection,
    path: &Path,
    row: &StoredRow,
    blob: &[u8],
) -> Result<(), CacheError> {
    let path_str = path.to_string_lossy().into_owned();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO files
            (path, size_bytes, mtime_nanos, last_offset, suffix_blake3,
             row_count, calls_blob, blob_format, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(path) DO UPDATE SET
            size_bytes    = excluded.size_bytes,
            mtime_nanos   = excluded.mtime_nanos,
            last_offset   = excluded.last_offset,
            suffix_blake3 = excluded.suffix_blake3,
            row_count     = excluded.row_count,
            calls_blob    = excluded.calls_blob,
            blob_format   = excluded.blob_format,
            updated_at    = excluded.updated_at",
        params![
            path_str,
            i64::try_from(row.fingerprint.size_bytes).unwrap_or(i64::MAX),
            row.fingerprint.mtime_nanos,
            i64::try_from(row.last_offset).unwrap_or(i64::MAX),
            row.fingerprint.suffix_blake3.to_vec(),
            i64::try_from(row.calls.len()).unwrap_or(i64::MAX),
            blob,
            db::BLOB_FORMAT_BINCODE_V1,
            now,
        ],
    )?;
    Ok(())
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
