//! Hangar persistence layer.
//!
//! Owns the `SQLite` schema (via embedded [`sqlx`] migrations) and the
//! repository wrappers that the daemon and services build on. This crate is the
//! single source of truth for the on-disk shape of `~/.agents-in-a-box/hangar.db`.
//!
//! # Migrations
//!
//! Migrations live in `migrations/` and follow the frozen naming convention
//! `NNNN_<terse_slug>.sql` (4-digit zero-padded ordinal, `snake_case` slug, no
//! `_up`/`_down` suffix — `SQLite` migrations are forward-only here). They are
//! embedded at compile time by [`sqlx::migrate!`], which on stable Rust
//! cannot register `migrations/` as a build dependency on its own; `build.rs`
//! emits `cargo:rerun-if-changed=migrations` so cargo rebuilds this crate
//! whenever a `.sql` file is added, edited, or removed. If a migration edit
//! is somehow still not picked up, that is a `build.rs` regression, not
//! expected behavior (`cargo clean -p ainb-hangar-store` remains the escape
//! hatch, but should not be needed).
//!
//! ## An applied migration file is IMMUTABLE — including its comments
//!
//! `sqlx` records a checksum of each migration's FULL FILE TEXT in
//! `_sqlx_migrations`. Editing an already-applied file — even a comment — changes
//! that checksum, and the next boot against an existing database aborts with
//! `migration N was previously applied but has been modified`, i.e. the daemon
//! refuses to start on every home that ever booted. Corrections to an applied
//! migration's prose therefore CANNOT be made in-file; document the correction
//! here (or in the owning repo module) instead. Only brand-new migrations are
//! editable, and only until they ship.
//!
//! ## D3 (migration 0053) is REVERSED by migration 0074
//!
//! `0053_squad_role_instructions.sql` states, as "DEVIATION (D3)", that
//! `squad_member.role` does NOT gate dispatch: that `SquadRepo::member_agent_ids`
//! stays role-blind, that every agent member is dispatched to regardless of role,
//! and that selective routing is deferred to parity item #16.
//!
//! **That is no longer true, and 0053's file cannot say so** (an applied
//! migration's text is frozen, see above). Dispatch IS role-gated as of migration
//! `0074_role_gated_pull_pipeline`: `role` is matched against
//! `board_column.services_role` in
//! [`service::pull::PullService`], and
//! [`service::squad_assign::SquadAssignService::assign_fanout`] no longer emits
//! one task per member at all. The old broadcast was the reported defect, not a
//! feature: one issue became N simultaneous runs, each in its own worktree, all
//! doing the same work.
//!
//! `member_agent_ids` itself is still role-blind, deliberately. It answers the
//! narrower question "which agents are in this squad", which is what the leader
//! briefing and the explicit `--redundant N` fan-out want. Role SELECTION lives
//! in the pull predicate.
//!
//! ## Foreign keys ARE enforced
//!
//! Several applied migration comments (0009/0010/0016/0017/0018/0027/0036) claim
//! "`PRAGMA foreign_keys` is off in this crate". That is **false and frozen**:
//! `sqlx` turns `PRAGMA foreign_keys = ON` on by default for every `SQLite`
//! connection and [`Store::open_in`] never disables it, so every declared
//! `REFERENCES` IS engine-enforced (e.g. `agent.runtime_id` cannot be orphaned,
//! and `0010`'s `autopilot_run_id` link is a real constraint, not documentation).
//! Those comments cannot be corrected in place (see above) — this is the
//! authoritative statement. Where a table declares NO foreign key, that is a
//! deliberate schema choice (a polymorphic actor-ref, or a link an FK could not
//! tenant-scope), NOT a consequence of FKs being off; the service-layer guards
//! those repos apply are still required, because an FK proves only that a parent
//! row exists, never which workspace owns it.

use sqlx::{SqlitePool, migrate::Migrate};

mod store;
pub use store::Store;

/// Fresh-home bootstrap: the idempotent default workspace + owner + runtime +
/// starter-agent lay-down every entry point (CLI, daemon boot, `agent_create`)
/// shares, plus the stable [`bootstrap::default_runtime_id`] the seed and the
/// daemon's claim loop both key off.
pub mod bootstrap;

