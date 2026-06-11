//! Upgrade-from-populated test for migration 0020 (per-workspace config columns).
//!
//! Migration 0020 adds three nullable columns to `workspace` (`context_prompt`,
//! `repo_whitelist`, `issue_prefix`) without touching any existing row.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0019),
//! 2. seed a live workspace + member (the row the new columns attach to),
//! 3. apply the embedded migrations (which adds exactly 0020),
//! 4. assert the pre-existing rows survive intact, the new workspace columns
//!    read their v1-preserving default (`NULL` — "not configured"), the new
//!    columns accept a written value, and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0020_workspace_config.sql`).
const NEW_MIGRATION_VERSION: i64 = 20;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only the migrations PRIOR
/// to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install runs
/// before upgrading.
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

/// Seed one workspace + user + member — the rows the new workspace columns
/// attach to, with a member so the FK graph mirrors a real install.
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
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, ?)")
        .bind("ws-1")
        .bind("user-1")
        .bind("owner")
        .execute(pool)
        .await
        .expect("insert member");
}

/// Snapshot the pre-existing entity counts, ordered, as the data the additions
/// must not touch.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in ["workspace", "user", "member"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count {t}: {e}"));
        out.push((t.to_string(), n));
    }
    out
}

#[tokio::test]
async fn migration_0020_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // Upgrade: the embedded migrator skips 0001..0019 (already recorded) and
    // applies 0020.
    apply_migrations(&pool).await.expect("upgrade applies 0020");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0020 recorded as applied");

    // 1. The pre-existing population survives intact — the additions touch no
    //    existing row.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The pre-existing workspace reads the v1-preserving column defaults:
    //    NULL for every new column ("not configured" — its agent runs and issue
    //    titles keep their exact prior behaviour).
    let row = sqlx::query(
        "SELECT context_prompt, repo_whitelist, issue_prefix FROM workspace WHERE id = ?",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("read upgraded workspace");
    assert_eq!(
        row.get::<Option<String>, _>("context_prompt"),
        None,
        "context_prompt must default to NULL after upgrade"
    );
    assert_eq!(
        row.get::<Option<String>, _>("repo_whitelist"),
        None,
        "repo_whitelist must default to NULL after upgrade"
    );
    assert_eq!(
        row.get::<Option<String>, _>("issue_prefix"),
        None,
        "issue_prefix must default to NULL after upgrade"
    );

    // 3. The new columns accept a written value (the config surface writes them).
    sqlx::query(
        "UPDATE workspace SET context_prompt = ?, repo_whitelist = ?, issue_prefix = ? \
         WHERE id = ?",
    )
    .bind("Always run cargo fmt.")
    .bind(r#"["org/api","org/web"]"#)
    .bind("[OPS] ")
    .bind("ws-1")
    .execute(&pool)
    .await
    .expect("write workspace config");
    let row = sqlx::query(
        "SELECT context_prompt, repo_whitelist, issue_prefix FROM workspace WHERE id = ?",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("read configured workspace");
    assert_eq!(
        row.get::<String, _>("context_prompt"),
        "Always run cargo fmt."
    );
    assert_eq!(
        row.get::<String, _>("repo_whitelist"),
        r#"["org/api","org/web"]"#
    );
    assert_eq!(row.get::<String, _>("issue_prefix"), "[OPS] ");

    // 4. Double-apply is idempotent: nothing re-runs, nothing changes.
    let snapshot = population_snapshot(&pool).await;
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        population_snapshot(&pool).await,
        snapshot,
        "double-apply must not change any row"
    );

    pool.close().await;
}
