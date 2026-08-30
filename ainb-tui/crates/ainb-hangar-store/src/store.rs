//! The [`Store`]: the owned `SQLite` connection pool plus its migration
//! lifecycle.
//!
//! Every consumer (daemon, services, repositories) reads and writes through the
//! single pool a `Store` hands out via [`Store::pool`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::apply_migrations;

/// File name of the Hangar database within its home directory.
const DB_FILE_NAME: &str = "hangar.db";

/// Environment variable that overrides the resolved Hangar database directory.
/// When set (and non-empty), [`Store::open_default`] treats its value as the
/// directory that DIRECTLY holds the database — the file lives at
/// `$AINB_HANGAR_HOME/hangar.db`, with NO `.agents-in-a-box` segment appended. The `.agents-in-a-box`
/// sub-directory is only used on the real-`$HOME` fallback path. This contract
/// is the one the P0.7 daemon tripwire asserts (`docs/hangar/phases/P0.md:222`).
/// Used by tests via the `with_isolated_home` helper to keep the real `$HOME`
/// untouched.
///
/// Re-exported from [`ainb_hangar_core::paths::HANGAR_HOME_ENV`] so the literal
/// lives in exactly one place alongside the resolver that reads it.
const HOME_ENV: &str = ainb_hangar_core::paths::HANGAR_HOME_ENV;

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
            // Foreign keys are load-bearing, so DECLARE the dependency rather than
            // inheriting sqlx's default. `agent.runtime_id` (and every other
            // `REFERENCES`) is only a real constraint with this on: it is what stops
            // a runtime being renamed out from under its agents, and what several
            // repos' "the engine enforces the parent link" reasoning rests on. An
            // sqlx default change would otherwise silently turn every FK inert with
            // nothing going red — see `fk_enforcement_is_on_and_rejects_orphans`.
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            // WAL + `NORMAL` fsync is the standard durable-enough tuning: the
            // default `FULL` fsyncs on EVERY commit, so under the daemon's many
            // concurrent writer loops (claim FSM, sweepers, attention ingest,
            // event-outbox drain, RPC mutations) the write lock is held across a
            // disk flush per commit — long enough on a slow/loaded CI host that a
            // best-effort secondary write (the board card auto-move, an attention
            // insert) can exhaust its `busy_timeout` and get swallowed. `NORMAL`
            // only fsyncs at checkpoint, cutting lock-hold time sharply. Pair it
            // with an explicit, generous `busy_timeout` so a contended writer
            // WAITS for the lock instead of erroring `SQLITE_BUSY`.
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(10));
        let pool = SqlitePoolOptions::new().connect_with(opts).await?;
        backup_before_pending_migrations(&pool, &db_path).await?;
        apply_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Open the Hangar database at its default location and apply migrations.
    ///
    /// The database directory is resolved as:
    /// 1. `$AINB_HANGAR_HOME` if set and non-empty — the db lives DIRECTLY at
    ///    `$AINB_HANGAR_HOME/hangar.db` (no `.agents-in-a-box` segment). This is the
    ///    contract the P0.7 daemon tripwire asserts.
    /// 2. otherwise `~/.agents-in-a-box/hangar.db` via the shared
    ///    [`ainb_hangar_core::hangar_home`] resolver.
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

    /// Check whether the existing default database accepts a read-only query.
    ///
    /// Unlike [`Self::open_default`], this never creates a database and never
    /// applies migrations. Status callers use it while another daemon may own
    /// the database, so migration ownership stays with daemon startup.
    pub async fn ping_default_read_only() -> anyhow::Result<()> {
        let dir = Self::default_dir()?;
        Self::ping_in_read_only(&dir).await
    }

    /// Read every `daemon_config` row without creating or migrating anything.
    ///
    /// [`Self::open_default`] runs `backup_before_pending_migrations` (a whole
    /// `VACUUM INTO` copy) and then `apply_migrations`. Doing that from a read
    /// — the TUI's first tick, `ainb config show` — migrates a running
    /// daemon's live schema out from under it after an upgrade, and blocks the
    /// caller for the length of the copy. Reads use this instead; migration
    /// ownership stays with daemon startup.
    ///
    /// `Ok(None)` when there is no database yet, so callers show defaults.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be resolved, or if an existing
    /// database cannot be read.
    pub async fn read_daemon_config_read_only() -> anyhow::Result<Option<Vec<(String, String)>>> {
        use sqlx::{ConnectOptions as _, Connection as _};

        let db_path = Self::default_dir()?.join(DB_FILE_NAME);
        if !db_path.exists() {
            return Ok(None);
        }
        let mut conn = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(false)
            .read_only(true)
            .connect()
            .await?;
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM daemon_config")
            .fetch_all(&mut conn)
            .await?;
        conn.close().await?;
        Ok(Some(rows))
    }

    async fn ping_in_read_only(dir: &Path) -> anyhow::Result<()> {
        use sqlx::ConnectOptions as _;

        let db_path = dir.join(DB_FILE_NAME);
        let mut conn = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(false)
            .read_only(true)
            .connect()
            .await?;
        sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&mut conn).await?;
        use sqlx::Connection as _;
        conn.close().await?;
        Ok(())
    }

    /// Borrow the underlying `SQLite` connection pool.
    #[must_use]
    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Resolve the directory that should contain `hangar.db`.
    ///
    /// Delegates to [`ainb_hangar_core::hangar_home`] for the home contract: if
    /// `$AINB_HANGAR_HOME` is set and non-empty, it IS the database directory
    /// verbatim (no `.agents-in-a-box` segment appended) — this matches the P0.7
    /// daemon tripwire contract. An empty `$AINB_HANGAR_HOME` is ignored (it
    /// would otherwise resolve to a relative path in the current working
    /// directory), falling back to `~/.agents-in-a-box`.
    fn default_dir() -> anyhow::Result<PathBuf> {
        ainb_hangar_core::hangar_home()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))
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

