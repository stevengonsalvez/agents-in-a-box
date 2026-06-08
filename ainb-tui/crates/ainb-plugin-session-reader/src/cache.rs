//! Persistent per-file parse cache for the session-reader plugin.
//!
//! Each entry keys on the absolute path of a provider session file and
//! stores the parsed `Vec<ProviderCall>` plus the `(mtime, size)` tuple
//! the parser saw at the time of insertion. On the next scan the cache
//! short-circuits the parse when the on-disk file still has the same
//! `(mtime, size)`; on mismatch the parser is re-run and the row is
//! overwritten with a fresh fingerprint.
//!
//! The on-disk format is the simplest thing that satisfies the plugin
//! plan's Phase 5 contract — schema v1, FNV-1a 64 content fingerprint,
//! `PRAGMA user_version` migration. The richer offset / append-aware
//! cache in `ainb-core/src/usage_cache/` exists for the main TUI; the
//! plugin owns its own data dir per the `write_plugin_data` capability
//! and keeps the cache shape decoupled.
//!
//! ## Cache path
//!
//! `${XDG_DATA_HOME or ~/.local/share}/ainb/plugins/data/session-reader/usage.sqlite`
//!
//! `AINB_HOME` overrides both XDG_DATA_HOME and the home fallback so
//! tests can point the cache at a temp dir without leaking into a real
//! user profile.
//!
//! ## WASM
//!
//! Gated behind `#[cfg(not(target_arch = "wasm32"))]` as a precaution.
//! Subprocess plugins compile native today, but the cfg-gate keeps a
//! hypothetical wasm32 cdylib path from pulling rusqlite.

#![cfg(not(target_arch = "wasm32"))]
// `insert`/`clear` take `&mut self` to model exclusive write access at
// the type level, even though `rusqlite::Connection::execute` itself
// only requires `&Connection`. Future schema migrations expect to
// mutate the connection state, so the API contract is forward-looking.
#![allow(clippy::needless_pass_by_ref_mut)]
// `u64` fingerprint stored in an `INTEGER` column via `from_ne_bytes`
// is an intentional bit-pattern reinterpretation, not a numeric cast.
#![allow(clippy::cast_possible_wrap)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ainb_plugin_types_sessions::ProviderCall;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::fnv::fnv1a_64;

/// How long a writer waits for a competing writer/checkpoint before SQLite
/// returns `SQLITE_BUSY`. Two ainb instances share one `usage.sqlite`, so an
/// overlapping scan's write would otherwise collide instantly and the caller
/// falls back to a cache-less (re-parsing) scan — doubling CPU. 5s comfortably
/// covers a scan's write burst.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Retry a SQLite op when it fails with `BUSY`/`LOCKED`. `busy_timeout` already
/// blocks for [`BUSY_TIMEOUT`] per attempt; this is a small bounded backstop
/// for the rare case it still surfaces (e.g. a WAL checkpoint stall under two
/// concurrent writers). Bounded so a genuinely wedged DB still surfaces an error.
fn with_busy_retry<T>(mut op: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    use rusqlite::ffi::ErrorCode;
    let mut attempt: u32 = 0;
    loop {
        let result = op();
        if let Err(rusqlite::Error::SqliteFailure(err, _)) = &result {
            if matches!(
                err.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) && attempt < 3
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50 * u64::from(attempt)));
                continue;
            }
        }
        return result;
    }
}

/// Current on-disk schema version. Bumping this triggers the v0→vN
/// migration block in [`UsageCache::migrate`].
pub const SCHEMA_VERSION: i64 = 1;

/// Cache error surface.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// I/O error opening the DB file or its parent directory.
    #[error("cache I/O: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite connection / query / migration failure.
    #[error("cache sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    /// bincode encode/decode failure on the parsed-calls blob.
    #[error("cache encode: {0}")]
    Encode(#[from] bincode::Error),
}

/// Persistent per-file parse cache.
///
/// A single connection per scan is fine — the plugin's scanner walks
/// providers in series and the connection is not shared across threads.
pub struct UsageCache {
    conn: Connection,
}

