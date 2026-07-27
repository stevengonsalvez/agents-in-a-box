//! Upgrade-from-populated test for migration 0062 (`issue_subscriber` +
//! `issue_reaction` — multica parity #22).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the BACKFILL (the reference's `016_backfill_subscribers`) on a REAL
//! populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0061),
//! 2. seed a workspace plus issues that already have a creator, an assignee and
//!    comments by distinct authors,
//! 3. apply the embedded migrations (which adds 0062),
//! 4. assert the seeded set became subscriptions with the RIGHT provenance, and
//!    that an actor who both created and commented keeps `reason='creator'`
//!    (first-reason-wins through the `OR IGNORE` statement ordering).
//!
//! Without the backfill an upgrading install is born empty, so
//! `inbox_aggregator::recipients_for` would fall back to the participant
//! approximation forever on every pre-existing issue.

use ainb_hangar_store::apply_migrations;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0062_issue_subscriber_reaction.sql`).
const NEW_MIGRATION_VERSION: i64 = 62;

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

/// The pre-0062 world: one issue with a creator AND an assignee plus two
/// comments (one by a third party, one by the CREATOR), and one issue with a
/// creator only.
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, \
             assignee_type, assignee_id, created_at) \
         VALUES ('iss-1','ws-1','With assignee','member','alice','agent','bot-7',100)",
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
         VALUES ('iss-2','ws-1','Solo','member','alice',200)",
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('c-1','iss-1','member','carol','hi',300)",
        // The CREATOR also comments — must NOT downgrade to 'commenter'.
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('c-2','iss-1','member','alice','mine',400)",
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

/// `(actor_type:actor_id, reason)` for one issue, sorted for a stable compare.
async fn subs(pool: &SqlitePool, issue_id: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> =
        sqlx::query("SELECT actor_type, actor_id, reason FROM issue_subscriber WHERE issue_id = ?")
            .bind(issue_id)
            .fetch_all(pool)
            .await
            .expect("read subscribers")
            .iter()
            .map(|r| {
                (
                    format!(
                        "{}:{}",
                        r.get::<String, _>("actor_type"),
                        r.get::<String, _>("actor_id")
                    ),
                    r.get::<String, _>("reason"),
                )
            })
            .collect();
    v.sort();
    v
}

#[tokio::test]
async fn migration_0062_backfills_subscribers_first_reason_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    assert!(
        !table_exists(&pool, "issue_subscriber").await,
        "issue_subscriber must not exist before 0062"
    );

    apply_migrations(&pool).await.expect("upgrade applies 0062");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0062 recorded as applied");

    // (a) Both tables and both indexes landed.
    assert!(table_exists(&pool, "issue_subscriber").await);
    assert!(table_exists(&pool, "issue_reaction").await);
    assert!(index_exists(&pool, "idx_issue_subscriber_actor").await);
    assert!(index_exists(&pool, "idx_issue_reaction_issue").await);

    // (b) The backfill: creator + assignee + the third-party commenter, and the
    //     creator-who-also-commented keeps `creator` (first reason wins).
    assert_eq!(
        subs(&pool, "iss-1").await,
        vec![
            ("agent:bot-7".to_string(), "assignee".to_string()),
            ("member:alice".to_string(), "creator".to_string()),
            ("member:carol".to_string(), "commenter".to_string()),
        ]
    );
    assert_eq!(
        subs(&pool, "iss-2").await,
        vec![("member:alice".to_string(), "creator".to_string())],
        "an unassigned, uncommented issue backfills its creator only"
    );

    // (c) The PK really is the (issue, actor) triple.
    let dup = sqlx::query(
        "INSERT INTO issue_subscriber (issue_id, actor_type, actor_id, reason, created_at) \
         VALUES ('iss-1','member','alice','manual',999)",
    )
    .execute(&pool)
    .await;
    assert!(
        dup.is_err(),
        "the (issue, actor) primary key rejects a duplicate"
    );

    // (d) Re-applying is a ledgered no-op — the backfill does not double up.
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(subs(&pool, "iss-1").await.len(), 3);
}