/// Makes each in-flight snapshot's path unique per CALL, not just per process:
/// two `Store::open_in`s can run concurrently inside one process (daemon boot
/// plus an in-process reader).
static STAGING_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many `hangar.db.pre-<version>.bak` files survive a boot. Two is the
/// last-known-good plus the one before it; more just eats the disk the daemon
/// runs on.
const BACKUP_KEEP: usize = 2;

/// Snapshot the database aside BEFORE any pending migration touches it.
///
/// Migrations here are forward-only with no down files, so restoring this file
/// is the ONLY rollback that exists. The backup is therefore a precondition of
/// migrating, not best effort: a failure to write it aborts the open rather
/// than walking through the one-way door unprotected.
///
/// Every opener races here, because `Store::open_in` is on the daemon boot
/// path AND on every `ainb hangar *` CLI invocation. Three properties keep the
/// artifact trustworthy under that race:
///
/// 1. `VACUUM INTO` writes a consistent snapshot of THIS connection's read
///    view. Unlike a file copy it cannot interleave with another process's
///    writes, and it needs no checkpoint (a `wal_checkpoint(TRUNCATE)` that
///    reports `busy = 1` did not run, which would have left the copy's tail in
///    `-wal`).
/// 2. The snapshot is re-opened and its own `MAX(version)` asserted to still be
///    `applied`. A process that read `applied` and then blocked on the
///    `busy_timeout` while another migrated would otherwise file a POST-upgrade
///    database under the `pre-<applied>` name, and restoring it would be a
///    silent no-op that looks correct.
/// 3. It lands under a caller-unique temp name and is `rename`d into place, so
///    the visible `.bak` is always a complete file and concurrent openers
///    cannot interleave into one another's output.
///
/// No-ops when nothing is pending (the ordinary boot) and on a fresh database
/// (no `_sqlx_migrations` table yet: there is no prior state to preserve).
/// Every OTHER failure to establish the pre-upgrade state aborts the open:
/// "cannot read the version" and "there is no version" are different facts, and
/// treating the first as the second migrates a populated database with no
/// rollback behind it.
async fn backup_before_pending_migrations(pool: &SqlitePool, db_path: &Path) -> anyhow::Result<()> {
    let applied =
        match sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await
        {
            Ok(applied) => applied,
            Err(error) if is_fresh_database(&error) => return Ok(()),
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context("could not read the applied schema version; refusing to migrate"));
            }
        };
    let Some(applied) = applied else {
        return Ok(()); // the table exists but no migration has ever applied
    };
    let embedded = sqlx::migrate!("./migrations").iter().map(|m| m.version).max().unwrap_or(0);
    if applied >= embedded {
        return Ok(());
    }

    // Prune to one slot BELOW the retention target first: pruning after the
    // snapshot would need room for three databases at once, and a host short on
    // disk would get a daemon that refuses to boot on the upgrade path.
    prune_backups(db_path, BACKUP_KEEP.saturating_sub(1));
    let backup = db_path.with_file_name(format!("{DB_FILE_NAME}.pre-{applied}.bak"));
    let staging = db_path.with_file_name(format!(
        "{DB_FILE_NAME}.pre-{applied}.{}-{}.tmp",
        std::process::id(),
        STAGING_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = tokio::fs::remove_file(&staging).await; // VACUUM INTO refuses an existing file
    let staging_arg = staging
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("backup path is not valid UTF-8: {}", staging.display()))?;
    sqlx::query("VACUUM INTO ?").bind(staging_arg).execute(pool).await?;

    let read = snapshot_applied_version(&staging).await.map_err(|error| error.to_string());
    match snapshot_verdict(applied, read) {
        Verdict::Keep => {}
        Verdict::Discard { observed } => {
            let _ = tokio::fs::remove_file(&staging).await;
            tracing::warn!(
                from_version = applied,
                observed,
                "pre-migration backup discarded: the database moved under it"
            );
            return Ok(());
        }
        Verdict::Broken { reason } => {
            let _ = tokio::fs::remove_file(&staging).await;
            anyhow::bail!(
                "pre-migration backup for version {applied} could not be verified ({reason}); \
                 refusing to migrate without a rollback"
            );
        }
    }
    tokio::fs::rename(&staging, &backup).await?;
    tracing::info!(
        backup = %backup.display(),
        from_version = applied,
        to_version = embedded,
        "pre-migration database backup written"
    );
    prune_backups(db_path, BACKUP_KEEP);
    Ok(())
}

