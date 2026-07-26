//! Behavioral test for migration 0049 (`blocked` + `cancelled` issue states,
//! multica gap #19).
//!
//! Migration 0049 is the FIRST constraint hangar has ever put on `issue.state`:
//! two `BEFORE INSERT` / `BEFORE UPDATE OF state` triggers that `RAISE(ABORT)`
//! on any token outside the seven canonical lifecycle statuses plus the two
//! legacy ones (`open` / `closed`, still written by the Beads bridge and the
//! CLI's `DEFAULT_ISSUE_STATE` — see D3 in the migration header).
//!
//! Run against a REAL on-disk sqlite with the FULL embedded chain applied, this
//! pins:
//!
//! 1. `blocked` and `cancelled` INSERT and read back (the new vocabulary really
//!    persists — the storage half of "an issue can move to blocked/cancelled"),
//! 2. an UPDATE into `cancelled` persists,
//! 3. a typo'd INSERT is REJECTED with a message naming the column,
//! 4. a typo'd UPDATE is rejected AND leaves the stored state unchanged (no
//!    partial write),
//! 5. the legacy `open` still inserts (D3 tolerance is deliberate, not an
//!    oversight — pin it so a future "tightening" is a conscious change).

use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0049_issue_state_blocked_cancelled.sql`).
const MIGRATION_VERSION: i64 = 49;

/// Open a fresh on-disk WAL pool in `dir` with the FULL embedded migration chain
/// applied (the schema a current install runs).
async fn pool_at_head(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");
    apply_migrations(&pool).await.expect("migrations apply");
    pool
}

/// Seed the workspace + creator every issue row needs.
async fn seed_workspace(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','ws-1','ws-1',0)",
    )
    .execute(pool)
    .await
    .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('user-1','a@x.com',0)")
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','user-1','owner')",
    )
    .execute(pool)
    .await
    .expect("insert member");
}

/// Attempt to insert one issue with `state`; returns the store result so a test
/// can assert on either success or the trigger's ABORT.
async fn try_insert(pool: &SqlitePool, id: &str, state: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, 'ws-1', ?, ?, 'member', 'user-1', 0)",
    )
    .bind(id)
    .bind(id)
    .bind(state)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Attempt to move one issue to `state`.
async fn try_update(pool: &SqlitePool, id: &str, state: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE issue SET state = ? WHERE id = ?")
        .bind(state)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Read one issue's stored `state`.
async fn state_of(pool: &SqlitePool, id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM issue WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read issue state")
}

/// 0049 is recorded exactly once by the embedded chain (so the file really is
/// part of the applied set, not an orphan on disk).
#[tokio::test]
async fn migration_0049_is_applied_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_head(dir.path()).await;

    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read 0049 record");
    assert_eq!(recorded, 1, "0049 recorded as applied exactly once");
}

/// The two NEW canonical tokens insert, read back verbatim, and an UPDATE into
/// `cancelled` persists — the storage half of "move to blocked / cancelled".
#[tokio::test]
async fn blocked_and_cancelled_insert_and_update_and_persist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_head(dir.path()).await;
    seed_workspace(&pool).await;

    try_insert(&pool, "i-blocked", "blocked").await.expect("insert blocked");
    assert_eq!(state_of(&pool, "i-blocked").await, "blocked");

    try_insert(&pool, "i-cancelled", "cancelled").await.expect("insert cancelled");
    assert_eq!(state_of(&pool, "i-cancelled").await, "cancelled");

    // Move an ordinary issue into cancelled (the UPDATE path the RPC drives).
    try_insert(&pool, "i-todo", "todo").await.expect("insert todo");
    try_update(&pool, "i-todo", "cancelled").await.expect("update to cancelled");
    assert_eq!(
        state_of(&pool, "i-todo").await,
        "cancelled",
        "the UPDATE persisted, it did not just report success"
    );

    // And back out of a terminal state — nothing here is one-way at the store.
    try_update(&pool, "i-todo", "blocked").await.expect("update to blocked");
    assert_eq!(state_of(&pool, "i-todo").await, "blocked");
}

/// Every canonical token is accepted (so the trigger's list can never drift
/// behind `IssueLifecycle::ALL` without this going red).
#[tokio::test]
async fn every_canonical_token_is_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_head(dir.path()).await;
    seed_workspace(&pool).await;

    for token in ainb_hangar_proto::lifecycle::IssueLifecycle::ALL {
        let id = format!("i-{}", token.as_str());
        try_insert(&pool, &id, token.as_str())
            .await
            .unwrap_or_else(|e| panic!("canonical token {} must insert: {e}", token.as_str()));
        assert_eq!(state_of(&pool, &id).await, token.as_str());
    }
}

/// A typo'd INSERT is rejected by the trigger, with a message that names the
/// column so the failure is diagnosable.
#[tokio::test]
async fn out_of_vocabulary_insert_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_head(dir.path()).await;
    seed_workspace(&pool).await;

    for typo in ["blockd", "cancelled_", "done ", "Blocked", "nonsense"] {
        let err = try_insert(&pool, &format!("i-{typo}"), typo)
            .await
            .expect_err("out-of-vocabulary state must be rejected");
        assert!(
            err.to_string().contains("invalid issue.state"),
            "rejection names the column, got: {err}"
        );
    }

    // Nothing was written by any of the rejected inserts.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(&pool)
        .await
        .expect("count issues");
    assert_eq!(count, 0, "a rejected insert writes no row");
}

/// A typo'd UPDATE is rejected AND leaves the stored state untouched.
#[tokio::test]
async fn out_of_vocabulary_update_leaves_the_row_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_head(dir.path()).await;
    seed_workspace(&pool).await;
    try_insert(&pool, "i-1", "cancelled").await.expect("insert cancelled");

    let err = try_update(&pool, "i-1", "nonsense")
        .await
        .expect_err("out-of-vocabulary update must be rejected");
    assert!(
        err.to_string().contains("invalid issue.state"),
        "rejection names the column, got: {err}"
    );
    assert_eq!(
        state_of(&pool, "i-1").await,
        "cancelled",
        "the rejected update must not have partially written"
    );
}

/// The legacy vocabulary is STILL admitted (D3): the Beads bridge and the CLI's
/// `DEFAULT_ISSUE_STATE` still write `open`, and `IssueLifecycle::for_state`
/// maps both tokens forward for display. Tightening to the seven canonical
/// tokens is a deliberate follow-up, so pin the tolerance.
#[tokio::test]
async fn legacy_open_and_closed_are_still_admitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_head(dir.path()).await;
    seed_workspace(&pool).await;

    try_insert(&pool, "i-open", "open").await.expect("legacy open still inserts");
    assert_eq!(state_of(&pool, "i-open").await, "open");

    try_update(&pool, "i-open", "closed")
        .await
        .expect("legacy closed still updates");
    assert_eq!(state_of(&pool, "i-open").await, "closed");
}