impl UsageCache {
    /// Open (or create) the cache DB at `path`. Applies pragmas and
    /// runs any pending schema migrations.
    ///
    /// On any I/O or SQL failure the caller is expected to log and
    /// fall back to a cache-less parse — never let cache failure break
    /// the scan.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // WAL keeps reads non-blocking and lets the writer proceed
        // without a global lock — the cache is read-mostly with bursts
        // of writes during a fresh scan. `synchronous=NORMAL` is the
        // WAL-recommended balance: durable across crashes, not power
        // loss, which matches a derived cache.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        // Block (up to BUSY_TIMEOUT) instead of erroring when another instance
        // holds the write lock, so concurrent scans serialize rather than
        // dropping to a cache-less reparse.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Look up a cache row. Returns `Some` only when (a) a row exists
    /// for `path` and (b) both `mtime` and `size` match exactly.
    ///
    /// Mismatch on either field misses — the caller re-parses and
    /// `insert`s the fresh result, overwriting the stale row.
    pub fn lookup(
        &self,
        path: &str,
        mtime: u64,
        size: u64,
    ) -> Result<Option<Vec<ProviderCall>>, CacheError> {
        let row: Option<(i64, i64, Vec<u8>)> = with_busy_retry(|| {
            self.conn
                .query_row(
                    "SELECT mtime, size, parsed FROM file_cache WHERE path = ?1",
                    params![path],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    },
                )
                .optional()
        })?;

        let Some((stored_mtime, stored_size, blob)) = row else {
            return Ok(None);
        };

        // Cast through u64 because the column is INTEGER (i64) but
        // the caller hands us u64 — both `mtime_nanos` and `size`
        // fit comfortably for any real file.
        if stored_mtime as u64 != mtime || stored_size as u64 != size {
            return Ok(None);
        }

        let calls: Vec<ProviderCall> = bincode::deserialize(&blob)?;
        Ok(Some(calls))
    }

    /// Upsert a cache row. `calls` is bincode-serialized; the FNV-1a 64
    /// fingerprint is computed over the same bytes the caller used to
    /// drive the parse (typically the file contents).
    pub fn insert(
        &mut self,
        path: &str,
        mtime: u64,
        size: u64,
        fingerprint: u64,
        calls: &[ProviderCall],
    ) -> Result<(), CacheError> {
        let blob = bincode::serialize(calls)?;
        let cached_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        with_busy_retry(|| {
            self.conn.execute(
                "INSERT INTO file_cache (path, mtime, size, fingerprint, parsed, cached_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET
                    mtime       = excluded.mtime,
                    size        = excluded.size,
                    fingerprint = excluded.fingerprint,
                    parsed      = excluded.parsed,
                    cached_at   = excluded.cached_at",
                params![
                    path,
                    i64::try_from(mtime).unwrap_or(i64::MAX),
                    i64::try_from(size).unwrap_or(i64::MAX),
                    i64::from_ne_bytes(fingerprint.to_ne_bytes()),
                    blob,
                    cached_at,
                ],
            )
        })?;
        Ok(())
    }

    /// Drop every cached row. Schema row (PRAGMA user_version) is
    /// preserved so re-opens don't trigger another migration.
    pub fn clear(&mut self) -> Result<(), CacheError> {
        self.conn.execute("DELETE FROM file_cache", [])?;
        // VACUUM is a tidiness operation; failure here doesn't matter.
        if let Err(err) = self.conn.execute("VACUUM", []) {
            tracing::warn!("session-reader cache VACUUM after clear failed: {err}");
        }
        Ok(())
    }

    /// Apply v0→v1 schema migration using `PRAGMA user_version`.
    /// Future bumps append additional `if version < N` blocks before
    /// the final `pragma_update`.
    fn migrate(conn: &Connection) -> Result<(), CacheError> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0);

        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS file_cache (
                     path        TEXT    PRIMARY KEY,
                     mtime       INTEGER NOT NULL,
                     size        INTEGER NOT NULL,
                     fingerprint INTEGER NOT NULL,
                     parsed      BLOB    NOT NULL,
                     cached_at   INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_mtime ON file_cache(mtime);",
            )?;
        }

        if version != SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }
}

/// Compute the FNV-1a 64 fingerprint of `bytes`. Wrapper over
/// [`crate::fnv::fnv1a_64`] so the cache module doesn't leak the
/// helper through callers' import paths.
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> u64 {
    fnv1a_64(bytes)
}