/// The ONE read failure that means "fresh database" rather than "broken
/// database": `sqlx` has not created `_sqlx_migrations` yet.
///
/// Everything else (a corrupt file, an I/O fault, a `_sqlx_migrations` whose
/// shape no longer answers the query) is a database whose pre-upgrade state we
/// failed to establish, and the caller aborts on it. The distinction matters
/// because the two are indistinguishable at the call site once the error is
/// discarded, and guessing "fresh" is the guess that skips the backup.
fn is_fresh_database(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.message().contains("no such table: _sqlx_migrations"))
}

/// What a just-written snapshot's own schema version means for the backup.
enum Verdict {
    /// The snapshot holds `applied`: file it under the pre-upgrade name.
    Keep,
    /// The snapshot holds a DIFFERENT version. Another opener migrated while
    /// this one was queued behind the `busy_timeout`; its own backup is the
    /// pre-upgrade one, and filing this post-upgrade snapshot under the
    /// pre-upgrade name would make a restore a silent no-op. Discard and boot.
    Discard {
        /// The version the snapshot actually holds.
        observed: i64,
    },
    /// The snapshot cannot be read back at all, or carries no schema version.
    /// That is a broken artifact rather than a race, and migrating on the
    /// strength of it walks through the one-way door with no rollback.
    Broken {
        /// Operator-facing cause.
        reason: String,
    },
}

/// Classify the snapshot re-read.
///
/// Split out from [`backup_before_pending_migrations`] because the three
/// outcomes are otherwise only reachable by racing a live `VACUUM INTO`, and
/// the two that are NOT `Keep` mean opposite things: one is a healthy
/// concurrent opener, the other is a backup that does not exist.
fn snapshot_verdict(applied: i64, read: Result<Option<i64>, String>) -> Verdict {
    match read {
        Ok(Some(version)) if version == applied => Verdict::Keep,
        Ok(Some(observed)) => Verdict::Discard { observed },
        Ok(None) => Verdict::Broken {
            reason: "it holds no schema version".to_string(),
        },
        Err(reason) => Verdict::Broken { reason },
    }
}

/// Re-open a snapshot on its own and read the schema version it actually holds.
/// Read-only and `create_if_missing(false)`, so a missing or corrupt file is an
/// error rather than a freshly minted empty database that would "verify".
async fn snapshot_applied_version(path: &Path) -> anyhow::Result<Option<i64>> {
    use sqlx::{ConnectOptions as _, Connection as _};

    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true)
        .connect()
        .await?;
    let version: Option<i64> = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&mut conn)
        .await?;
    conn.close().await?;
    Ok(version)
}

