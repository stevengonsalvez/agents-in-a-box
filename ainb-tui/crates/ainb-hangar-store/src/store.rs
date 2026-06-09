//! The [`Store`]: the owned `SQLite` connection pool plus its migration
//! lifecycle.
//!
//! Every consumer (daemon, services, repositories) reads and writes through the
//! single pool a `Store` hands out via [`Store::pool`].

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::apply_migrations;

/// File name of the Hangar database within its home directory.
const DB_FILE_NAME: &str = "hangar.db";

/// Sub-directory under the real `$HOME` that holds the Hangar database on the
/// default (no-override) path. Not appended when [`Store::home_env`] override is
/// set — the override value is the database directory verbatim.
const HANGAR_DIR: &str = ".ainb";

/// Environment variable that overrides the resolved Hangar database directory.
/// When set (and non-empty), [`Store::open_default`] treats its value as the
/// directory that DIRECTLY holds the database — the file lives at
/// `$AINB_HANGAR_HOME/hangar.db`, with NO `.ainb` segment appended. The `.ainb`
/// sub-directory is only used on the real-`$HOME` fallback path. This contract
/// is the one the P0.7 daemon tripwire asserts (`docs/hangar/phases/P0.md:222`).
/// Used by tests via the `with_isolated_home` helper to keep the real `$HOME`
/// untouched.
const HOME_ENV: &str = "AINB_HANGAR_HOME";

/// Owned `SQLite` connection pool plus the migrations applied to it.
///
/// A `Store` is the single entry point to Hangar persistence. Construct it once
/// at boot ([`Store::open_default`]) or per-test ([`Store::open_in`]); clone the
/// returned [`SqlitePool`] freely via [`Store::pool`] — `sqlx` pools are cheap
/// to share and internally reference-counted.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if absent) the Hangar database inside `dir`, enable `WAL`
    /// journaling, and apply all embedded migrations.
    ///
    /// The database file is always `dir/hangar.db`. `WAL` (write-ahead logging)
    /// is chosen because Hangar has one long-lived daemon writer plus many
    /// short-lived TUI readers; `WAL` lets readers proceed without blocking the
    /// writer, which the default rollback journal cannot do. The on-disk shape
    /// (epoch-ms integers, no `SQLite`-only column types in the schema) keeps a
    /// future Postgres backend a near-drop-in replacement.
    ///
    /// `dir` is created (recursively, if absent) before the database is opened,
    /// so callers need not pre-create the tree. `SQLite`'s
    /// `create_if_missing(true)` only creates the database *file*, not its
    /// parent directory; this method covers the directory too for symmetry with
    /// [`Store::open_default`].
    ///
    /// # Errors
    ///
    /// Returns an error if `dir` cannot be created, the pool cannot be opened
    /// (e.g. `dir` is not writable) or if a migration fails to apply.
    pub async fn open_in(dir: &Path) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(dir).await?;
        let db_path = dir.join(DB_FILE_NAME);
        let opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().connect_with(opts).await?;
        apply_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Open the Hangar database at its default location and apply migrations.
    ///
    /// The database directory is resolved as:
    /// 1. `$AINB_HANGAR_HOME` if set and non-empty — the db lives DIRECTLY at
    ///    `$AINB_HANGAR_HOME/hangar.db` (no `.ainb` segment). This is the
    ///    contract the P0.7 daemon tripwire asserts.
    /// 2. otherwise `~/.ainb/hangar.db` via [`dirs::home_dir`].
    ///
    /// The directory is created (via [`Store::open_in`]) if it does not yet
    /// exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be resolved or created, or if
    /// [`Store::open_in`] fails.
    pub async fn open_default() -> anyhow::Result<Self> {
        let dir = Self::default_dir()?;
        Self::open_in(&dir).await
    }

    /// Borrow the underlying `SQLite` connection pool.
    #[must_use]
    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Resolve the directory that should contain `hangar.db`.
    ///
    /// If `$AINB_HANGAR_HOME` is set and non-empty, it IS the database directory
    /// verbatim (no `.ainb` segment appended) — this matches the P0.7 daemon
    /// tripwire contract. An empty `$AINB_HANGAR_HOME` is ignored (it would
    /// otherwise resolve to a relative path in the current working directory),
    /// falling back to `~/.ainb`. The real-`$HOME` fallback nests the database
    /// under the `.ainb` sub-directory.
    fn default_dir() -> anyhow::Result<PathBuf> {
        match std::env::var_os(HOME_ENV).filter(|p| !p.is_empty()) {
            Some(p) => Ok(PathBuf::from(p)),
            None => Ok(dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
                .join(HANGAR_DIR)),
        }
    }

    /// The environment variable that overrides the Hangar home directory.
    ///
    /// Exposed so the test-support helper and callers stay in sync on the exact
    /// variable name without duplicating the literal.
    #[must_use]
    pub const fn home_env() -> &'static str {
        HOME_ENV
    }
}
