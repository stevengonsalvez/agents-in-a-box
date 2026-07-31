//! Upgrade-from-populated test for migration 0074 (`role_gated_pull_pipeline` ,
//! the `board_column` role gate + WIP cap + prior-agent exclusion, and the
//! `agent_task_queue.run_group` fan-out cluster stamp).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0073),
//! 2. seed a workspace with a board, two columns, a card and a task, the
//!    pre-0074 world, in which no column can be role-gated,
//! 3. apply the embedded migrations (which adds 0074),
//! 4. assert the four new columns landed, that every PRE-EXISTING row took the
//!    inert default (so no operator's existing board changes behaviour), that
//!    the sibling payloads survived byte-identical, and that all three new
//!    indexes exist.
//!
//! Assertion (b) is the load-bearing one. `services_role IS NULL` means "not a
//! pull queue", so a board that predates this migration must come out the other
//! side with every column still unpullable. If a future edit gave
//! `services_role` a non-NULL default, every existing column would silently
//! become a role-gated queue and start pulling work, this test is what turns
//! that red.

use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0074_role_gated_pull_pipeline.sql`).
const NEW_MIGRATION_VERSION: i64 = 74;

/// The exact column name seeded before the upgrade, re-read verbatim after, so
/// an ALTER TABLE that rewrote a sibling column would show.
const SEEDED_COLUMN_NAME: &str = "In Progress";
/// Same, for the seeded column's `fsm_state`, the pre-existing stage concept
/// this migration must reuse rather than duplicate.
const SEEDED_FSM_STATE: &str = "in_progress";

async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(dir.join("hangar.db"))
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
    assert!(!migrator.migrations.is_empty());
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// The pre-0074 world: a board with two columns, a card parked on one of them,
/// and a queued task on that card's issue. Every shape the pull statement will
/// later read, seeded BEFORE the columns it reads exist.
async fn seed_populated(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(pool)
        .await
        .expect("seed workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-amy','amy@x.dev',0)")
        .execute(pool)
        .await
        .expect("seed user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','d-1','claude','local')",
    )
    .execute(pool)
    .await
    .expect("seed runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1','ws-1','claude','rt-1','workspace','u-amy')",
    )
    .execute(pool)
    .await
    .expect("seed agent");
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('i-1','ws-1','Ship the pull pipeline','open','member','u-amy',10)",
    )
    .execute(pool)
    .await
    .expect("seed issue");
    sqlx::query(
        "INSERT INTO board (id, workspace_id, name, created_at) VALUES ('b-1','ws-1','Kanban',5)",
    )
    .execute(pool)
    .await
    .expect("seed board");
    sqlx::query(
        "INSERT INTO board_column (id, board_id, ord, name, fsm_state, auto_move) \
         VALUES ('col-1','b-1',0,'Backlog',NULL,0)",
    )
    .execute(pool)
    .await
    .expect("seed first column");
    sqlx::query(
        "INSERT INTO board_column (id, board_id, ord, name, fsm_state, auto_move) \
         VALUES ('col-2','b-1',1,?,?,1)",
    )
    .bind(SEEDED_COLUMN_NAME)
    .bind(SEEDED_FSM_STATE)
    .execute(pool)
    .await
    .expect("seed second column");
    sqlx::query(
        "INSERT INTO board_card (board_id, issue_id, column_id, added_at) \
         VALUES ('b-1','i-1','col-2',6)",
    )
    .execute(pool)
    .await
    .expect("seed card");
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES ('t-1','ws-1','rt-1','ag-1','i-1','queued',11)",
    )
    .execute(pool)
    .await
    .expect("seed task");
}

async fn object_exists(pool: &SqlitePool, kind: &str, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_master WHERE type = ? AND name = ?")
        .bind(kind)
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("sqlite_master")
        > 0
}

/// Whether `table` has a column named `column` (reads the `PRAGMA` catalog, so a
/// renamed / missing column is caught rather than surfacing as a query error).
async fn has_column(pool: &SqlitePool, table: &str, column: &str) -> bool {
    sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?"
    ))
    .bind(column)
    .fetch_one(pool)
    .await
    .expect("pragma_table_info")
        > 0
}

#[tokio::test]
async fn migration_0074_adds_the_pull_gate_columns_inert() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Pre-flight: the columns genuinely do not exist yet, so the assertions
    // below cannot pass vacuously against an already-migrated database.
    assert!(
        !has_column(&pool, "board_column", "services_role").await,
        "services_role must not exist before 0074"
    );
    assert!(
        !has_column(&pool, "agent_task_queue", "run_group").await,
        "run_group must not exist before 0074"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0074");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0074 recorded as applied");

    // (a) All four new columns landed.
    assert!(has_column(&pool, "board_column", "services_role").await);
    assert!(has_column(&pool, "board_column", "wip_limit").await);
    assert!(has_column(&pool, "board_column", "excludes_prior_agent").await);
    assert!(has_column(&pool, "agent_task_queue", "run_group").await);

    // (b) NO BACKFILL, and the defaults are INERT. Every pre-existing column is
    //     ungated (NULL services_role = not a pull queue), uncapped (NULL
    //     wip_limit = unlimited) and does not exclude prior agents, so an
    //     operator's existing board behaves EXACTLY as it did before the
    //     upgrade. A non-NULL default here would silently turn every existing
    //     column into a role-gated queue.
    let gates: Vec<(Option<String>, Option<i64>, i64)> = sqlx::query_as(
        "SELECT services_role, wip_limit, excludes_prior_agent \
         FROM board_column ORDER BY ord",
    )
    .fetch_all(&pool)
    .await
    .expect("read new board_column columns");
    assert_eq!(gates.len(), 2, "both seeded columns survive the upgrade");
    for (services_role, wip_limit, excludes_prior_agent) in &gates {
        assert_eq!(
            *services_role, None,
            "pre-existing column must stay ungated"
        );
        assert_eq!(*wip_limit, None, "pre-existing column must stay uncapped");
        assert_eq!(
            *excludes_prior_agent, 0,
            "prior-agent exclusion defaults off"
        );
    }

    let run_group: Option<String> =
        sqlx::query_scalar("SELECT run_group FROM agent_task_queue WHERE id = 't-1'")
            .fetch_one(&pool)
            .await
            .expect("read run_group");
    assert_eq!(
        run_group, None,
        "a pre-existing task belongs to no fan-out cluster"
    );

    // (c) The pre-existing payloads survived BYTE-IDENTICAL, an ALTER TABLE
    //     that rewrote a sibling column would show up here. `fsm_state` and
    //     `auto_move` matter most: 0074 reuses them rather than adding a
    //     parallel stage concept, so they must come through untouched.
    let (name, fsm_state, auto_move): (String, Option<String>, i64) =
        sqlx::query_as("SELECT name, fsm_state, auto_move FROM board_column WHERE id = 'col-2'")
            .fetch_one(&pool)
            .await
            .expect("read preserved columns");
    assert_eq!(name, SEEDED_COLUMN_NAME);
    assert_eq!(fsm_state.as_deref(), Some(SEEDED_FSM_STATE));
    assert_eq!(auto_move, 1);

    let (card_column, card_added): (String, i64) =
        sqlx::query_as("SELECT column_id, added_at FROM board_card WHERE issue_id = 'i-1'")
            .fetch_one(&pool)
            .await
            .expect("card survives");
    assert_eq!(card_column, "col-2", "the card did not move");
    assert_eq!(card_added, 6);

    let (status, created_at): (String, i64) =
        sqlx::query_as("SELECT status, created_at FROM agent_task_queue WHERE id = 't-1'")
            .fetch_one(&pool)
            .await
            .expect("task survives");
    assert_eq!(status, "queued");
    assert_eq!(created_at, 11);

    // (d) All three new indexes landed. The pull statement's plan depends on
    //     them; a missing one degrades the claim loop to a full scan.
    assert!(object_exists(&pool, "index", "idx_board_column_pull").await);
    assert!(object_exists(&pool, "index", "idx_task_issue_status").await);
    assert!(object_exists(&pool, "index", "idx_task_run_group").await);

    // (e) The new columns actually accept the values the pipeline writes, and
    //     the partial index tolerates a gated column alongside the ungated ones.
    sqlx::query(
        "INSERT INTO board_column \
         (id, board_id, ord, name, fsm_state, auto_move, services_role, wip_limit, excludes_prior_agent) \
         VALUES ('col-3','b-1',2,'Review','review',1,'reviewer',3,1)",
    )
    .execute(&pool)
    .await
    .expect("a role-gated column is insertable");
    let (role, wip, excl): (Option<String>, Option<i64>, i64) = sqlx::query_as(
        "SELECT services_role, wip_limit, excludes_prior_agent FROM board_column WHERE id = 'col-3'",
    )
    .fetch_one(&pool)
    .await
    .expect("read back the gated column");
    assert_eq!(role.as_deref(), Some("reviewer"));
    assert_eq!(wip, Some(3));
    assert_eq!(excl, 1);

    // (f) Re-applying is a ledgered no-op.
    apply_migrations(&pool).await.expect("second apply is a no-op");
}