/// Typed repository wrappers over the Hangar schema.
///
/// One sub-module per logical table group: [`repo::agent`],
/// [`repo::agent_runtime`], [`repo::issue`], [`repo::skill`], and
/// [`repo::task`].
pub mod repo;

/// Task-FSM services: clock-aware lifecycle transitions (claim, start,
/// complete, fail, cancel) over the [`service::finalize`] idempotent primitive.
pub mod service;

/// The shared idempotent-finalize primitive, re-exported at the crate root.
///
/// Promoted out of [`service::finalize`] so the P1.4 retry sweeper (and any
/// future finalizer) can reuse the exact same 0-row-UPDATE → re-read →
/// success-or-mismatch algorithm the four P1.3 services share. Mirrors the
/// reference control plane `task.go:1010`.
pub use service::finalize::{
    FinalizeError, FinalizeOutcome, finalize_idempotent as idempotent_finalize,
};

/// Test-only helpers (isolated `$HOME`, `ENV_LOCK`) for driving [`Store`].
///
/// Available in this crate's own tests and to any downstream crate that enables
/// the `test-support` feature (the daemon's tripwire does this for its boot
/// tests).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Apply every embedded migration to `pool`, bringing a fresh or partially
/// migrated database up to the current schema version.
///
/// Idempotent: `sqlx` records applied migrations in `_sqlx_migrations` and
/// skips any already present, so calling this on every daemon boot is safe.
///
/// # Errors
///
/// Returns an error if a migration fails to apply (for example a checksum
/// mismatch on a previously applied migration, or malformed SQL).
pub async fn apply_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    let migrator = sqlx::migrate!("./migrations");
    reconcile_superseded_codex_launch_migrations(pool, &migrator).await?;
    reconcile_foreign_migration_93(pool).await?;
    migrator.run(pool).await?;
    Ok(())
}

/// The description version 93 carries on a database that booted a build of the
/// unmerged branch, i.e. `0093_fleet_provider_event_retention.sql`. SQLx spells
/// a description by replacing the filename slug's underscores with spaces.
const FOREIGN_MIGRATION_93_DESCRIPTION: &str = "fleet provider event retention";

/// The only object that file created, and the only thing to unwind.
const FOREIGN_MIGRATION_93_INDEX: &str = "idx_fleet_provider_event_retention";

/// Unwind a version 93 that belongs to a different migration entirely.
///
/// Two unrelated files claimed 93 while one of them sat unmerged:
/// `0093_board_card_issue_index.sql` on main (shipped in v1.23.2) and
/// `0093_fleet_provider_event_retention.sql` on the branch. A machine that
/// booted a branch build recorded 93 under the branch file's text, so every
/// later boot of a released binary dies in SQLx's checksum guard with
/// `migration 93 was previously applied but has been modified` and the daemon
/// never starts at all. The guard is right; the version number was wrong.
///
/// Unlike the 0087/0089/0090 reconciliation above, the loser here is not an
/// inert file this binary can record as applied: nothing is embedded under
/// version 93 but that text. (Since the branch landed, the same text IS
/// embedded — as `0094` — but recording 93 from it would still be wrong, and
/// 94 applies on its own once the row is gone.) So the repair is the other
/// direction. Drop the index it created and delete its row, leaving the
/// database exactly as if that branch build had never run, and let SQLx apply
/// main's 93 — and then 94 — normally.
///
/// Nothing is lost. The index is a pure read optimisation over rows the branch
/// migration never wrote to, and the branch's sweep code is not in this binary.
/// Unwinding it, rather than only unsticking the row, is what lets the branch
/// re-land under a free version: its `CREATE INDEX` has no `IF NOT EXISTS`, so
/// an orphan left behind would fail the renumbered file on this same machine.
///
/// Keyed on the foreign description, so a database whose 93 is main's own is
/// untouched, and so is one that never reached 93.
///
/// This runs OUTSIDE `store::backup_before_pending_migrations`, whose gate is
/// numeric: `MAX(version)` is already 93 here, so it reads an ordinary boot and
/// takes no snapshot. That is deliberate rather than an oversight. Teaching the
/// gate about this row would make a failed `VACUUM INTO` abort the open, so a
/// host short on disk would be left with a database it cannot repair AND cannot
/// boot, which is the exact failure this function exists to remove. The trade
/// is only sound because the repair touches no user data: one index drop, whose
/// definition lives in the file that owns it, and one bookkeeping row.
async fn reconcile_foreign_migration_93(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    (&mut *connection).ensure_migrations_table().await?;
    let foreign: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 93 AND description = ?",
    )
    .bind(FOREIGN_MIGRATION_93_DESCRIPTION)
    .fetch_one(&mut *connection)
    .await?;
    if foreign == 0 {
        return Ok(());
    }

    sqlx::query(&format!(
        "DROP INDEX IF EXISTS {FOREIGN_MIGRATION_93_INDEX}"
    ))
    .execute(&mut *connection)
    .await?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 93 AND description = ?")
        .bind(FOREIGN_MIGRATION_93_DESCRIPTION)
        .execute(&mut *connection)
        .await?;
    tracing::warn!(
        version = 93,
        description = FOREIGN_MIGRATION_93_DESCRIPTION,
        "unwound a migration that claimed a version number owned by another file; \
         re-applying this binary's migration 93"
    );
    Ok(())
}

