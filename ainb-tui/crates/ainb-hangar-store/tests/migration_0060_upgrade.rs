//! Upgrade-from-populated test for migration 0060 (`inbox_entry.recipient_type`
//! / `recipient_id` — multica parity #1).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database that already has
//! pre-0060, workspace-wide inbox rows:
//!
//! 1. apply only the PRIOR migrations (0001..0059),
//! 2. seed a workspace / user / member / issue graph plus a legacy `inbox_entry`
//!    row written with the pre-0060 column list,
//! 3. apply the embedded migrations (which adds 0060),
//! 4. assert the legacy row SURVIVES and backfills to the local human
//!    (`member:me`) so no upgrading install loses a notification, that the
//!    recipient CHECK rejects a foreign family, that both new indexes exist, and
//!    that a second apply is a no-op.

use ainb_hangar_store::apply_migrations;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test (`0060_inbox_recipient.sql`).
const NEW_MIGRATION_VERSION: i64 = 60;

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

/// Seed the pre-0060 world: a workspace with a member, an issue, and ONE legacy
/// inbox row written with the pre-0060 column list (no recipient columns exist
/// yet, so this insert is only possible before the upgrade).
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-1','a@example.com',0)",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','user-1','owner')",
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('iss-1','ws-1','Card','open','member','user-1',0)",
        "INSERT INTO inbox_entry \
         (id, workspace_id, kind, event, subject_id, summary, created_at, read_at) \
         VALUES ('ie-legacy','ws-1','issue','issue_created','iss-1','New issue: Card',100,NULL)",
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
async fn migration_0060_backfills_legacy_rows_to_the_local_human() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Pre-condition: the recipient columns genuinely do not exist yet.
    assert!(
        !column_exists(&pool, "inbox_entry", "recipient_type").await,
        "recipient_type must not exist before 0060"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0060");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0060 recorded as applied");

    // 1. The legacy row survives and is now addressed to the LOCAL HUMAN, so an
    //    upgrading install loses nothing from the human's inbox.
    let row = sqlx::query(
        "SELECT recipient_type, recipient_id, summary, read_at FROM inbox_entry WHERE id = ?",
    )
    .bind("ie-legacy")
    .fetch_one(&pool)
    .await
    .expect("legacy row survives");
    assert_eq!(row.get::<String, _>("recipient_type"), "member");
    assert_eq!(row.get::<String, _>("recipient_id"), "me");
    assert_eq!(row.get::<String, _>("summary"), "New issue: Card");
    assert_eq!(
        row.get::<Option<i64>, _>("read_at"),
        None,
        "the legacy row keeps its unread state"
    );

    // 2. A recipient-scoped read finds it under `member:me` and nowhere else.
    let mine: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry \
         WHERE workspace_id = ? AND recipient_type = 'member' AND recipient_id = 'me'",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("count member:me");
    assert_eq!(mine, 1, "the backfilled row reads under member:me");
    let theirs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry \
         WHERE workspace_id = ? AND recipient_type = 'agent'",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("count agents");
    assert_eq!(theirs, 0, "no agent inherits the legacy row");

    // 3. An agent-addressed insert lands and is disjoint from the human's rows.
    sqlx::query(
        "INSERT INTO inbox_entry \
         (id, workspace_id, recipient_type, recipient_id, kind, event, subject_id, summary, created_at) \
         VALUES ('ie-agent', ?, 'agent', 'a1', 'task', 'task_queued', 't-1', 'Task queued: t-1', 200)",
    )
    .bind("ws-1")
    .execute(&pool)
    .await
    .expect("agent-addressed insert");
    let mine_again: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry \
         WHERE workspace_id = ? AND recipient_type = 'member' AND recipient_id = 'me'",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("count member:me again");
    assert_eq!(
        mine_again, 1,
        "the agent's entry does not appear in the human's inbox"
    );

    // 4. The CHECK constraint rejects a foreign recipient family.
    let bad = sqlx::query(
        "INSERT INTO inbox_entry \
         (id, workspace_id, recipient_type, recipient_id, kind, event, subject_id, summary, created_at) \
         VALUES ('ie-bad', ?, 'bogus', 'x', 'issue', 'issue_created', 'iss-1', 'm', 300)",
    )
    .bind("ws-1")
    .execute(&pool)
    .await;
    assert!(
        bad.is_err(),
        "an unknown recipient_type must violate the CHECK constraint"
    );

    // 5. Both recipient indexes exist (the list + badge covering paths).
    assert!(index_exists(&pool, "idx_inbox_entry_recipient_created").await);
    assert!(index_exists(&pool, "idx_inbox_entry_recipient_unread").await);

    // 6. Double-apply is idempotent: nothing re-runs, the rows stay as they are.
    apply_migrations(&pool).await.expect("second apply is a no-op");
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbox_entry")
        .fetch_one(&pool)
        .await
        .expect("count after re-apply");
    assert_eq!(total, 2, "double-apply must not change any row");

    pool.close().await;
}