/// Keep only the newest `keep` pre-migration backups beside the database.
///
/// Best effort: a stale extra file is harmless, a failed boot is not.
fn prune_backups(db_path: &Path, keep: usize) {
    let Some(dir) = db_path.parent() else {
        return;
    };
    let prefix = format!("{DB_FILE_NAME}.pre-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<(i64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let version = name.strip_prefix(&prefix)?.strip_suffix(".bak")?.parse().ok()?;
            Some((version, path))
        })
        .collect();
    backups.sort_unstable_by_key(|(version, _)| std::cmp::Reverse(*version));
    for (_, path) in backups.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Foreign keys MUST be enforced. The whole runtime-identity design rests on
    /// it (`agent.runtime_id` is an FK, which is why a runtime cannot be renamed),
    /// as does several repos' "the engine enforces the parent link" reasoning.
    ///
    /// This is pinned in CI deliberately: it was previously only an inherited sqlx
    /// default that no test asserted, and a comment claiming the OPPOSITE ("PRAGMA
    /// foreign_keys is off in this crate") survived long enough to justify a real
    /// bug. An sqlx default change — or a stray `.foreign_keys(false)` — must fail
    /// here rather than silently turning every `REFERENCES` into a decoration.
    #[tokio::test]
    async fn fk_enforcement_is_on_and_rejects_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // (a) The pragma itself is ON.
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.unwrap();
        assert_eq!(fk, 1, "PRAGMA foreign_keys must be ON");

        // (b) It is actually ENFORCED: an agent naming a nonexistent runtime (and
        //     workspace/owner) is rejected, not silently persisted as an orphan.
        let err = sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES ('a-orphan', 'ws-nope', 'orphan', 'rt-nope', 'workspace', 'u-nope')",
        )
        .execute(pool)
        .await
        .expect_err("an agent with a dangling runtime_id FK must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("FOREIGN KEY constraint failed"),
            "expected an FK violation, got: {msg}"
        );
    }

    #[tokio::test]
    async fn read_only_ping_never_creates_or_migrates_a_database() {
        let missing = tempfile::tempdir().unwrap();
        let missing_db = missing.path().join(DB_FILE_NAME);
        assert!(Store::ping_in_read_only(missing.path()).await.is_err());
        assert!(!missing_db.exists());

        let ready = tempfile::tempdir().unwrap();
        let store = Store::open_in(ready.path()).await.unwrap();
        Store::ping_in_read_only(ready.path()).await.unwrap();
        drop(store);
    }

    /// A version query that fails for any reason OTHER than "the table is not
    /// there yet" must abort the open. Read as "fresh database" it would skip
    /// the pre-migration backup and walk a populated database through the
    /// forward-only door with nothing to restore from.
    ///
    /// Simulated by shadowing `_sqlx_migrations` with a VIEW that has no
    /// `version` column: the table is present under that name, the query is
    /// the real one, and it fails deterministically with `no such column`.
    #[tokio::test]
    async fn an_unreadable_schema_version_aborts_the_open() {
        let dir = tempfile::tempdir().unwrap();
        // A real, fully migrated database first, so the only thing wrong on the
        // next open is the version query itself.
        drop(Store::open_in(dir.path()).await.expect("first open"));

        let opts = SqliteConnectOptions::new().filename(dir.path().join(DB_FILE_NAME));
        let pool = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
        sqlx::query("ALTER TABLE _sqlx_migrations RENAME TO _sqlx_migrations_data")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE VIEW _sqlx_migrations AS SELECT description FROM _sqlx_migrations_data",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = Store::open_in(dir.path())
            .await
            .expect_err("an unreadable schema version must abort, not look fresh");
        let chain = format!("{error:#}");
        assert!(
            chain.contains("could not read the applied schema version"),
            "the abort must name its cause, got: {chain}"
        );
        assert!(
            !dir.path().join(format!("{DB_FILE_NAME}.pre-0.bak")).exists(),
            "a database that could not be read must not be filed as a version-0 backup"
        );
    }

    /// The verdict split: only a version that MOVED is the concurrent-opener
    /// case that keeps booting. A snapshot that cannot be read back, or that
    /// carries no version at all, is a MISSING backup and must abort.
    #[test]
    fn only_a_moved_version_is_discarded_the_rest_abort() {
        assert!(matches!(snapshot_verdict(7, Ok(Some(7))), Verdict::Keep));
        assert!(matches!(
            snapshot_verdict(7, Ok(Some(8))),
            Verdict::Discard { observed: 8 }
        ));
        assert!(matches!(
            snapshot_verdict(7, Ok(None)),
            Verdict::Broken { .. }
        ));
        assert!(matches!(
            snapshot_verdict(7, Err("disk I/O error".to_string())),
            Verdict::Broken { .. }
        ));
    }

    /// The `Broken` verdict is reachable from a real file, not just from a
    /// hand-built `Err`: a staging path that is not a database (a torn
    /// `VACUUM INTO`, a truncated write) and one that is not there at all both
    /// fail the re-read rather than verifying as an empty database.
    #[tokio::test]
    async fn a_snapshot_that_is_not_a_database_fails_its_re_read() {
        let dir = tempfile::tempdir().unwrap();
        let torn = dir.path().join("hangar.db.pre-7.torn.tmp");
        std::fs::write(&torn, b"not a sqlite file").unwrap();
        assert!(
            snapshot_applied_version(&torn).await.is_err(),
            "a non-database staging file must not read as a verified snapshot"
        );
        assert!(
            snapshot_applied_version(&dir.path().join("absent.tmp")).await.is_err(),
            "create_if_missing(false) must make an absent snapshot an error"
        );
    }
}
