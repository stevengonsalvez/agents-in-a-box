//! Upgrade-from-populated test for migration 0065 (`issue_cascade_barrier` —
//! the child-done barrier claim ledger, multica parity #3-rest / MUL-4155).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0064),
//! 2. seed a workspace + parent + two staged children + an already-posted
//!    cascade comment (the pre-0065 world),
//! 3. apply the embedded migrations (which adds 0065),
//! 4. assert the table + index exist, the ledger is EMPTY (no backfill), the
//!    pre-existing comment survived, and the PK actually bites.

use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0065_issue_cascade_barrier.sql`).
const NEW_MIGRATION_VERSION: i64 = 65;

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

/// The pre-0065 world: a parent with two staged children, one of which already
/// produced a cascade comment under the old single-frontier code.
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('p-1','ws-1','Parent','open','member','u-amy',10)",
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at, \
          parent_issue_id, stage) \
         VALUES ('c-1','ws-1','A','done','member','u-amy',11,'p-1',1)",
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at, \
          parent_issue_id, stage) \
         VALUES ('c-2','ws-1','B','open','member','u-amy',12,'p-1',2)",
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('cm-1','p-1','member','u-amy','Sub-issue c-1 \"A\" is done. 1/2 sub-issues \
          complete.',13)",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
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

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

#[tokio::test]
async fn migration_0065_adds_an_empty_barrier_ledger_with_a_biting_pk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    assert!(
        !object_exists(&pool, "table", "issue_cascade_barrier").await,
        "issue_cascade_barrier must not exist before 0065"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0065");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0065 recorded as applied");

    // (a) The table and its workspace index landed.
    assert!(object_exists(&pool, "table", "issue_cascade_barrier").await);
    assert!(object_exists(&pool, "index", "idx_issue_cascade_barrier_ws").await);

    // (b) NO BACKFILL — a pre-0065 barrier has no claim row. It cannot re-fire
    //     anyway (re-firing needs a fresh non-terminal → terminal transition).
    assert_eq!(count(&pool, "issue_cascade_barrier").await, 0);

    // (c) The pre-existing cascade comment survived untouched.
    let body: String = sqlx::query_scalar("SELECT body FROM comment WHERE id = 'cm-1'")
        .fetch_one(&pool)
        .await
        .expect("seeded comment survives");
    assert!(body.contains("Sub-issue c-1"), "comment body preserved: {body}");

    // (d) The PK bites: a barrier can be claimed exactly once. This is the whole
    //     dedupe mechanism, so it is asserted at the engine level.
    let claim = "INSERT INTO issue_cascade_barrier \
         (parent_issue_id, workspace_id, stage_key, comment_id, created_at) \
         VALUES ('p-1','ws-1','stage:1:1','cm-1',20)";
    sqlx::query(claim).execute(&pool).await.expect("first claim wins");
    assert!(
        sqlx::query(claim).execute(&pool).await.is_err(),
        "a second claim of the same (parent, stage_key) must violate the PK"
    );
    let ignored = sqlx::query(&claim.replace("INSERT INTO", "INSERT OR IGNORE INTO"))
        .execute(&pool)
        .await
        .expect("INSERT OR IGNORE never errors");
    assert_eq!(
        ignored.rows_affected(),
        0,
        "the losing claimant affects 0 rows — that is how it learns to post nothing"
    );

    // A DIFFERENT stage_key under the same parent is a distinct barrier.
    sqlx::query(
        "INSERT INTO issue_cascade_barrier \
         (parent_issue_id, workspace_id, stage_key, comment_id, created_at) \
         VALUES ('p-1','ws-1','stage:2:1','cm-1',21)",
    )
    .execute(&pool)
    .await
    .expect("distinct stage_key is a distinct barrier");
    assert_eq!(count(&pool, "issue_cascade_barrier").await, 2);

    // (e) Re-applying is a ledgered no-op.
    apply_migrations(&pool).await.expect("second apply is a no-op");
}
