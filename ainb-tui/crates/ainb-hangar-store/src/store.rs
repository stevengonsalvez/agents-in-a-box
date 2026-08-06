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
async fn backup_before_pending_migrations(pool: &SqlitePool, db_path: &Path) -> anyhow::Result<()> {
    let applied: Result<Option<i64>, sqlx::Error> =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await;
    let Ok(Some(applied)) = applied else {
        return Ok(()); // fresh database, or no migration ever applied
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

    let snapshot_version = snapshot_applied_version(&staging).await;
    match snapshot_version {
        Ok(Some(version)) if version == applied => {}
        other => {
            // Another opener migrated while this one was queued behind the
            // busy_timeout: its own backup is the pre-upgrade one, and keeping
            // this post-upgrade snapshot under the pre-upgrade name would make
            // a restore a silent no-op.
            let _ = tokio::fs::remove_file(&staging).await;
            tracing::warn!(
                from_version = applied,
                observed = ?other.ok().flatten(),
                "pre-migration backup discarded: the database moved under it"
            );
            return Ok(());
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
}
