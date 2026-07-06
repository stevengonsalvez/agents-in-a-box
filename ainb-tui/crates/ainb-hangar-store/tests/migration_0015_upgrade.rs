//! Upgrade-from-populated test for migration 0015 (agent archive flag + runtime
//! config knobs).
//!
//! Migration 0015 adds six columns to `agent`:
//!   - `archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))`,
//!   - `model TEXT` (nullable provider model override),
//!   - `cli_args TEXT NOT NULL DEFAULT '[]'` (JSON array of CLI args),
//!   - `mcp_config TEXT NOT NULL DEFAULT '{}'` (JSON object MCP config),
//!   - `thinking TEXT` (nullable reasoning level),
//!   - `agent_env TEXT NOT NULL DEFAULT '{}'` (JSON object env map).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0014),
//! 2. seed a live agent row (the row the new columns must leave intact),
//! 3. apply the embedded migrations (which adds exactly 0015),
//! 4. assert the pre-existing row survives intact, reads back the column
//!    defaults (archived 0, model NULL, `cli_args` `'[]'`, `mcp_config` `'{}'`,
//!    thinking NULL, `agent_env` `'{}'`), the new columns accept explicit writes,
//!    and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0015_agent_archive_and_config.sql`).
const NEW_MIGRATION_VERSION: i64 = 15;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only the migrations
/// PRIOR to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing
/// install runs before upgrading. Uses the same on-disk `migrations/` directory
/// the embedded migrator is compiled from, so the prior set can never drift
/// from what `apply_migrations` would itself apply.
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

/// Seed the FK graph (one workspace, one user, one runtime) plus a live agent —
/// the row whose survival across the column additions the test proves.
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
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(pool)
    .await
    .expect("insert agent_runtime");

    // One agent, seeded WITHOUT the new columns (they don't exist yet at the
    // prior schema). After upgrade it must read back the column defaults.
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("Builder")
    .bind("rt-1")
    .bind("Build carefully.")
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("seed agent");
}

/// Snapshot the pre-existing agent identity columns, ordered by id, as
/// `(id, name, runtime_id)` rows — the data the column additions must not touch.
async fn agent_identity_snapshot(pool: &SqlitePool) -> Vec<(String, String, String)> {
    sqlx::query("SELECT id, name, runtime_id FROM agent ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("snapshot agents")
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("id"),
                r.get::<String, _>("name"),
                r.get::<String, _>("runtime_id"),
            )
        })
        .collect()
}

/// Assert the agent `id` reads back the migration-0015 column DEFAULTS: archived
/// 0, model NULL, `cli_args` `'[]'`, `mcp_config` `'{}'`, thinking NULL,
/// `agent_env` `'{}'`.
async fn assert_default_config(pool: &SqlitePool, id: &str) {
    let row = sqlx::query(
        "SELECT archived, model, cli_args, mcp_config, thinking, agent_env \
         FROM agent WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("read upgraded agent");
    assert_eq!(row.get::<i64, _>("archived"), 0, "default archived is 0");
    assert_eq!(row.get::<Option<String>, _>("model"), None, "model NULL");
    assert_eq!(row.get::<String, _>("cli_args"), "[]", "cli_args '[]'");
    assert_eq!(
        row.get::<String, _>("mcp_config"),
        "{}",
        "mcp_config '{{}}'"
    );
    assert_eq!(
        row.get::<Option<String>, _>("thinking"),
        None,
        "thinking NULL"
    );
    assert_eq!(row.get::<String, _>("agent_env"), "{}", "agent_env '{{}}'");
}

/// Write every new column on agent `id` (archive + model + thinking + JSON
/// args/mcp/env) and assert each reads back verbatim — proving the columns are
/// writable post-upgrade.
async fn assert_explicit_writes(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "UPDATE agent SET archived = ?, model = ?, cli_args = ?, mcp_config = ?, \
         thinking = ?, agent_env = ? WHERE id = ?",
    )
    .bind(1_i64)
    .bind("claude-opus-4")
    .bind(r#"["--verbose"]"#)
    .bind(r#"{"servers":{"fs":{}}}"#)
    .bind("high")
    .bind(r#"{"FOO":"bar"}"#)
    .bind(id)
    .execute(pool)
    .await
    .expect("write new columns");
    let row = sqlx::query(
        "SELECT archived, model, cli_args, mcp_config, thinking, agent_env \
         FROM agent WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("read written agent");
    assert_eq!(row.get::<i64, _>("archived"), 1, "archived persists");
    assert_eq!(
        row.get::<Option<String>, _>("model"),
        Some("claude-opus-4".to_string()),
        "model persists"
    );
    assert_eq!(
        row.get::<String, _>("cli_args"),
        r#"["--verbose"]"#,
        "cli_args JSON persists"
    );
    assert_eq!(
        row.get::<String, _>("mcp_config"),
        r#"{"servers":{"fs":{}}}"#,
        "mcp_config JSON persists"
    );
    assert_eq!(
        row.get::<Option<String>, _>("thinking"),
        Some("high".to_string()),
        "thinking persists"
    );
    assert_eq!(
        row.get::<String, _>("agent_env"),
        r#"{"FOO":"bar"}"#,
        "agent_env JSON persists"
    );
}

#[tokio::test]
async fn migration_0015_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = agent_identity_snapshot(&pool).await;
    assert_eq!(before.len(), 1, "seeded population: {before:?}");

    // Upgrade: the embedded migrator skips 0001..0014 (already recorded) and
    // applies 0015.
    apply_migrations(&pool).await.expect("upgrade applies 0015");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0015 recorded as applied");

    // 1. The pre-existing row survives intact.
    let after = agent_identity_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch agent rows");

    // 2. The pre-existing row reads back the new column DEFAULTS.
    assert_default_config(&pool, "agent-1").await;

    // 3. New semantics: the columns accept explicit writes (archive + model +
    //    thinking + JSON args/mcp/env), all of which read back verbatim.
    assert_explicit_writes(&pool, "agent-1").await;

    // 4. The archived CHECK rejects an out-of-range value (only 0 or 1 allowed).
    let bad = sqlx::query("UPDATE agent SET archived = 2 WHERE id = ?")
        .bind("agent-1")
        .execute(&pool)
        .await;
    assert!(
        bad.is_err(),
        "archived CHECK must reject a value outside 0/1"
    );

    // 5. Double-apply is idempotent: nothing re-runs, nothing changes.
    let snapshot = agent_identity_snapshot(&pool).await;
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        agent_identity_snapshot(&pool).await,
        snapshot,
        "double-apply must not change any row"
    );

    pool.close().await;
}
