//! Upgrade-from-populated test for migration 0012 (per-(issue, agent) pending
//! scope).
//!
//! Migration 0012 replaces the migration-0004 `idx_one_pending_task_per_issue`
//! (one pending task per issue across ALL agents) with
//! `idx_one_pending_task_per_issue_agent` scoped to `(issue_id, agent_id)` —
//! Multica's per-(issue, agent) concurrency model, where DIFFERENT agents may
//! each hold a pending task for one issue while the SAME agent still coalesces
//! to one.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0011),
//! 2. seed live rows, including an existing *pending* task (the row the old
//!    index actively guards),
//! 3. apply the embedded migrations (which adds exactly 0012),
//! 4. assert every pre-existing row survives intact, the new per-(issue, agent)
//!    semantics hold, and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0012_pending_per_issue_agent.sql`).
const NEW_MIGRATION_VERSION: i64 = 12;

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

/// Seed the FK graph (workspace, user, runtime, two agents, one issue) plus a
/// realistic task population: one *pending* (`queued`) issue task — the row the
/// old index actively guards —, one terminal (`done`) issue task, and one
/// issue-less chat task.
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
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    for agent_id in ["agent-1", "agent-2"] {
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind("ws-1")
        .bind(format!("Agent {agent_id}"))
        .bind("rt-1")
        .bind("workspace")
        .bind("user-1")
        .execute(pool)
        .await
        .expect("insert agent");
    }
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .bind("An issue")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(0_i64)
    .execute(pool)
    .await
    .expect("insert issue");

    // (id, agent, issue, status): one pending, one terminal, one chat task.
    let tasks: [(&str, &str, Option<&str>, &str); 3] = [
        ("task-pending", "agent-1", Some("issue-1"), "queued"),
        ("task-done", "agent-1", Some("issue-1"), "done"),
        ("task-chat", "agent-2", None, "queued"),
    ];
    for (id, agent_id, issue_id, status) in tasks {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind("ws-1")
        .bind("rt-1")
        .bind(agent_id)
        .bind(issue_id)
        .bind(status)
        .bind(1_i64)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("seed task {id}: {e}"));
    }
}

/// Snapshot the full task population, ordered by id, as
/// `(id, agent_id, issue_id, status)` rows.
async fn task_snapshot(pool: &SqlitePool) -> Vec<(String, String, Option<String>, String)> {
    sqlx::query("SELECT id, agent_id, issue_id, status FROM agent_task_queue ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("snapshot tasks")
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("id"),
                r.get::<String, _>("agent_id"),
                r.get::<Option<String>, _>("issue_id"),
                r.get::<String, _>("status"),
            )
        })
        .collect()
}

/// Insert one `queued` task row, returning the raw sqlx result.
async fn insert_pending(
    pool: &SqlitePool,
    id: &str,
    agent_id: &str,
    issue_id: &str,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, 'ws-1', 'rt-1', ?, ?, 'queued', 2)",
    )
    .bind(id)
    .bind(agent_id)
    .bind(issue_id)
    .execute(pool)
    .await
}

#[tokio::test]
async fn migration_0012_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Pre-upgrade sanity: the OLD global-per-issue index blocks agent-2 from
    // queueing on issue-1 while agent-1's pending task holds the slot.
    let err = insert_pending(&pool, "task-blocked", "agent-2", "issue-1")
        .await
        .expect_err("old index must block a second pending task per issue across agents");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected UNIQUE violation pre-upgrade, got: {err}"
    );

    let before = task_snapshot(&pool).await;
    assert_eq!(before.len(), 3, "seeded population: {before:?}");

    // Upgrade: the embedded migrator skips 0001..0011 (already recorded) and
    // applies exactly 0012.
    apply_migrations(&pool).await.expect("upgrade applies 0012");
    let latest: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read migration version");
    assert_eq!(latest, NEW_MIGRATION_VERSION, "0012 recorded as applied");

    // 1. Every pre-existing row survives intact.
    let after = task_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch table rows");

    // The old index is gone; the new per-(issue, agent) index exists.
    let old_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='index' AND name='idx_one_pending_task_per_issue'",
    )
    .fetch_one(&pool)
    .await
    .expect("count old index");
    assert_eq!(old_count, 0, "old global-per-issue index must be dropped");
    let new_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master \
         WHERE type='index' AND name='idx_one_pending_task_per_issue_agent'",
    )
    .fetch_one(&pool)
    .await
    .expect("new index exists");
    let new_sql = new_sql.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        new_sql.contains("(issue_id, agent_id)"),
        "new index scoped to (issue_id, agent_id): {new_sql}"
    );

    // 2. New semantics: a DIFFERENT agent may now queue on the same issue
    //    even though agent-1's pending task is still queued ...
    insert_pending(&pool, "task-parallel", "agent-2", "issue-1")
        .await
        .expect("post-upgrade, a different agent queues on the same issue");
    // ... while the SAME agent still coalesces to one pending task per issue.
    let err = insert_pending(&pool, "task-dupe", "agent-1", "issue-1")
        .await
        .expect_err("same agent + same issue must still violate the unique index");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected UNIQUE violation for same (issue, agent), got: {err}"
    );

    // 3. Double-apply is idempotent: nothing re-runs, nothing changes.
    let snapshot = task_snapshot(&pool).await;
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        task_snapshot(&pool).await,
        snapshot,
        "double-apply must not change any row"
    );

    pool.close().await;
}