/// Reconcile the 0087-versus-0089/0090 column collision before SQLx runs.
///
/// 0089 and 0090 landed at 10:27 and 10:29; 0087 landed at 15:46 the SAME day
/// with a LOWER number. Both sides add `resumable` and `event_watermark` to
/// `interactive_codex_thread`, and SQLx runs in version order, so there is no
/// ordering in which both can execute — one of them always dies on
/// `duplicate column name`. Which one depends on when a database was created,
/// which is why this cannot be fixed by editing files alone:
///
/// ```text
///   fresh db          87 runs, creates the columns    -> 89/90 would collide
///   db from 10:29-15:46   89/90 ran, columns exist    -> 87 would collide
/// ```
///
/// So the columns are owned by whichever side got there first on THAT database,
/// and the loser is recorded as applied without executing. 0089/0090 are inert
/// files (see their headers); 0087 still does real work for a fresh database
/// and must NOT be skipped there.
///
/// The `columns.contains(...)` guard on 87 is the whole correctness condition.
/// Skipping 87 unconditionally is the obvious-looking simplification and it is
/// wrong: on a fresh database this runs before ANY migration, so the table does
/// not exist yet and no columns are reported — recording 87 as applied then
/// means nothing ever adds `resumable`, and every later read fails with
/// `no such column: resumable`. That was live on main.
async fn reconcile_superseded_codex_launch_migrations(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    (&mut *connection).ensure_migrations_table().await?;
    let applied: std::collections::HashSet<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .collect();
    let columns: std::collections::HashSet<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('interactive_codex_thread')")
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .collect();

    let mut superseded = Vec::new();
    // Only when the columns are ALREADY there, i.e. 89/90 won this database.
    if columns.contains("resumable") && !applied.contains(&87) {
        superseded.push(87);
    }
    if columns.contains("resumable") && !applied.contains(&89) {
        superseded.push(89);
    }
    if columns.contains("event_watermark") && !applied.contains(&90) {
        superseded.push(90);
    }
    for version in superseded {
        let migration = embedded(migrator, version)?;
        sqlx::query(
            "INSERT OR IGNORE INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) VALUES (?, ?, TRUE, ?, -1)",
        )
        .bind(migration.version)
        .bind(&*migration.description)
        .bind(&*migration.checksum)
        .execute(&mut *connection)
        .await?;
    }

    // A database from the 10:29-15:46 window RAN 0089/0090 and recorded the
    // checksum of the text they had then. That text is gone — the files are
    // inert now — so SQLx would refuse to boot on a checksum mismatch, which
    // is exactly the corruption guard doing its job on a change we made on
    // purpose. Restamp those two rows to the current text. Safe because the
    // effect the old text had (the two columns) is present and verified above;
    // only the recorded spelling of an already-satisfied migration changes.
    for version in [89_i64, 90] {
        if !applied.contains(&version) {
            continue;
        }
        let migration = embedded(migrator, version)?;
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(&*migration.checksum)
            .bind(version)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// One embedded migration by version, or an error naming the missing one.
fn embedded(
    migrator: &sqlx::migrate::Migrator,
    version: i64,
) -> anyhow::Result<&sqlx::migrate::Migration> {
    migrator
        .iter()
        .find(|migration| migration.version == version)
        .ok_or_else(|| anyhow::anyhow!("missing embedded migration {version}"))
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn memory_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    async fn columns_of(pool: &SqlitePool, table: &str) -> std::collections::HashSet<String> {
        sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .collect()
    }

    async fn index_exists(pool: &SqlitePool, name: &str) -> bool {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
        count == 1
    }

    /// Rewind an up-to-date database to what a machine that booted a build of
    /// the unmerged branch carries: main's 93 never ran, the branch's did, and
    /// 93 is recorded under the branch file's description and checksum.
    async fn seed_unmerged_0093(pool: &SqlitePool) {
        sqlx::query("DROP INDEX IF EXISTS idx_board_card_issue")
            .execute(pool)
            .await
            .unwrap();
        // The caller migrated first, and this tree carries the renumbered
        // `0094_fleet_provider_event_retention.sql`, so 94 is already recorded
        // and its index already exists. The machine being modelled ran the OLD
        // branch build, which had that text at 93 and no 94 at all, so unwind
        // both before seeding the broken state. Without this the bare
        // `CREATE INDEX` below dies on "index ... already exists" and the test
        // never reaches the repair it is here to exercise.
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 94")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DROP INDEX IF EXISTS idx_fleet_provider_event_retention")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 93")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) \
             VALUES (93, 'fleet provider event retention', TRUE, ?, -1)",
        )
        .bind(vec![0xde_u8, 0xad, 0xbe, 0xef])
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE INDEX idx_fleet_provider_event_retention \
             ON fleet_provider_event(observed_at) \
             WHERE raw_payload <> '' AND projection_revision IS NOT NULL AND source <> 'acp'",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// The property that matters, and the one a recorded-versions check cannot
    /// see: after migrating, the COLUMNS are actually there.
    ///
    /// Asserting only that 87/89/90 appear in `_sqlx_migrations` passes even
    /// when every one of them was recorded without executing — which is exactly
    /// how a build that produced a table with no `resumable` column reached main
    /// with a green store suite. The daemon then failed at runtime with
    /// `no such column: resumable`.
    #[tokio::test]
    async fn a_fresh_database_really_gets_the_codex_thread_columns() {
        let pool = memory_pool().await;
        apply_migrations(&pool).await.unwrap();

        let columns = columns_of(&pool, "interactive_codex_thread").await;
        assert!(
            columns.contains("resumable"),
            "resumable missing; got {columns:?}"
        );
        assert!(
            columns.contains("event_watermark"),
            "event_watermark missing; got {columns:?}"
        );

        for version in [87_i64, 89, 90] {
            let present: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
                    .bind(version)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(present, 1, "migration {version} was not recorded");
        }

        // Re-running is a no-op, including the checksum restamp.
        apply_migrations(&pool).await.unwrap();
        assert!(columns_of(&pool, "interactive_codex_thread").await.contains("resumable"));
    }

    /// The other side of the collision: a database created in the 10:29-15:46
    /// window on 2026-08-12 RAN 0089/0090 under their original text and never
    /// ran 0087. It carries checksums for SQL that no longer exists, so without
    /// the restamp SQLx refuses to boot on a checksum mismatch — a real upgrade
    /// breaking for anyone who ran a daemon that afternoon.
    #[tokio::test]
    async fn a_database_that_ran_the_original_0089_still_upgrades() {
        let pool = memory_pool().await;
        // Migrate to 88, then hand-apply what the ORIGINAL 0089/0090 did and
        // record them under a checksum that cannot match the current files.
        apply_migrations(&pool).await.unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version IN (87, 89, 90)")
            .execute(&pool)
            .await
            .unwrap();
        for version in [89_i64, 90] {
            sqlx::query(
                "INSERT INTO _sqlx_migrations \
                 (version, description, success, checksum, execution_time) \
                 VALUES (?, 'legacy', TRUE, ?, -1)",
            )
            .bind(version)
            .bind(vec![0xde_u8, 0xad, 0xbe, 0xef])
            .execute(&pool)
            .await
            .unwrap();
        }

        apply_migrations(&pool)
            .await
            .expect("a database carrying the original 0089/0090 must still upgrade");

        let columns = columns_of(&pool, "interactive_codex_thread").await;
        assert!(columns.contains("resumable"), "got {columns:?}");
        // 0087 must have been recorded, not executed: executing it would have
        // died on `duplicate column name: resumable`.
        let present: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 87")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(present, 1, "0087 must be reconciled, not run");
    }

    /// Two migration files sharing a version number merge cleanly, because the
    /// filenames differ and git sees no textual conflict, then fail at runtime
    /// on `UNIQUE constraint failed: _sqlx_migrations.version` for every fresh
    /// database. It has happened twice: 0082 (fixed by renumbering the
    /// attention migration to 0084) and 0093, where an unmerged branch and main
    /// both took the slot. Diff the numbers, not the filenames.
    #[test]
    fn every_migration_file_has_a_unique_version() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut by_version: std::collections::BTreeMap<u32, Vec<String>> =
            std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("migrations/ must be readable") {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".sql") else {
                continue;
            };
            let (digits, slug) = stem
                .split_once('_')
                .unwrap_or_else(|| panic!("{name} does not follow NNNN_<slug>.sql"));
            assert_eq!(
                digits.len(),
                4,
                "{name} must use a 4-digit zero-padded ordinal"
            );
            assert!(
                !slug.is_empty(),
                "{name} must carry a slug after the ordinal"
            );
            let version: u32 = digits
                .parse()
                .unwrap_or_else(|_| panic!("{name} must start with a 4-digit ordinal"));
            by_version.entry(version).or_default().push(name);
        }
        assert!(
            !by_version.is_empty(),
            "no migrations found in {}",
            dir.display()
        );
        let clashes: Vec<String> = by_version
            .iter()
            .filter(|(_, files)| files.len() > 1)
            .map(|(version, files)| format!("{version}: {}", files.join(", ")))
            .collect();
        assert!(
            clashes.is_empty(),
            "migration versions claimed more than once: {}",
            clashes.join("; ")
        );
    }

    /// A database that booted a build of PR #790 recorded version 93 as
    /// `fleet provider event retention`. Main's 93 is `board_card_issue_index`,
    /// so every later boot dies on `migration 93 was previously applied but has
    /// been modified` and the daemon never starts. The repair has to undo the
    /// foreign migration, not just unstick the row: leaving its index behind
    /// would fail the renumbered file the moment #790 lands.
    #[tokio::test]
    async fn a_database_that_ran_the_unmerged_0093_still_upgrades() {
        let pool = memory_pool().await;
        apply_migrations(&pool).await.unwrap();
        seed_unmerged_0093(&pool).await;

        apply_migrations(&pool)
            .await
            .expect("a database carrying the unmerged 0093 must still upgrade");

        assert!(
            index_exists(&pool, "idx_board_card_issue").await,
            "main's 0093 never ran"
        );
        let description: String =
            sqlx::query_scalar("SELECT description FROM _sqlx_migrations WHERE version = 93")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(description, "board card issue index");

        // The claim the repair's doc makes, now actually checkable: unwinding
        // the index is what lets the renumbered file re-land. Its CREATE INDEX
        // carries no IF NOT EXISTS, so had the orphan survived, 94 would have
        // died here instead of recording itself and rebuilding the index.
        let renumbered: String =
            sqlx::query_scalar("SELECT description FROM _sqlx_migrations WHERE version = 94")
                .fetch_one(&pool)
                .await
                .expect("the renumbered retention migration must apply after the repair");
        assert_eq!(renumbered, "fleet provider event retention");
        assert!(
            index_exists(&pool, "idx_fleet_provider_event_retention").await,
            "0094 must rebuild the index the repair dropped"
        );

        // Re-running is a no-op: the row now matches, so the repair must not fire.
        apply_migrations(&pool).await.unwrap();
        assert!(index_exists(&pool, "idx_board_card_issue").await);
    }

    /// The repair is keyed on the foreign description, so an untouched database
    /// must pass straight through it with its index intact.
    #[tokio::test]
    async fn a_healthy_database_keeps_its_board_card_index() {
        let pool = memory_pool().await;
        apply_migrations(&pool).await.unwrap();
        apply_migrations(&pool).await.unwrap();
        assert!(index_exists(&pool, "idx_board_card_issue").await);
    }

    /// The daemon does not call [`apply_migrations`] directly: it boots through
    /// [`Store::open_in`], which runs `backup_before_pending_migrations` first.
    /// That gate compares version NUMBERS only, and on this database
    /// `MAX(version)` is already 93, so it reads an ordinary boot and takes no
    /// snapshot. Pin the real path, including the absent backup, because that
    /// absence is why the repair has to be safe without one.
    #[tokio::test]
    async fn the_daemon_boot_path_repairs_the_unmerged_0093() {
        let home = tempfile::tempdir().expect("create a home for the database");
        let store = Store::open_in(home.path()).await.expect("first boot");
        seed_unmerged_0093(store.pool()).await;
        drop(store);

        let store = Store::open_in(home.path())
            .await
            .expect("the daemon must boot against a database carrying the unmerged 0093");

        let description: String =
            sqlx::query_scalar("SELECT description FROM _sqlx_migrations WHERE version = 93")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(description, "board card issue index");
        assert!(index_exists(store.pool(), "idx_board_card_issue").await);
        // Dropped by the repair, then rebuilt by the renumbered 0094 in the
        // same boot. Its absence would mean 94 never applied.
        assert!(index_exists(store.pool(), "idx_fleet_provider_event_retention").await);
        // On main this asserted the ABSENCE of a snapshot: the gate is numeric,
        // read MAX(version) = 93 against an embedded max of 93, saw nothing
        // pending and took none — which is what proved the repair cannot depend
        // on one. This tree carries 0094, so 94 IS genuinely pending and the
        // gate fires for that ordinary reason, naming the file after the
        // applied max. The original property is unchanged and still pinned, by
        // `a_database_that_ran_the_unmerged_0093_still_upgrades`: it drives
        // `apply_migrations` directly and never reaches the backup path at all.
        assert!(
            home.path().join("hangar.db.pre-93.bak").exists(),
            "94 is pending, so the ordinary pre-upgrade snapshot must be taken"
        );
    }
}

