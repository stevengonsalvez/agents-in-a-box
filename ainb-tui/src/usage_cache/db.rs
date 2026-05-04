// ABOUTME: SQLite connection + schema management for the usage cache.
// Schema is content-addressable by version; bumping `SCHEMA_VERSION` drops &
// rebuilds the DB. Acceptable because this is a derived cache, never source of
// truth.

//! Sqlite connection + schema lifecycle for the persistent usage cache.

use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;

use super::store::CacheError;

/// Bumping this drops & rebuilds the DB on next open.
pub const SCHEMA_VERSION: i64 = 1;

/// `files.blob_format` value for the current bincode-encoded
/// `Vec<ProviderCall>` blob shape (default options).
///
/// Renamed from `BLOB_FORMAT_BINCODE_V1` because the discriminant value drifts
/// (currently 2, was 1) every time `ProviderCall` layout changes. The constant
/// names the *current* on-disk format, not a fixed schema generation — the
/// numeric value is the version, the constant name is its role.
///
/// History:
///   1 — original 14-field `ProviderCall`.
///   2 — added `branch: Option<String>` (PR-D, branch attribution).
///   3 — `timestamp` migrated from `DateTime<Local>` to `DateTime<Utc>` so
///       caches survive timezone moves / DST transitions, and a stable
///       `id: u64` field added (hash of `path:offset`) so `analyze_turns`
///       results can be precomputed once on the unfiltered call set.
pub const BLOB_FORMAT_BINCODE_CURRENT: i64 = 3;

/// Open (or create) the usage-cache sqlite DB at `path`, applying schema and
/// performance pragmas. Caller owns the resulting connection — wrap in a
/// `Mutex` for cross-thread access.
pub fn open(path: &Path) -> Result<Connection, CacheError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CacheError::Io)?;
    }

    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    // WAL gives us non-blocking readers + a single durable writer, which is
    // exactly the cache's access pattern (TUI reads, occasional background
    // writes). `synchronous=NORMAL` is the WAL-recommended balance: durable
    // across crashes, not across power loss — fine for a derived cache.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    ensure_schema(&conn)?;
    Ok(conn)
}

/// Ensure the cache DB matches `SCHEMA_VERSION`. Drops & recreates on mismatch.
fn ensure_schema(conn: &Connection) -> Result<(), CacheError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)")?;

    let current: Option<i64> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .ok();

    if current == Some(SCHEMA_VERSION) {
        return Ok(());
    }

    // Drop everything but `schema_version` itself, then recreate.
    conn.execute_batch(
        "DROP TABLE IF EXISTS files;
         DELETE FROM schema_version;",
    )?;

    conn.execute_batch(
        "CREATE TABLE files (
             path           TEXT    PRIMARY KEY,
             size_bytes     INTEGER NOT NULL,
             mtime_nanos    INTEGER NOT NULL,
             last_offset    INTEGER NOT NULL,
             suffix_blake3  BLOB    NOT NULL,
             row_count      INTEGER NOT NULL,
             calls_blob     BLOB    NOT NULL,
             blob_format    INTEGER NOT NULL,
             updated_at     INTEGER NOT NULL
         );
         CREATE INDEX idx_files_updated_at ON files(updated_at);",
    )?;

    conn.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        params![SCHEMA_VERSION],
    )?;

    Ok(())
}
