//! Upgrade-from-populated test for migration 0021 (the aggregated inbox, e38.14).
//!
//! Migration 0021 adds the `inbox_entry` table (plus two indexes) for the
//! aggregated notification inbox. It touches no existing table — a pure additive
//! `CREATE TABLE` / `CREATE INDEX` — so every pre-existing row must survive and
//! each upgrading workspace simply starts with an empty inbox.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0020),
//! 2. seed a live workspace + member + issue (the rows that existed before the
//!    inbox table, the FK target the new `workspace_id` points at),
//! 3. apply the embedded migrations (which adds exactly 0021),
//! 4. assert the pre-existing rows survive intact, the new table exists and is
//!    empty (zero unread for an upgrading workspace), the `read_at`/`NULL` =
//!    unread convention round-trips, and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0021_inbox_entry.sql`).
const NEW_MIGRATION_VERSION: i64 = 21;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only the migrations PRIOR
/// to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install runs
/// before upgrading.
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

/// Seed one workspace + user + member + issue — the rows that pre-date the inbox
/// table, including the `workspace` row the new `inbox_entry.workspace_id` FK
/// points at.
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
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, ?)")
        .bind("ws-1")
        .bind("user-1")
        .bind("owner")
        .execute(pool)
        .await
        .expect("insert member");
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, 'Pre-existing issue', 'open', 'member', 'user-1', 0)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .execute(pool)
    .await
    .expect("insert issue");
}

/// Snapshot the pre-existing entity counts, ordered, as the data the addition
/// must not touch.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in ["workspace", "user", "member", "issue"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count {t}: {e}"));
        out.push((t.to_string(), n));
    }
    out
}

#[tokio::test]
async fn migration_0021_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // The inbox table does NOT exist before the upgrade.
    let exists_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'inbox_entry'",
    )
    .fetch_one(&pool)
    .await
    .expect("probe inbox_entry before");
    assert_eq!(exists_before, 0, "inbox_entry must not exist before 0021");

    // Upgrade: the embedded migrator skips 0001..0020 (already recorded) and
    // applies 0021.
    apply_migrations(&pool).await.expect("upgrade applies 0021");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0021 recorded as applied");

    // 1. The pre-existing population survives intact — the addition touches no
    //    existing row.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The new table now exists and an upgrading workspace starts empty (zero
    //    unread): a pre-0021 install never accumulated inbox rows.
    let empty: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbox_entry WHERE workspace_id = ?")
        .bind("ws-1")
        .fetch_one(&pool)
        .await
        .expect("count inbox after upgrade");
    assert_eq!(empty, 0, "upgrading workspace starts with an empty inbox");

    // 3. The read_at/NULL = unread convention round-trips: an inserted row with
    //    read_at = NULL is unread; setting read_at flips it to read.
    sqlx::query(
        "INSERT INTO inbox_entry \
         (id, workspace_id, kind, event, subject_id, summary, created_at, read_at) \
         VALUES (?, ?, 'issue', 'issue_created', ?, 'New issue: Pre-existing issue', 100, NULL)",
    )
    .bind("ie-1")
    .bind("ws-1")
    .bind("issue-1")
    .execute(&pool)
    .await
    .expect("insert inbox entry");

    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry WHERE workspace_id = ? AND read_at IS NULL",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("count unread");
    assert_eq!(unread, 1, "a fresh inbox row (read_at NULL) is unread");

    // Mark it read by stamping read_at; the unread count drops to zero.
    sqlx::query("UPDATE inbox_entry SET read_at = 200 WHERE id = ?")
        .bind("ie-1")
        .execute(&pool)
        .await
        .expect("mark read");
    let unread_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry WHERE workspace_id = ? AND read_at IS NULL",
    )
    .bind("ws-1")
    .fetch_one(&pool)
    .await
    .expect("count unread after mark");
    assert_eq!(unread_after, 0, "stamping read_at clears the unread count");

    // The CHECK constraint rejects an unknown kind (a malformed writer can never
    // land a foreign family).
    let bad = sqlx::query(
        "INSERT INTO inbox_entry \
         (id, workspace_id, kind, event, subject_id, summary, created_at) \
         VALUES ('ie-bad', ?, 'nonsense', 'x', 's', 'm', 1)",
    )
    .bind("ws-1")
    .execute(&pool)
    .await;
    assert!(
        bad.is_err(),
        "an unknown kind must violate the CHECK constraint"
    );

    // 4. Double-apply is idempotent: nothing re-runs, the row we inserted stays.
    let row = sqlx::query("SELECT read_at FROM inbox_entry WHERE id = ?")
        .bind("ie-1")
        .fetch_one(&pool)
        .await
        .expect("read inbox row");
    let read_at_before: Option<i64> = row.get("read_at");
    apply_migrations(&pool).await.expect("second apply is a no-op");
    let row = sqlx::query("SELECT read_at FROM inbox_entry WHERE id = ?")
        .bind("ie-1")
        .fetch_one(&pool)
        .await
        .expect("read inbox row after re-apply");
    assert_eq!(
        row.get::<Option<i64>, _>("read_at"),
        read_at_before,
        "double-apply must not change any row"
    );

    pool.close().await;
}
