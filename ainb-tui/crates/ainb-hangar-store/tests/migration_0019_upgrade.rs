//! Upgrade-from-populated test for migration 0019 (autopilot execution modes +
//! concurrency policies).
//!
//! Migration 0019 adds two columns to `autopilot` (`execution_mode`,
//! `concurrency_policy`) without touching any existing row.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0018),
//! 2. seed a live workspace + member + agent + autopilot (the row the new
//!    columns attach to),
//! 3. apply the embedded migrations (which adds exactly 0019),
//! 4. assert the pre-existing rows survive intact, the new autopilot columns
//!    read their v1-preserving defaults (`run_only` + `skip`), the columns'
//!    `CHECK` constraints reject a junk discriminant, and a second apply is a
//!    no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0019_autopilot_execution_concurrency.sql`).
const NEW_MIGRATION_VERSION: i64 = 19;

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

/// Seed the FK graph the autopilot row requires: one workspace + user + member +
/// runtime + agent + one autopilot (the row the new columns attach to).
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
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(pool)
    .await
    .expect("insert agent_runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("Worker")
    .bind("rt-1")
    .bind(None::<String>)
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, instructions, cron_expr, \
          max_concurrent_runs, next_tick_at, enabled, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?)",
    )
    .bind("ap-1")
    .bind("ws-1")
    .bind("agent-1")
    .bind("nightly")
    .bind(None::<String>)
    .bind("0 0 * * *")
    .bind(1_i64)
    .bind(None::<i64>)
    .bind(0_i64)
    .execute(pool)
    .await
    .expect("insert autopilot");
}

/// Snapshot the pre-existing entity identity, ordered, as the data the additions
/// must not touch.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in [
        "workspace",
        "user",
        "member",
        "agent_runtime",
        "agent",
        "autopilot",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count {t}: {e}"));
        out.push((t.to_string(), n));
    }
    out
}

#[tokio::test]
async fn migration_0019_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // Upgrade: the embedded migrator skips 0001..0018 (already recorded) and
    // applies 0019.
    apply_migrations(&pool).await.expect("upgrade applies 0019");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0019 recorded as applied");

    // 1. The pre-existing population survives intact — the additions touch no
    //    existing row.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The pre-existing autopilot reads the v1-preserving column defaults: an
    //    upgrading install keeps its exact prior behaviour (run_only fire path,
    //    skip-when-at-limit scheduling).
    let row = sqlx::query("SELECT execution_mode, concurrency_policy FROM autopilot WHERE id = ?")
        .bind("ap-1")
        .fetch_one(&pool)
        .await
        .expect("read upgraded autopilot");
    assert_eq!(
        row.get::<String, _>("execution_mode"),
        "run_only",
        "execution_mode must default to run_only (the v1 fire path) after upgrade"
    );
    assert_eq!(
        row.get::<String, _>("concurrency_policy"),
        "skip",
        "concurrency_policy must default to skip (the v1 scheduler) after upgrade"
    );

    // 3. The new columns' CHECK constraints reject a junk discriminant.
    let bad_mode = sqlx::query("UPDATE autopilot SET execution_mode = 'bogus' WHERE id = ?")
        .bind("ap-1")
        .execute(&pool)
        .await;
    assert!(
        bad_mode.is_err(),
        "the execution_mode CHECK must reject a junk discriminant"
    );
    let bad_policy = sqlx::query("UPDATE autopilot SET concurrency_policy = 'bogus' WHERE id = ?")
        .bind("ap-1")
        .execute(&pool)
        .await;
    assert!(
        bad_policy.is_err(),
        "the concurrency_policy CHECK must reject a junk discriminant"
    );

    // The valid discriminants all write.
    for mode in ["create_issue", "run_only"] {
        sqlx::query("UPDATE autopilot SET execution_mode = ? WHERE id = ?")
            .bind(mode)
            .bind("ap-1")
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("execution_mode={mode} must write: {e}"));
    }
    for policy in ["skip", "queue", "replace"] {
        sqlx::query("UPDATE autopilot SET concurrency_policy = ? WHERE id = ?")
            .bind(policy)
            .bind("ap-1")
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("concurrency_policy={policy} must write: {e}"));
    }

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
