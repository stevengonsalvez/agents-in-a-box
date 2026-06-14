//! Upgrade-from-populated test for migration 0014 (issue priority, due date,
//! and labels).
//!
//! Migration 0014 adds three columns to `issue`:
//!   - `priority INTEGER NOT NULL DEFAULT 0` (0..3 = P3..P0, higher = more
//!     urgent),
//!   - `due_date INTEGER` (epoch millis, nullable),
//!   - `labels TEXT NOT NULL DEFAULT '[]'` (a JSON array of label strings).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0013),
//! 2. seed live issue rows (the rows the new columns must leave intact),
//! 3. apply the embedded migrations (which adds exactly 0014),
//! 4. assert every pre-existing row survives intact, reads back the column
//!    defaults (priority 0, due_date NULL, labels '[]'), the new columns accept
//!    explicit writes, and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0014_issue_priority_due_labels.sql`).
const NEW_MIGRATION_VERSION: i64 = 14;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only the migrations
/// PRIOR to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing
/// install runs before upgrading. Uses the same on-disk `migrations/` directory
/// the embedded migrator is compiled from, so the prior set can never drift
/// from what `apply_migrations` would itself apply.
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

/// Seed the FK graph (one workspace, one user) plus two live issues — the rows
/// whose survival across the column additions the test proves.
async fn seed_populated(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");

    // Two issues, seeded WITHOUT the new columns (they don't exist yet at the
    // prior schema). After upgrade they must read back the column defaults.
    let issues: [(&str, &str, &str); 2] = [
        ("issue-1", "An issue", "open"),
        ("issue-2", "Another issue", "done"),
    ];
    for (id, title, state) in issues {
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, state, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind("ws-1")
        .bind(title)
        .bind(state)
        .bind("member")
        .bind("user-1")
        .bind(7_i64)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("seed issue {id}: {e}"));
    }
}

/// Snapshot the pre-existing issue identity columns, ordered by id, as
/// `(id, title)` rows — the data 0014's column additions must not touch. The
/// `state` column is deliberately excluded: a LATER migration (0023) legitimately
/// canonicalises the lifecycle vocabulary (`open -> todo`), which is not 0014's
/// concern; the post-chain states are asserted separately.
async fn issue_identity_snapshot(pool: &SqlitePool) -> Vec<(String, String)> {
    sqlx::query("SELECT id, title FROM issue ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("snapshot issues")
        .iter()
        .map(|r| (r.get::<String, _>("id"), r.get::<String, _>("title")))
        .collect()
}

#[tokio::test]
async fn migration_0014_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = issue_identity_snapshot(&pool).await;
    assert_eq!(before.len(), 2, "seeded population: {before:?}");

    // Upgrade: the embedded migrator skips 0001..0013 (already recorded) and
    // applies 0014.
    apply_migrations(&pool).await.expect("upgrade applies 0014");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0014 recorded as applied");

    // 1. Every pre-existing row survives intact (id + title — 0014's additions
    //    touch neither).
    let after = issue_identity_snapshot(&pool).await;
    assert_eq!(
        after, before,
        "upgrade must not touch issue identity columns"
    );

    // The lifecycle vocabulary is canonicalised by the downstream 0023 migration
    // the full chain applies: the seeded legacy `open` becomes `todo`, the
    // already-terminal `done` is unchanged.
    let states: Vec<(String, String)> = sqlx::query("SELECT id, state FROM issue ORDER BY id")
        .fetch_all(&pool)
        .await
        .expect("read post-chain states")
        .iter()
        .map(|r| (r.get::<String, _>("id"), r.get::<String, _>("state")))
        .collect();
    assert_eq!(
        states,
        vec![
            ("issue-1".to_string(), "todo".to_string()),
            ("issue-2".to_string(), "done".to_string()),
        ],
        "0023 remaps legacy open -> todo; done stays done"
    );

    // 2. Each pre-existing row reads back the new column DEFAULTS: priority 0,
    //    due_date NULL, labels '[]'.
    let row = sqlx::query("SELECT priority, due_date, labels FROM issue WHERE id = ?")
        .bind("issue-1")
        .fetch_one(&pool)
        .await
        .expect("read upgraded issue");
    assert_eq!(row.get::<i64, _>("priority"), 0, "default priority is 0");
    assert_eq!(
        row.get::<Option<i64>, _>("due_date"),
        None,
        "default due_date is NULL"
    );
    assert_eq!(
        row.get::<String, _>("labels"),
        "[]",
        "default labels is '[]'"
    );

    // 3. New semantics: the columns accept explicit writes (priority jump, a
    //    due date, and a JSON label list).
    sqlx::query("UPDATE issue SET priority = ?, due_date = ?, labels = ? WHERE id = ?")
        .bind(3_i64)
        .bind(1_700_000_000_000_i64)
        .bind(r#"["bug","p0"]"#)
        .bind("issue-1")
        .execute(&pool)
        .await
        .expect("write new columns");
    let row = sqlx::query("SELECT priority, due_date, labels FROM issue WHERE id = ?")
        .bind("issue-1")
        .fetch_one(&pool)
        .await
        .expect("read written issue");
    assert_eq!(
        row.get::<i64, _>("priority"),
        3,
        "explicit priority persists"
    );
    assert_eq!(
        row.get::<Option<i64>, _>("due_date"),
        Some(1_700_000_000_000),
        "explicit due_date persists"
    );
    assert_eq!(
        row.get::<String, _>("labels"),
        r#"["bug","p0"]"#,
        "explicit labels JSON persists"
    );

    // 4. Double-apply is idempotent: nothing re-runs, nothing changes.
    let snapshot = issue_identity_snapshot(&pool).await;
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        issue_identity_snapshot(&pool).await,
        snapshot,
        "double-apply must not change any row"
    );

    pool.close().await;
}