/// Probe whether the LIVE database has drifted away from the schema this
/// binary was compiled against, returning a human-readable description of the
/// drift (or the probe failure) — `None` means healthy.
///
/// The trap this catches: a stale daemon binary keeps serving a database that
/// a NEWER binary has since migrated forward. `sqlx` only validates migrations
/// at pool-open, so the running daemon never notices — its queries half-work
/// against the newer schema and every surface renders empty zeros instead of
/// an error. The daemon-health snapshot calls this on demand so the TUI can
/// scream "restart the daemon" instead of rendering a silently-blank pane.
///
/// Two signals, both folded into the returned string:
/// - the applied `MAX(version)` in `_sqlx_migrations` is AHEAD of the newest
///   migration embedded in this binary → stale binary;
/// - the probe query itself fails (file deleted, corrupt, locked) → the
///   database is unreachable outright.
pub async fn schema_drift(pool: &SqlitePool) -> Option<String> {
    let embedded_max = sqlx::migrate!("./migrations").iter().map(|m| m.version).max().unwrap_or(0);
    let applied_max: Result<Option<i64>, sqlx::Error> =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await;
    match applied_max {
        Ok(Some(applied)) if applied > embedded_max => Some(format!(
            "database schema (migration {applied}) is AHEAD of this daemon binary \
             (migration {embedded_max}) — stale binary; restart the daemon from the \
             current build"
        )),
        Ok(_) => None,
        Err(e) => Some(format!("database unreachable: {e}")),
    }
}
