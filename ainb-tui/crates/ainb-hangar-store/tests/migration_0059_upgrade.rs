//! Upgrade-from-populated test for migration 0059 (`activity_log` — multica
//! parity #13).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0058),
//! 2. seed a workspace / user / issue / comment graph,
//! 3. apply the embedded migrations (which adds 0059),
//! 4. assert the pre-existing rows survived, the table + both indexes exist, a
//!    record-and-read round-trips, `details` defaults to `{}`, the FK on
//!    `workspace_id` is enforced while `issue_id` deliberately is NOT, and the
//!    un-CHECKed `action` / `actor_type` columns accept tokens this binary does
//!    not know (the append-only vocabulary contract).

use ainb_hangar_store::apply_migrations;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test (`0059_activity_log.sql`).
const NEW_MIGRATION_VERSION: i64 = 59;

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

async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-1','a@example.com',0)",
        "INSERT INTO issue \
         (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES ('iss-1','ws-1','Card','open','member','user-1',0)",
        "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
         VALUES ('cmt-1','iss-1','member','user-1','hello',10)",
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
async fn upgrades_populated_db_and_creates_the_activity_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;

    // Pre-condition: the table genuinely does not exist yet.
    assert!(
        !table_exists(&pool, "activity_log").await,
        "activity_log must not predate 0059"
    );
    seed_populated(&pool).await;

    apply_migrations(&pool).await.expect("0059 applies");

    assert!(table_exists(&pool, "activity_log").await);

    // The pre-existing graph survived untouched.
    let issues: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(&pool)
        .await
        .expect("count issues");
    assert_eq!(issues, 1);
    let comments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment")
        .fetch_one(&pool)
        .await
        .expect("count comments");
    assert_eq!(comments, 1);

    // Both indexes from the migration exist.
    let indexes: Vec<String> = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'activity_log' \
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("index list")
    .iter()
    .map(|r| r.get::<String, _>("name"))
    .collect();
    assert!(
        indexes.iter().any(|n| n == "idx_activity_log_issue"),
        "per-issue index missing: {indexes:?}"
    );
    assert!(
        indexes.iter().any(|n| n == "idx_activity_log_ws"),
        "per-workspace index missing: {indexes:?}"
    );

    // A record + read works on the upgraded database.
    sqlx::query(
        "INSERT INTO activity_log \
         (id, workspace_id, issue_id, actor_type, actor_id, action, details, created_at) \
         VALUES ('act-1','ws-1','iss-1','member','user-1','status_changed', \
                 '{\"from\":\"open\",\"to\":\"in_progress\"}',200)",
    )
    .execute(&pool)
    .await
    .expect("record on upgraded db");
    let row = sqlx::query(
        "SELECT actor_type, actor_id, action, details FROM activity_log WHERE id = 'act-1'",
    )
    .fetch_one(&pool)
    .await
    .expect("read back");
    assert_eq!(row.get::<String, _>("action"), "status_changed");
    assert_eq!(
        row.get::<Option<String>, _>("actor_type").as_deref(),
        Some("member")
    );
    assert_eq!(
        row.get::<String, _>("details"),
        "{\"from\":\"open\",\"to\":\"in_progress\"}"
    );

    // `details` defaults to an empty object when omitted, and a system row
    // stores a NULL actor_id.
    sqlx::query(
        "INSERT INTO activity_log (id, workspace_id, issue_id, actor_type, action, created_at) \
         VALUES ('act-2','ws-1','iss-1','system','created',201)",
    )
    .execute(&pool)
    .await
    .expect("default details");
    let row = sqlx::query("SELECT details, actor_id FROM activity_log WHERE id = 'act-2'")
        .fetch_one(&pool)
        .await
        .expect("read defaults");
    assert_eq!(row.get::<String, _>("details"), "{}");
    assert_eq!(row.get::<Option<String>, _>("actor_id"), None);

    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .expect("fk check")
            .is_empty(),
        "no dangling foreign keys after the upgrade"
    );
}

/// Migration decision 1: no CHECK on `action` / `actor_type`, so a future
/// daemon's token persists. Decision 3: `workspace_id` is the ONLY foreign key,
/// so an activity row survives the death of the issue / actor it describes.
#[tokio::test]
async fn vocabulary_is_open_and_only_workspace_is_a_foreign_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("0059 applies");

    // An unknown action + an unknown actor kind store fine (append-only
    // vocabulary, enforced in Rust).
    sqlx::query(
        "INSERT INTO activity_log (id, workspace_id, issue_id, actor_type, action, created_at) \
         VALUES ('act-future','ws-1','iss-1','robot','teleported_from_2027',1)",
    )
    .execute(&pool)
    .await
    .expect("unknown action/actor accepted (no CHECK by design)");

    // Ids that carry no FK: a row about entities that never existed is legal,
    // which is what lets the narrative outlive what it describes.
    sqlx::query(
        "INSERT INTO activity_log \
         (id, workspace_id, issue_id, actor_type, actor_id, action, created_at) \
         VALUES ('act-ghost','ws-1','iss-gone','member','user-gone','created',2)",
    )
    .execute(&pool)
    .await
    .expect("no FK on issue/actor by design");

    // …but the one FK that IS declared is enforced.
    let bad_ws = sqlx::query(
        "INSERT INTO activity_log (id, workspace_id, action, created_at) \
         VALUES ('act-bad','ws-nope','created',3)",
    )
    .execute(&pool)
    .await;
    assert!(bad_ws.is_err(), "workspace_id FK must be enforced");
}
