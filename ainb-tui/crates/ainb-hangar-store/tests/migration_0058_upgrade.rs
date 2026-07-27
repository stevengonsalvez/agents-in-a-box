//! Upgrade-from-populated test for migration 0058 (`dispatch_attempt` —
//! multica parity #12).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0057),
//! 2. seed a workspace / user / runtime / agent / issue / task graph,
//! 3. apply the embedded migrations (which adds 0058),
//! 4. assert the pre-existing rows survived, the table + both indexes exist, a
//!    record-and-read works, the FK on `workspace_id` is enforced while
//!    `issue_id` deliberately is NOT, and the un-CHECKed `reason` column accepts
//!    a token this binary does not know (the append-only vocabulary contract).

use ainb_hangar_store::apply_migrations;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test (`0058_dispatch_attempt.sql`).
const NEW_MIGRATION_VERSION: i64 = 58;

/// Open a fresh on-disk WAL pool in `dir` and apply only the migrations PRIOR to
/// [`NEW_MIGRATION_VERSION`], with foreign keys ON (the crate's production
/// setting).
async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .expect("open pool");

    let mut migrator = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .expect("load migrations directory");
    migrator
        .migrations
        .to_mut()
        .retain(|m| m.version < NEW_MIGRATION_VERSION);
    assert!(
        !migrator.migrations.is_empty(),
        "prior-migration set must not be empty"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-1','a@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local','online')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-1')",
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('iss-1','ws-1','Card','open','member','user-1',0)",
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES ('task-1','ws-1','rt-1','agent-1','iss-1','queued',100)",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

async fn table_exists(pool: &SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("sqlite_master")
        > 0
}

#[tokio::test]
async fn upgrades_populated_db_and_creates_the_audit_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;

    // Pre-condition: the table genuinely does not exist yet.
    assert!(
        !table_exists(&pool, "dispatch_attempt").await,
        "dispatch_attempt must not predate 0058"
    );
    seed_populated(&pool).await;

    apply_migrations(&pool).await.expect("0058 applies");

    assert!(table_exists(&pool, "dispatch_attempt").await);

    // The pre-existing graph survived untouched.
    let issues: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(&pool)
        .await
        .expect("count issues");
    assert_eq!(issues, 1);
    let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
        .fetch_one(&pool)
        .await
        .expect("count tasks");
    assert_eq!(tasks, 1);

    // Both indexes from the migration exist.
    let indexes: Vec<String> = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'dispatch_attempt' \
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("index list")
    .iter()
    .map(|r| r.get::<String, _>("name"))
    .collect();
    assert!(
        indexes.iter().any(|n| n == "idx_dispatch_attempt_issue"),
        "per-issue index missing: {indexes:?}"
    );
    assert!(
        indexes.iter().any(|n| n == "idx_dispatch_attempt_ws"),
        "per-workspace index missing: {indexes:?}"
    );

    // A record + read works on the upgraded database.
    sqlx::query(
        "INSERT INTO dispatch_attempt \
         (id, workspace_id, issue_id, agent_id, runtime_id, task_id, reason, detail, source, created_at) \
         VALUES ('att-1','ws-1','iss-1','agent-1','rt-1','task-1','runtime_offline','rt-1 is offline','manual',200)",
    )
    .execute(&pool)
    .await
    .expect("record on upgraded db");
    let row = sqlx::query("SELECT reason, detail, task_id, source FROM dispatch_attempt WHERE id = 'att-1'")
        .fetch_one(&pool)
        .await
        .expect("read back");
    assert_eq!(row.get::<String, _>("reason"), "runtime_offline");
    assert_eq!(row.get::<String, _>("source"), "manual");
    assert_eq!(row.get::<Option<String>, _>("task_id").as_deref(), Some("task-1"));
    assert_eq!(
        row.get::<Option<String>, _>("detail").as_deref(),
        Some("rt-1 is offline")
    );

    // `source` defaults when omitted.
    sqlx::query(
        "INSERT INTO dispatch_attempt (id, workspace_id, issue_id, reason, created_at) \
         VALUES ('att-2','ws-1','iss-1','queued',201)",
    )
    .execute(&pool)
    .await
    .expect("default source");
    let src: String =
        sqlx::query_scalar("SELECT source FROM dispatch_attempt WHERE id = 'att-2'")
            .fetch_one(&pool)
            .await
            .expect("read source");
    assert_eq!(src, "manual");

    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .expect("fk check")
            .is_empty(),
        "no dangling foreign keys after the upgrade"
    );
}

/// Migration decision 1: no CHECK on `reason`, so a future daemon's token
/// persists. Decision 2: `workspace_id` is the ONLY foreign key, so an audit row
/// survives the death of the issue / agent / task it describes.
#[tokio::test]
async fn vocabulary_is_open_and_only_workspace_is_a_foreign_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("0058 applies");

    // An unknown code stores fine (append-only vocabulary, enforced in Rust).
    sqlx::query(
        "INSERT INTO dispatch_attempt (id, workspace_id, issue_id, reason, created_at) \
         VALUES ('att-future','ws-1','iss-1','a_code_from_2027',1)",
    )
    .execute(&pool)
    .await
    .expect("unknown reason accepted (no CHECK by design)");

    // Ids that carry no FK: a row about entities that never existed is legal,
    // which is what lets the audit outlive what it describes.
    sqlx::query(
        "INSERT INTO dispatch_attempt \
         (id, workspace_id, issue_id, agent_id, runtime_id, task_id, reason, created_at) \
         VALUES ('att-ghost','ws-1','iss-gone','agent-gone','rt-gone','task-gone','already_active',2)",
    )
    .execute(&pool)
    .await
    .expect("no FK on issue/agent/runtime/task by design");

    // …but the one FK that IS declared is enforced.
    let bad_ws = sqlx::query(
        "INSERT INTO dispatch_attempt (id, workspace_id, reason, created_at) \
         VALUES ('att-bad','ws-nope','queued',3)",
    )
    .execute(&pool)
    .await;
    assert!(bad_ws.is_err(), "workspace_id FK must be enforced");
}
