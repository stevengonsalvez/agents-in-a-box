//! Upgrade-from-populated test for migration 0023 (five-status issue lifecycle
//! vocabulary, 63l.3).
//!
//! Migration 0023 remaps the legacy issue `state` vocabulary forward IN PLACE so
//! every existing row lands in a real canonical column: `open -> todo` and
//! `closed -> done`. It touches no schema (a pure pair of `UPDATE`s on the small
//! `issue` table) and no workspace row (the HGR default issue id lives at the
//! display layer, not the stored `issue_prefix` column), so every pre-existing
//! row except the legacy-state issues survives byte-identical.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0022),
//! 2. seed a live workspace + member + several issues spanning the legacy
//!    (`open`, `closed`) and canonical (`in_progress`) vocabularies, plus a
//!    workspace that has explicitly configured an `issue_prefix`,
//! 3. apply the embedded migrations (which adds exactly 0023),
//! 4. assert the legacy states are remapped forward, the canonical / unknown
//!    states are untouched, the workspace prefixes are untouched, and a second
//!    apply is a no-op (idempotent).

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0023_issue_lifecycle_vocab.sql`).
const NEW_MIGRATION_VERSION: i64 = 23;

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

/// Seed two workspaces + an owner + several issues spanning the legacy
/// (`open` / `closed`) and canonical (`in_progress` / unknown) state vocabularies.
/// `ws-2` carries an explicitly-configured `issue_prefix` to prove 0023 leaves
/// the prefix column untouched.
async fn seed_populated(pool: &SqlitePool) {
    for (id, prefix) in [("ws-1", None::<&str>), ("ws-2", Some("[OPS] "))] {
        sqlx::query(
            "INSERT INTO workspace (id, slug, name, created_at, issue_prefix) \
             VALUES (?, ?, ?, 0, ?)",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .bind(prefix)
        .execute(pool)
        .await
        .expect("insert workspace");
    }
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('user-1', 'a@x.com', 0)")
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','user-1','owner')",
    )
    .execute(pool)
    .await
    .expect("insert member");

    // Issues spanning the legacy + canonical + unknown vocabularies.
    for (id, ws, state) in [
        ("i-open", "ws-1", "open"),          // legacy -> todo
        ("i-closed", "ws-1", "closed"),      // legacy -> done
        ("i-inprog", "ws-1", "in_progress"), // canonical, untouched
        ("i-weird", "ws-1", "triaging"),     // unknown, left as-is
        ("i-open2", "ws-2", "open"),         // legacy in a second workspace
    ] {
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, ?, 'member', 'user-1', 0)",
        )
        .bind(id)
        .bind(ws)
        .bind(id)
        .bind(state)
        .execute(pool)
        .await
        .expect("insert issue");
    }
}

/// Read one issue's `state`.
async fn state_of(pool: &SqlitePool, id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM issue WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read issue state")
}

#[tokio::test]
async fn migration_0023_remaps_legacy_states_on_a_populated_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Before: the legacy states are still the pre-redesign tokens.
    assert_eq!(state_of(&pool, "i-open").await, "open");
    assert_eq!(state_of(&pool, "i-closed").await, "closed");

    // Head-count BEFORE: the prior set tops out at 0022 (0023 not yet applied).
    let head_before: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read head before");
    assert_eq!(
        head_before,
        NEW_MIGRATION_VERSION - 1,
        "head is 0022 before upgrade"
    );

    // Upgrade: the embedded migrator skips 0001..0022 (already recorded) and
    // applies exactly 0023.
    apply_migrations(&pool).await.expect("upgrade applies 0023");

    // Head-count AFTER (assert #1): the recorded head is now 0023 — the ACTUAL
    // value read from the embedded migration set, not a guess.
    let head_after: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read head after");
    assert_eq!(head_after, NEW_MIGRATION_VERSION, "head is migration 0023");

    // Head-count AFTER (assert #2): 0023 is recorded exactly once.
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read 0023 record");
    assert_eq!(recorded, 1, "0023 recorded as applied exactly once");

    // 1. Legacy states are remapped forward, in every workspace.
    assert_eq!(state_of(&pool, "i-open").await, "todo", "open -> todo");
    assert_eq!(state_of(&pool, "i-closed").await, "done", "closed -> done");
    assert_eq!(
        state_of(&pool, "i-open2").await,
        "todo",
        "open -> todo (ws-2)"
    );

    // 2. Canonical + unknown states are LEFT AS-IS (no spurious rewrite).
    assert_eq!(
        state_of(&pool, "i-inprog").await,
        "in_progress",
        "canonical in_progress untouched"
    );
    assert_eq!(
        state_of(&pool, "i-weird").await,
        "triaging",
        "unknown state left as-is (display layer tolerates it)"
    );

    // 3. The workspace prefixes are UNTOUCHED — 0023 does not backfill HGR (the
    //    default is a display-layer concern). ws-1 stays NULL, ws-2 keeps its
    //    explicit prefix.
    let p1: Option<String> =
        sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id='ws-1'")
            .fetch_one(&pool)
            .await
            .expect("read ws-1 prefix");
    let p2: Option<String> =
        sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id='ws-2'")
            .fetch_one(&pool)
            .await
            .expect("read ws-2 prefix");
    assert_eq!(
        p1, None,
        "fresh workspace prefix stays NULL (HGR is display-only)"
    );
    assert_eq!(p2.as_deref(), Some("[OPS] "), "explicit prefix preserved");

    // 4. Double-apply is idempotent: a second apply rewrites nothing.
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        state_of(&pool, "i-open").await,
        "todo",
        "idempotent: still todo"
    );
    assert_eq!(
        state_of(&pool, "i-inprog").await,
        "in_progress",
        "idempotent"
    );
    let head_again: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read head after re-apply");
    assert_eq!(
        head_again, NEW_MIGRATION_VERSION,
        "double-apply records no new version"
    );

    pool.close().await;
}
