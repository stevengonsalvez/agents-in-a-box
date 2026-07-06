//! Upgrade-from-populated test for migration 0016 (first-class issue labels).
//!
//! Migration 0016 adds two tables — `label` (workspace-scoped definition with a
//! `(workspace_id, name)` UNIQUE index) and `issue_label` (the attach join with
//! a `(issue_id, label_id)` composite PK) — without touching the existing
//! `issue.labels` JSON column (migration 0014), which is kept as a denormalized
//! read-cache.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0015),
//! 2. seed a live issue row that already carries a `labels` JSON value (the row
//!    the new tables must leave intact),
//! 3. apply the embedded migrations (which adds exactly 0016),
//! 4. assert the pre-existing issue survives intact (its `labels` JSON cache
//!    untouched), the two new tables accept writes, the `(workspace_id, name)`
//!    UNIQUE index rejects a duplicate label, the `(issue_id, label_id)`
//!    composite PK rejects a duplicate attach, and a second apply is a no-op
//!    (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test (`0016_issue_label.sql`).
const NEW_MIGRATION_VERSION: i64 = 16;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only the migrations PRIOR
/// to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install runs
/// before upgrading. Uses the same on-disk `migrations/` directory the embedded
/// migrator is compiled from, so the prior set can never drift from what
/// `apply_migrations` would itself apply.
async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");

    let mut migrator = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .expect("load migrations directory");
    migrator.migrations.to_mut().retain(|m| m.version < NEW_MIGRATION_VERSION);
    assert!(
        !migrator.migrations.is_empty(),
        "prior-migration set must not be empty"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// Seed the FK graph (one workspace) plus a live issue row that already carries a
/// `labels` JSON value — the row whose survival across the table additions the
/// test proves. The issue is written WITH the 0014 `labels` column (present at
/// the prior schema) so the test can prove the JSON cache is untouched.
async fn seed_populated(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");

    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at, labels) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .bind("Refactor API")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .bind(r#"["bug","p1"]"#)
    .execute(pool)
    .await
    .expect("seed issue with labels JSON cache");
}

/// Snapshot the pre-existing issue identity + its `labels` JSON cache, ordered by
/// id, as `(id, title, labels)` rows — the data the table additions must not
/// touch.
async fn issue_snapshot(pool: &SqlitePool) -> Vec<(String, String, String)> {
    sqlx::query("SELECT id, title, labels FROM issue ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("snapshot issues")
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("id"),
                r.get::<String, _>("title"),
                r.get::<String, _>("labels"),
            )
        })
        .collect()
}

#[tokio::test]
async fn migration_0016_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = issue_snapshot(&pool).await;
    assert_eq!(before.len(), 1, "seeded population: {before:?}");
    assert_eq!(before[0].2, r#"["bug","p1"]"#, "JSON cache seeded");

    // Upgrade: the embedded migrator skips 0001..0015 (already recorded) and
    // applies 0016.
    apply_migrations(&pool).await.expect("upgrade applies 0016");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0016 recorded as applied");

    // 1. The pre-existing issue survives intact — its `labels` JSON cache is
    //    untouched by the table additions (the column is kept, not migrated).
    let after = issue_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch issue rows");

    // 2. The new `label` table accepts writes and enforces the
    //    `(workspace_id, name)` UNIQUE index (resolve-or-reject guard).
    sqlx::query("INSERT INTO label (id, workspace_id, name, color) VALUES (?, ?, ?, ?)")
        .bind("label-1")
        .bind("ws-1")
        .bind("bug")
        .bind("#ff0000")
        .execute(&pool)
        .await
        .expect("insert label");
    let dup = sqlx::query("INSERT INTO label (id, workspace_id, name, color) VALUES (?, ?, ?, ?)")
        .bind("label-2")
        .bind("ws-1")
        .bind("bug")
        .bind(None::<String>)
        .execute(&pool)
        .await;
    assert!(
        dup.is_err(),
        "the (workspace_id, name) UNIQUE index must reject a duplicate label name"
    );
    // A NULL color is valid (the column is nullable).
    sqlx::query("INSERT INTO label (id, workspace_id, name, color) VALUES (?, ?, ?, ?)")
        .bind("label-3")
        .bind("ws-1")
        .bind("p1")
        .bind(None::<String>)
        .execute(&pool)
        .await
        .expect("insert label with NULL color");

    // 3. The `issue_label` join accepts an attach and enforces the
    //    `(issue_id, label_id)` composite PK (the idempotency / dedupe guard).
    sqlx::query("INSERT INTO issue_label (issue_id, label_id) VALUES (?, ?)")
        .bind("issue-1")
        .bind("label-1")
        .execute(&pool)
        .await
        .expect("attach label to issue");
    let dup_attach = sqlx::query("INSERT INTO issue_label (issue_id, label_id) VALUES (?, ?)")
        .bind("issue-1")
        .bind("label-1")
        .execute(&pool)
        .await;
    assert!(
        dup_attach.is_err(),
        "the (issue_id, label_id) composite PK must reject a duplicate attach"
    );
    let attached: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue_label WHERE issue_id = ?")
        .bind("issue-1")
        .fetch_one(&pool)
        .await
        .expect("count attaches");
    assert_eq!(attached, 1, "exactly one attach landed");

    // 4. Double-apply is idempotent: nothing re-runs, nothing changes.
    let snapshot = issue_snapshot(&pool).await;
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        issue_snapshot(&pool).await,
        snapshot,
        "double-apply must not change any row"
    );

    pool.close().await;
}
