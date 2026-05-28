//! Tripwire: the embedded migrations apply cleanly to a fresh `SQLite` DB and
//! create the expected v1 schema. Uses a real on-disk `tempdir/hangar.db`
//! (not `:memory:`) so `WAL` is exercised exactly like production.

use ainb_hangar_store::apply_migrations;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// Open a fresh on-disk `SQLite` pool inside a tempdir and apply all migrations.
async fn fresh_pool(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .expect("valid sqlite url")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .expect("open pool");
    apply_migrations(&pool).await.expect("migrations apply");
    pool
}

/// Return the `sql` definition recorded in `sqlite_master` for a given table,
/// with runs of whitespace collapsed to a single space so column-alignment
/// padding in the migration source does not make substring assertions brittle.
async fn table_sql(pool: &SqlitePool, name: &str) -> String {
    let row = sqlx::query("SELECT sql FROM sqlite_master WHERE type='table' AND name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("table {name} missing: {e}"));
    let raw: String = row.get("sql");
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test]
async fn migrations_apply_to_fresh_sqlite_and_create_workspace_user_member_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    // Exactly the three expected tables exist after 0001.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='table' AND name IN ('workspace','user','member')",
    )
    .fetch_one(&pool)
    .await
    .expect("count query");
    assert_eq!(count, 3, "expected workspace, user, member tables");

    // workspace columns + constraints.
    let ws = table_sql(&pool, "workspace").await;
    assert!(ws.contains("id TEXT PRIMARY KEY"), "workspace.id PK: {ws}");
    assert!(
        ws.contains("slug TEXT NOT NULL UNIQUE"),
        "workspace.slug unique: {ws}"
    );
    assert!(
        ws.contains("name TEXT NOT NULL"),
        "workspace.name not null: {ws}"
    );
    assert!(
        ws.contains("created_at INTEGER NOT NULL"),
        "workspace.created_at epoch millis: {ws}"
    );

    // user columns + constraints.
    let user = table_sql(&pool, "user").await;
    assert!(user.contains("id TEXT PRIMARY KEY"), "user.id PK: {user}");
    assert!(
        user.contains("email TEXT NOT NULL UNIQUE"),
        "user.email unique: {user}"
    );
    assert!(
        user.contains("created_at INTEGER NOT NULL"),
        "user.created_at: {user}"
    );

    // member composite PK + role CHECK.
    let member = table_sql(&pool, "member").await;
    assert!(
        member.contains("PRIMARY KEY (workspace_id, user_id)"),
        "member composite PK: {member}"
    );
    assert!(
        member.contains("role TEXT NOT NULL"),
        "member.role not null: {member}"
    );
    assert!(
        member.contains("CHECK (role IN ('owner','admin','member'))"),
        "member.role CHECK: {member}"
    );

    pool.close().await;
}