/// Resolve the default cache DB path:
/// `${AINB_HOME or XDG_DATA_HOME or ~/.local/share}/ainb/plugins/data/session-reader/usage.sqlite`.
///
/// `AINB_HOME` is honored first specifically so tests can point the
/// cache at a temp dir without touching the real user profile (the
/// flag also documents itself as a test-only override).
#[must_use]
pub fn default_db_path() -> Option<PathBuf> {
    let base = if let Some(home) = std::env::var_os("AINB_HOME") {
        PathBuf::from(home)
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home).join(".local").join("share")
    };
    Some(
        base.join("ainb")
            .join("plugins")
            .join("data")
            .join("session-reader")
            .join("usage.sqlite"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_types_sessions::{Provider, ProviderCall};
    use chrono::{DateTime, Utc};
    use tempfile::TempDir;

    fn fake_call(id: u64) -> ProviderCall {
        ProviderCall {
            id,
            provider: Provider::Claude,
            model: "claude-sonnet".into(),
            session_id: "s".into(),
            project: "p".into(),
            project_path: "/tmp/p".into(),
            timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            input_tokens: 10,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 20,
            reasoning_tokens: 0,
            cost_usd: Some(0.001),
            tools: vec!["Read".into()],
            bash_commands: vec![],
            user_message: "hi".into(),
            branch: Some("main".into()),
        }
    }

    fn open_in_tmp() -> (TempDir, UsageCache) {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("usage.sqlite");
        let cache = UsageCache::open(&db).expect("open cache");
        (dir, cache)
    }

    #[test]
    fn open_creates_parent_dirs_and_db() {
        let dir = TempDir::new().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("usage.sqlite");
        let _cache = UsageCache::open(&nested).expect("open cache");
        assert!(nested.exists(), "DB file created at {nested:?}");
    }

    #[test]
    fn insert_then_lookup_hits_for_same_mtime_and_size() {
        let (_dir, mut cache) = open_in_tmp();
        let calls = vec![fake_call(1), fake_call(2)];
        cache.insert("/tmp/a.jsonl", 42, 100, 0xDEADBEEF, &calls).expect("insert");

        let hit = cache.lookup("/tmp/a.jsonl", 42, 100).expect("lookup").expect("hit");
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0].id, 1);
        assert_eq!(hit[1].id, 2);
    }

    #[test]
    fn lookup_misses_when_mtime_differs() {
        let (_dir, mut cache) = open_in_tmp();
        cache.insert("/tmp/a.jsonl", 42, 100, 0, &[fake_call(1)]).expect("insert");

        let miss = cache.lookup("/tmp/a.jsonl", 43, 100).expect("lookup");
        assert!(miss.is_none(), "mtime mismatch must miss");
    }

    #[test]
    fn lookup_misses_when_size_differs() {
        let (_dir, mut cache) = open_in_tmp();
        cache.insert("/tmp/a.jsonl", 42, 100, 0, &[fake_call(1)]).expect("insert");

        let miss = cache.lookup("/tmp/a.jsonl", 42, 101).expect("lookup");
        assert!(miss.is_none(), "size mismatch must miss");
    }

    #[test]
    fn lookup_misses_for_unknown_path() {
        let (_dir, cache) = open_in_tmp();
        let miss = cache.lookup("/tmp/never.jsonl", 0, 0).expect("lookup");
        assert!(miss.is_none());
    }

    #[test]
    fn fingerprint_persists_through_insert() {
        let (_dir, mut cache) = open_in_tmp();
        let fp = fingerprint(b"some-file-bytes");
        cache.insert("/tmp/a.jsonl", 1, 2, fp, &[fake_call(1)]).expect("insert");

        // Read fingerprint back out via raw SQL to assert it was
        // stored byte-stably (reinterpret cast through ne_bytes).
        let stored: i64 = cache
            .conn
            .query_row(
                "SELECT fingerprint FROM file_cache WHERE path = ?1",
                params!["/tmp/a.jsonl"],
                |row| row.get(0),
            )
            .expect("select");
        let recovered = u64::from_ne_bytes(stored.to_ne_bytes());
        assert_eq!(recovered, fp);
    }

    #[test]
    fn migrate_sets_user_version_to_one() {
        let (_dir, cache) = open_in_tmp();
        let version: i64 = cache
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn migrate_creates_idx_mtime_index() {
        let (_dir, cache) = open_in_tmp();
        let count: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_mtime'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(count, 1, "idx_mtime index exists");
    }

    #[test]
    fn reopen_is_idempotent() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("usage.sqlite");
        {
            let mut c = UsageCache::open(&db).expect("open 1");
            c.insert("/p", 1, 2, 3, &[fake_call(7)]).expect("insert");
        }
        {
            let c = UsageCache::open(&db).expect("open 2");
            let hit = c.lookup("/p", 1, 2).expect("lookup").expect("hit");
            assert_eq!(hit[0].id, 7);
        }
    }

    #[test]
    fn clear_drops_rows_but_preserves_schema_version() {
        let (_dir, mut cache) = open_in_tmp();
        cache.insert("/tmp/a.jsonl", 1, 1, 0, &[fake_call(1)]).expect("insert");
        cache.clear().expect("clear");
        let miss = cache.lookup("/tmp/a.jsonl", 1, 1).expect("lookup");
        assert!(miss.is_none(), "clear drops rows");

        let version: i64 = cache
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(version, SCHEMA_VERSION, "clear preserves schema version");
    }

    #[test]
    fn insert_overwrites_stale_row() {
        let (_dir, mut cache) = open_in_tmp();
        cache.insert("/tmp/a.jsonl", 1, 1, 0, &[fake_call(1)]).expect("insert v1");
        cache
            .insert("/tmp/a.jsonl", 2, 2, 0, &[fake_call(2), fake_call(3)])
            .expect("insert v2");

        let hit = cache.lookup("/tmp/a.jsonl", 2, 2).expect("lookup").expect("hit");
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0].id, 2);
        assert_eq!(hit[1].id, 3);

        // Original (mtime=1, size=1) row is gone.
        let stale = cache.lookup("/tmp/a.jsonl", 1, 1).expect("lookup");
        assert!(stale.is_none());
    }

    #[test]
    fn concurrent_writers_serialize_without_busy_error() {
        // Two ainb instances share one usage.sqlite. With WAL + busy_timeout,
        // overlapping writers must serialize instead of erroring SQLITE_BUSY
        // (which would drop the scan to a cache-less reparse). Without the
        // busy_timeout pragma this test flakes/panics under contention.
        use std::sync::Arc;
        let dir = TempDir::new().expect("tempdir");
        let db = Arc::new(dir.path().join("usage.sqlite"));
        // Seed the DB so both threads open an existing WAL file.
        drop(UsageCache::open(&db).expect("seed open"));

        let handles: Vec<_> = (0..2u64)
            .map(|t| {
                let db = Arc::clone(&db);
                std::thread::spawn(move || {
                    let mut cache = UsageCache::open(&db).expect("open under contention");
                    for i in 0..200u64 {
                        let path = format!("/tmp/t{t}-{i}.jsonl");
                        cache
                            .insert(&path, i, i, i, &[fake_call(i)])
                            .expect("insert under contention");
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("writer thread");
        }

        // Both writers' rows landed.
        let cache = UsageCache::open(&db).expect("reopen");
        assert!(cache.lookup("/tmp/t0-199.jsonl", 199, 199).expect("lookup").is_some());
        assert!(cache.lookup("/tmp/t1-199.jsonl", 199, 199).expect("lookup").is_some());
    }

    // `default_db_path` reads `AINB_HOME` / `XDG_DATA_HOME` / `HOME`
    // directly via `std::env::var_os`. The crate is `#[forbid(unsafe_code)]`
    // so we can't mutate the process env from a test; the override
    // path is exercised by the host's integration tests where the
    // plugin is spawned with `AINB_HOME` already set in the
    // subprocess environment.
    #[test]
    fn default_db_path_returns_expected_suffix_when_home_set() {
        if std::env::var_os("HOME").is_none()
            && std::env::var_os("XDG_DATA_HOME").is_none()
            && std::env::var_os("AINB_HOME").is_none()
        {
            return;
        }
        let resolved = default_db_path().expect("path");
        assert!(
            resolved.ends_with("ainb/plugins/data/session-reader/usage.sqlite"),
            "resolved path has expected suffix: {resolved:?}"
        );
    }
}
