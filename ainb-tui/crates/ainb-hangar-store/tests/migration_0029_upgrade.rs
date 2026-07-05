//! Upgrade-from-populated test for migration 0029 (durable run history +
//! cost_rollup view, P10 / D19).
//!
//! Migration 0029 adds the `run_history` table (plus one index) and the
//! `cost_rollup` VIEW for the observability timeline + daily cost rollups. It
//! touches no existing table — a pure additive `CREATE TABLE` / `CREATE INDEX` /
//! `CREATE VIEW` — so every pre-existing row must survive and each upgrading
//! workspace simply starts with an empty history.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0028),
//! 2. seed a live workspace + user + member + `agent_runtime` + agent + issue + task
//!    (the FK targets the new `workspace_id` / `task_id` columns point at),
//! 3. apply the embedded migrations (which adds exactly 0029),
//! 4. assert the pre-existing rows survive intact, the new table + view exist and
//!    are empty, an inserted run row round-trips (and rolls up), and a second
//!    apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0029_run_history.sql`).
const NEW_MIGRATION_VERSION: i64 = 29;

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
/// that pre-date the run-history table, including the FK targets the new
/// `run_history.{workspace_id,task_id}` columns point at.
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
async fn migration_0029_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // Neither the table nor the view exists before the upgrade.
    let exists_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'run_history' OR name = 'cost_rollup'",
    )
    .fetch_one(&pool)
    .await
    .expect("probe run_history/cost_rollup before");
    assert_eq!(
        exists_before, 0,
        "run_history + cost_rollup must not exist before 0029"
    );

    // Upgrade: the embedded migrator skips 0001..0028 (already recorded) and
    // applies 0029.
    apply_migrations(&pool).await.expect("upgrade applies 0029");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0029 recorded as applied");

    // 1. The pre-existing population survives intact.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The new table + view exist; an upgrading workspace starts with an empty
    //    history.
    let empty: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_history WHERE workspace_id = ?")
        .bind("ws-1")
        .fetch_one(&pool)
        .await
        .expect("count history after upgrade");
    assert_eq!(empty, 0, "upgrading workspace starts with zero runs");
    let rollup_empty: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cost_rollup WHERE workspace_id = ?")
            .bind("ws-1")
            .fetch_one(&pool)
            .await
            .expect("count rollup after upgrade");
    assert_eq!(rollup_empty, 0, "empty history rolls up to nothing");

    // 3. An inserted run row round-trips AND rolls up.
    sqlx::query(
        "INSERT INTO run_history \
         (run_id, task_id, workspace_id, session_id, provider, profile, \
          started_at, finished_at, outcome, input_tokens, output_tokens, cost_usd, \
          diff_add, diff_del) \
         VALUES ('run-1', 'task-1', 'ws-1', 'sess-1', 'claude', NULL, \
                 1000, 2000, 'success', 1200, 340, 0.0231, 12, 3)",
    )
    .execute(&pool)
    .await
    .expect("insert run row");

    let row = sqlx::query(
        "SELECT provider, outcome, input_tokens, output_tokens, cost_usd, diff_add \
         FROM run_history WHERE run_id = 'run-1'",
    )
    .fetch_one(&pool)
    .await
    .expect("read run row");
    assert_eq!(row.get::<String, _>("provider"), "claude");
    assert_eq!(row.get::<String, _>("outcome"), "success");
    assert_eq!(row.get::<i64, _>("input_tokens"), 1200);
    assert_eq!(row.get::<i64, _>("diff_add"), 12);
    assert!((row.get::<f64, _>("cost_usd") - 0.0231).abs() < 1e-9);

    // The rollup view sums the run into its day/provider bucket.
    let rolled = sqlx::query(
        "SELECT input_tokens, cost_usd, runs FROM cost_rollup \
         WHERE workspace_id = 'ws-1' AND provider = 'claude'",
    )
    .fetch_one(&pool)
    .await
    .expect("read rollup row");
    assert_eq!(rolled.get::<i64, _>("input_tokens"), 1200);
    assert_eq!(rolled.get::<i64, _>("runs"), 1);

    // A run whose task_id points at a non-existent task is rejected (the task FK
    // is enforced when present).
    sqlx::query(
        "INSERT INTO run_history \
         (run_id, task_id, workspace_id, provider, finished_at, outcome) \
         VALUES ('run-2', 'task-nope', 'ws-1', 'claude', 3000, 'failed')",
    )
    .execute(&pool)
    .await
    .expect_err("a run with a missing task FK is rejected");

    // But a task-LESS run (NULL task_id) is accepted — the D19 `task_id?` shape.
    sqlx::query(
        "INSERT INTO run_history \
         (run_id, task_id, workspace_id, provider, finished_at, outcome) \
         VALUES ('run-3', NULL, 'ws-1', 'codex', 4000, 'success')",
    )
    .execute(&pool)
    .await
    .expect("a NULL-task run is accepted");

    // 4. Double-apply is idempotent: nothing re-runs, the rows we inserted stay.
    apply_migrations(&pool).await.expect("second apply is a no-op");
    let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_history WHERE workspace_id = ?")
        .bind("ws-1")
        .fetch_one(&pool)
        .await
        .expect("count runs after re-apply");
    assert_eq!(
        still, 2,
        "double-apply must not change any row (run-1 + run-3)"
    );

    pool.close().await;
}
