//! Upgrade-from-populated test for migration 0061 (`autopilot_rule_version` +
//! the `autopilot_run` attribution pair — multica parity #14).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database that already has
//! pre-0061 autopilots and runs:
//!
//! 1. apply only the PRIOR migrations (0001..0060),
//! 2. seed a workspace / user / runtime / agent graph plus a legacy `autopilot`
//!    and a legacy `autopilot_run` written with the pre-0061 column list,
//! 3. apply the embedded migrations (which adds 0061),
//! 4. assert the DELIBERATE NON-BACKFILL: the legacy autopilot has ZERO version
//!    rows and the legacy run reads NULL/NULL — an honest unknown, never a
//!    fabricated audit record — that the first edit after upgrade mints v1,
//!    that both new indexes exist (including the monotonic-sequence guard), and
//!    that a second apply is a no-op.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
use ainb_hangar_store::apply_migrations;
use ainb_hangar_store::repo::autopilot::{AutopilotEdit, AutopilotRepo, UpdateOutcome};
use ainb_hangar_store::repo::autopilot_rule_version::AutopilotRuleVersionRepo;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0061_autopilot_rule_version.sql`).
const NEW_MIGRATION_VERSION: i64 = 61;

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

/// Seed the pre-0061 world: a workspace, an agent, ONE autopilot and ONE run,
/// both written with the pre-0061 column list.
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-1','a@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-1')",
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, instructions, cron_expr, max_concurrent_runs, \
          execution_mode, concurrency_policy, next_tick_at, enabled, created_at) \
         VALUES ('ap-legacy','ws-1','agent-1','legacy','old instructions','0 9 * * *',1, \
                 'run_only','skip',NULL,1,100)",
        "INSERT INTO autopilot_run \
         (id, autopilot_id, started_at, completed_at, status, source) \
         VALUES ('run-legacy','ap-legacy',200,260,'completed','schedule')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

async fn index_exists(pool: &SqlitePool, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("sqlite_master")
        > 0
}

async fn column_exists(pool: &SqlitePool, table: &str, column: &str) -> bool {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .expect("table_info")
        .iter()
        .any(|r| r.get::<String, _>("name") == column)
}

#[tokio::test]
async fn migration_0061_adds_the_ledger_without_fabricating_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Pre-condition: nothing from 0061 exists yet.
    assert!(
        !column_exists(&pool, "autopilot_run", "accountable_actor").await,
        "accountable_actor must not exist before 0061"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0061");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0061 recorded as applied");

    // (a) NO BACKFILL. A pre-0061 autopilot has ZERO version rows: inventing a
    //     v1 with `published_by = member:me` and `created_at = now()` for a rule
    //     created months ago would be a FABRICATED audit record.
    let versions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM autopilot_rule_version WHERE autopilot_id = ?")
            .bind("ap-legacy")
            .fetch_one(&pool)
            .await
            .expect("count versions");
    assert_eq!(
        versions, 0,
        "a pre-0061 autopilot stays UNVERSIONED — no fabricated v1"
    );

    // (b) The legacy run survives untouched and reads NULL/NULL: unknown, not
    //     misattributed.
    let row = sqlx::query(
        "SELECT status, source, accountable_actor, attribution, started_at \
         FROM autopilot_run WHERE id = ?",
    )
    .bind("run-legacy")
    .fetch_one(&pool)
    .await
    .expect("legacy run survives");
    assert_eq!(row.get::<String, _>("status"), "completed");
    assert_eq!(row.get::<String, _>("source"), "schedule");
    assert_eq!(row.get::<i64, _>("started_at"), 200);
    assert_eq!(
        row.get::<Option<String>, _>("accountable_actor"),
        None,
        "a pre-0061 run is an honest unknown, never misattributed"
    );
    assert_eq!(row.get::<Option<String>, _>("attribution"), None);

    // (c) The first substantive edit AFTER upgrade mints version 1 —
    //     `MAX(version)+1` over an empty set.
    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let id = AutopilotId::from_str("ap-legacy").unwrap();
    let outcome = AutopilotRepo::update_as(
        &pool,
        &FixedClock(1_767_225_600_000),
        &ws,
        &id,
        &AutopilotEdit {
            instructions: Some(Some("new instructions".to_string())),
            ..AutopilotEdit::default()
        },
        Some(&ActorRef::new(ActorKind::Member, "user-1").unwrap()),
    )
    .await
    .expect("first edit after upgrade");
    assert_eq!(
        outcome,
        UpdateOutcome::Updated { version: Some(1) },
        "an unversioned rule's first substantive edit mints v1, not v2"
    );
    let latest = AutopilotRuleVersionRepo::latest(&pool, &ws, &id)
        .await
        .expect("latest")
        .expect("present");
    assert_eq!(latest.change_kind, "instructions");
    assert_eq!(latest.published_by.as_deref(), Some("member:user-1"));

    // (d) Both indexes exist, and the sequence guard really is UNIQUE.
    assert!(index_exists(&pool, "idx_autopilot_rule_version_seq").await);
    assert!(index_exists(&pool, "idx_autopilot_rule_version_latest").await);
    let dup = sqlx::query(
        "INSERT INTO autopilot_rule_version \
         (id, workspace_id, autopilot_id, version, change_kind, created_at) \
         VALUES ('dup','ws-1','ap-legacy',1,'created',1)",
    )
    .execute(&pool)
    .await;
    assert!(
        dup.is_err(),
        "the (autopilot_id, version) unique index must reject a duplicate version"
    );

    // (e) `change_kind` carries NO CHECK, deliberately: a newer daemon's token
    //     must be storable, and the Rust parse is what stays tolerant.
    sqlx::query(
        "INSERT INTO autopilot_rule_version \
         (id, workspace_id, autopilot_id, version, change_kind, created_at) \
         VALUES ('future','ws-1','ap-legacy',99,'teleported',1)",
    )
    .execute(&pool)
    .await
    .expect("an unknown change_kind must be storable (no CHECK, migration decision 1)");

    // (f) Re-applying is a no-op.
    apply_migrations(&pool).await.expect("second apply is a no-op");
    let recorded_again: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version again");
    assert_eq!(recorded_again, 1);
}
