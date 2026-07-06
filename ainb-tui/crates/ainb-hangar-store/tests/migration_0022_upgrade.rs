//! Upgrade-from-populated test for migration 0022 (per-task provider usage,
//! e38.35).
//!
//! Migration 0022 adds the `task_usage` table (plus one index) for the usage
//! dashboard's token/cost + per-agent rollup. It touches no existing table — a
//! pure additive `CREATE TABLE` / `CREATE INDEX` — so every pre-existing row must
//! survive and each upgrading workspace simply starts with zero recorded usage.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0021),
//! 2. seed a live workspace + user + member + `agent_runtime` + agent + issue + task
//!    (the rows that existed before the usage table, including the FK targets the
//!    new `workspace_id` / `agent_id` / `task_id` columns point at),
//! 3. apply the embedded migrations (which adds exactly 0022),
//! 4. assert the pre-existing rows survive intact, the new table exists and is
//!    empty (zero usage for an upgrading workspace), an inserted usage row
//!    round-trips, and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0022_task_usage.sql`).
const NEW_MIGRATION_VERSION: i64 = 22;

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

/// Seed one workspace + user + member + runtime + agent + issue + task — the rows
/// that pre-date the usage table, including the FK targets the new
/// `task_usage.{workspace_id,agent_id,task_id}` columns point at.
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
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, 'd', 'claude', 'local', 'online')",
    )
    .bind("rt-1")
    .bind("ws-1")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, 'claude-agent', ?, 'workspace', 'user-1')",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("rt-1")
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, 'Pre-existing issue', 'open', 'member', 'user-1', 0)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .execute(pool)
    .await
    .expect("insert issue");
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, ?, ?, ?, ?, 'done', 0)",
    )
    .bind("task-1")
    .bind("ws-1")
    .bind("rt-1")
    .bind("agent-1")
    .bind("issue-1")
    .execute(pool)
    .await
    .expect("insert task");
}

/// Snapshot the pre-existing entity counts, ordered, as the data the addition
/// must not touch.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in [
        "workspace",
        "user",
        "member",
        "agent_runtime",
        "agent",
        "issue",
        "agent_task_queue",
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
async fn migration_0022_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // The usage table does NOT exist before the upgrade.
    let exists_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'task_usage'",
    )
    .fetch_one(&pool)
    .await
    .expect("probe task_usage before");
    assert_eq!(exists_before, 0, "task_usage must not exist before 0022");

    // Upgrade: the embedded migrator skips 0001..0021 (already recorded) and
    // applies 0022.
    apply_migrations(&pool).await.expect("upgrade applies 0022");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0022 recorded as applied");

    // 1. The pre-existing population survives intact — the addition touches no
    //    existing row.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The new table now exists and an upgrading workspace starts with zero
    //    recorded usage: a pre-0022 install never persisted usage rows.
    let empty: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_usage WHERE workspace_id = ?")
        .bind("ws-1")
        .fetch_one(&pool)
        .await
        .expect("count usage after upgrade");
    assert_eq!(empty, 0, "upgrading workspace starts with zero usage");

    // 3. An inserted usage row round-trips: tokens + cost persist and read back.
    sqlx::query(
        "INSERT INTO task_usage \
         (task_id, workspace_id, agent_id, input_tokens, output_tokens, cost_usd, created_at) \
         VALUES (?, ?, ?, 1200, 340, 0.0231, 100)",
    )
    .bind("task-1")
    .bind("ws-1")
    .bind("agent-1")
    .execute(&pool)
    .await
    .expect("insert usage row");

    let row = sqlx::query(
        "SELECT input_tokens, output_tokens, cost_usd FROM task_usage WHERE task_id = ?",
    )
    .bind("task-1")
    .fetch_one(&pool)
    .await
    .expect("read usage row");
    assert_eq!(row.get::<i64, _>("input_tokens"), 1200);
    assert_eq!(row.get::<i64, _>("output_tokens"), 340);
    assert!((row.get::<f64, _>("cost_usd") - 0.0231).abs() < 1e-9);

    // A usage row whose task_id points at a non-existent task is rejected (the
    // task FK is enforced).
    sqlx::query(
        "INSERT INTO task_usage (task_id, workspace_id, agent_id, created_at) \
         VALUES ('task-1b', ?, ?, 200)",
    )
    .bind("ws-1")
    .bind("agent-1")
    .execute(&pool)
    .await
    .expect_err("a second usage row for a missing task FK is rejected");

    // 4. Double-apply is idempotent: nothing re-runs, the row we inserted stays.
    apply_migrations(&pool).await.expect("second apply is a no-op");
    let still: i64 = sqlx::query_scalar("SELECT input_tokens FROM task_usage WHERE task_id = ?")
        .bind("task-1")
        .fetch_one(&pool)
        .await
        .expect("read usage row after re-apply");
    assert_eq!(still, 1200, "double-apply must not change any row");

    pool.close().await;
}
