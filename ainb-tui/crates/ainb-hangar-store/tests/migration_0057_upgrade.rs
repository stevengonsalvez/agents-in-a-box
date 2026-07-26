//! Upgrade-from-populated test for migration 0057 (`autopilot.api_trigger_enabled`
//! + `autopilot_run.{status skipped, source, failure_reason}` — multica parity
//! item 15).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database, which matters far
//! more here than for the usual ADD COLUMN migration: 0057 REBUILDS
//! `autopilot_run` (SQLite cannot widen a `CHECK` in place) while
//! `agent_task_queue.autopilot_run_id` holds a live foreign key into it.
//!
//! 1. apply only the PRIOR migrations (0001..0056),
//! 2. seed a workspace / user / runtime / agent / autopilot, two
//!    `autopilot_run` rows (one in-flight `running`, one `completed`) and an
//!    `agent_task_queue` row whose `autopilot_run_id` points at the in-flight run,
//! 3. apply the embedded migrations (which adds 0057),
//! 4. assert the rows survived byte-identically, backfilled their new columns,
//!    the child FK still resolves (`PRAGMA foreign_key_check` empty), the
//!    dropped-with-the-table index is back, and the widened CHECK accepts
//!    `skipped` / `api` while still rejecting junk.

use ainb_hangar_store::apply_migrations;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0057_autopilot_api_trigger_and_skipped_run.sql`).
const NEW_MIGRATION_VERSION: i64 = 57;

/// Open a fresh on-disk WAL pool in `dir` and apply only the migrations PRIOR to
/// [`NEW_MIGRATION_VERSION`]. Foreign keys are ON (the crate's production
/// setting) — that is exactly the condition the naive rebuild would fail under.
async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .foreign_keys(true)
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

/// Seed the full FK chain plus two runs and a task pointing at the in-flight run.
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-1','a@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-1')",
        "INSERT INTO autopilot (id, workspace_id, agent_id, name, cron_expr, created_at) \
         VALUES ('ap-1','ws-1','agent-1','daily','0 9 * * *',0)",
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, completed_at, status) \
         VALUES ('run-open','ap-1',100,NULL,'running')",
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, completed_at, status) \
         VALUES ('run-done','ap-1',50,80,'completed')",
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, status, created_at, autopilot_run_id) \
         VALUES ('task-1','ws-1','rt-1','agent-1','queued',100,'run-open')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

/// The load-bearing upgrade proof: the rebuild preserves the table and its
/// inbound foreign key.
#[tokio::test]
async fn upgrades_populated_db_preserving_runs_and_child_fk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Pre-condition: `skipped` is genuinely rejected today.
    let rejected = sqlx::query(
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, status) \
         VALUES ('run-pre','ap-1',1,'skipped')",
    )
    .execute(&pool)
    .await;
    assert!(
        rejected.is_err(),
        "pre-0057 schema must reject status='skipped'"
    );

    // THE COMMIT THAT USED TO FAIL: the rebuild under an enforced inbound FK.
    apply_migrations(&pool).await.expect("0057 applies");

    // Both run rows survive with identical identity/time/status columns…
    let rows = sqlx::query(
        "SELECT id, autopilot_id, started_at, completed_at, status, source, failure_reason \
         FROM autopilot_run ORDER BY started_at",
    )
    .fetch_all(&pool)
    .await
    .expect("read runs");
    assert_eq!(rows.len(), 2, "no run row was lost by the rebuild");

    let done = &rows[0];
    assert_eq!(done.get::<String, _>("id"), "run-done");
    assert_eq!(done.get::<String, _>("autopilot_id"), "ap-1");
    assert_eq!(done.get::<i64, _>("started_at"), 50);
    assert_eq!(done.get::<Option<i64>, _>("completed_at"), Some(80));
    assert_eq!(done.get::<String, _>("status"), "completed");

    let open = &rows[1];
    assert_eq!(open.get::<String, _>("id"), "run-open");
    assert_eq!(open.get::<i64, _>("started_at"), 100);
    assert_eq!(open.get::<Option<i64>, _>("completed_at"), None);
    assert_eq!(open.get::<String, _>("status"), "running");

    // …and backfill to the documented defaults. Provenance is NOT invented.
    for row in &rows {
        assert_eq!(
            row.get::<String, _>("source"),
            "schedule",
            "pre-0057 rows backfill to source='schedule'"
        );
        assert_eq!(row.get::<Option<String>, _>("failure_reason"), None);
    }

    // The child FK still resolves: the task was neither orphaned nor deleted.
    let task_run: Option<String> =
        sqlx::query_scalar("SELECT autopilot_run_id FROM agent_task_queue WHERE id = 'task-1'")
            .fetch_one(&pool)
            .await
            .expect("read task");
    assert_eq!(task_run.as_deref(), Some("run-open"));

    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .expect("foreign_key_check");
    assert!(
        violations.is_empty(),
        "the rebuild must leave no dangling foreign keys"
    );

    // The index dropped with the table is recreated (it serves `list_runs` AND
    // the scheduler's in-flight count).
    let idx: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_autopilot_run_autopilot_started'",
    )
    .fetch_one(&pool)
    .await
    .expect("read sqlite_master");
    assert_eq!(idx, 1, "idx_autopilot_run_autopilot_started must be back");
}

/// The CHECK was WIDENED, not dropped: `skipped`/`api` are accepted, junk is not.
#[tokio::test]
async fn widened_checks_accept_skipped_and_api_but_still_reject_junk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("0057 applies");

    sqlx::query(
        "INSERT INTO autopilot_run \
         (id, autopilot_id, started_at, completed_at, status, source, failure_reason) \
         VALUES ('run-skip','ap-1',200,200,'skipped','api','concurrency limit: 1/1 in flight')",
    )
    .execute(&pool)
    .await
    .expect("status='skipped' + source='api' must now be accepted");

    let bad_status = sqlx::query(
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, status) \
         VALUES ('run-bogus','ap-1',1,'bogus')",
    )
    .execute(&pool)
    .await;
    assert!(bad_status.is_err(), "status CHECK must still reject junk");

    let bad_source = sqlx::query(
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, status, source) \
         VALUES ('run-bogus2','ap-1',1,'running','bogus')",
    )
    .execute(&pool)
    .await;
    assert!(bad_source.is_err(), "source CHECK must reject junk");
}

/// Pre-existing autopilots stay api-OFF until an operator opts in.
#[tokio::test]
async fn preexisting_autopilots_read_api_trigger_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("0057 applies");

    let enabled: i64 =
        sqlx::query_scalar("SELECT api_trigger_enabled FROM autopilot WHERE id = 'ap-1'")
            .fetch_one(&pool)
            .await
            .expect("read api_trigger_enabled");
    assert_eq!(enabled, 0, "api trigger defaults OFF for upgraded rows");

    let bad = sqlx::query("UPDATE autopilot SET api_trigger_enabled = 7 WHERE id = 'ap-1'")
        .execute(&pool)
        .await;
    assert!(
        bad.is_err(),
        "api_trigger_enabled CHECK must reject non-0/1"
    );
}
