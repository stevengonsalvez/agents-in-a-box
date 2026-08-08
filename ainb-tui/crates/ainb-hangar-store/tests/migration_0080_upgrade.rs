//! Upgrade-from-populated test for migration 0080 (per-session ACP config:
//! `model`, `reasoning_effort`, `persona` on `fleet_acp_session`).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database, the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0079),
//! 2. seed a live ACP session with its chat messages and delivery legs,
//! 3. apply the embedded migrations (which adds 0080),
//! 4. assert every seeded row SURVIVES with a NULL (no-override) config, the
//!    pinned `permission_mode` is untouched and still NOT NULL, the persona
//!    bound BITES in octets, and a double-apply is a no-op.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0080_fleet_acp_session_config.sql`).
const NEW_MIGRATION_VERSION: i64 = 80;

/// The persona octet bound the migration's CHECK enforces, mirroring
/// `ainb_hangar_proto::fleet::FLEET_COPILOT_PERSONA_MAX`.
const PERSONA_MAX: usize = 8 * 1024;

/// Open a fresh on-disk WAL pool in `dir` and apply only the migrations PRIOR
/// to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install
/// runs before upgrading.
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
        migrator.migrations.iter().any(|m| m.version == NEW_MIGRATION_VERSION - 1),
        "the prior-migration set must reach 0079, the schema 0080 alters"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// Pre-existing chat-bus content an upgrading install carries: one live ACP
/// session under a pinned permission mode, one message, one delivered leg.
async fn seed_populated(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO fleet_acp_session \
         (session_key, scope_key, provider, provider_version, acp_session_id, cwd, \
          permission_mode, state, created_at, last_active_at) \
         VALUES ('acp:01J0KEY', 'channel:copilot', 'claude-agent-acp', '0.64.0', \
                 'adapter-session-1', '/repo', 'default', 'ACTIVE', 100, 101)",
    )
    .execute(pool)
    .await
    .expect("seed acp session");
    sqlx::query(
        "INSERT INTO fleet_message (id, request_id, scope_key, sender, kind, body, created_at) \
         VALUES ('01J0MSG', 'req-1', 'channel:copilot', 'operator', 'user', 'what is blocked?', 102)",
    )
    .execute(pool)
    .await
    .expect("seed message");
    sqlx::query(
        "INSERT INTO fleet_message_delivery (message_id, session_key, state, resolved_at) \
         VALUES ('01J0MSG', 'acp:01J0KEY', 'DELIVERED', 103)",
    )
    .execute(pool)
    .await
    .expect("seed delivery leg");
}

/// Migration 0080 on a populated DB: the session survives, its config columns
/// arrive NULL (no override), and the pinned permission mode is untouched.
#[tokio::test]
async fn migration_0080_adds_null_config_to_a_populated_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    apply_migrations(&pool).await.expect("upgrade applies 0080");

    // (a) The pre-existing session survives whole, and the new columns default
    //     to NULL: "no override, use the daemon's static config", so an
    //     upgraded install behaves exactly as it did before.
    let row = sqlx::query(
        "SELECT permission_mode, provider, provider_version, acp_session_id, state, \
                model, reasoning_effort, persona \
         FROM fleet_acp_session WHERE session_key = 'acp:01J0KEY'",
    )
    .fetch_one(&pool)
    .await
    .expect("session survives the upgrade");
    assert_eq!(
        row.try_get::<String, _>("permission_mode").expect("permission_mode"),
        "default",
        "the PINNED permission mode must survive untouched; it is not overridable"
    );
    assert_eq!(
        row.try_get::<String, _>("provider").expect("provider"),
        "claude-agent-acp"
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("provider_version").expect("provider_version"),
        Some("0.64.0".to_string())
    );
    assert_eq!(row.try_get::<String, _>("state").expect("state"), "ACTIVE");
    for column in ["model", "reasoning_effort", "persona"] {
        assert_eq!(
            row.try_get::<Option<String>, _>(column).expect("config column present"),
            None,
            "{column} must arrive NULL, meaning no per-session override"
        );
    }

    // (b) The chat rows the session owns are untouched by a column add.
    let (message_count, delivery_state): (i64, String) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
            .fetch_one(&pool)
            .await
            .expect("count messages"),
        sqlx::query_scalar("SELECT state FROM fleet_message_delivery WHERE message_id = '01J0MSG'")
            .fetch_one(&pool)
            .await
            .expect("read leg"),
    );
    assert_eq!(message_count, 1);
    assert_eq!(delivery_state, "DELIVERED");

    // (c) `permission_mode` is still NOT NULL, i.e. 0080 did not rebuild the
    //     table and quietly relax part 1's invariant.
    let mode_notnull: i64 = sqlx::query_scalar(
        "SELECT \"notnull\" FROM pragma_table_info('fleet_acp_session') WHERE name = 'permission_mode'",
    )
    .fetch_one(&pool)
    .await
    .expect("read permission_mode column info");
    assert_eq!(mode_notnull, 1, "permission_mode must remain NOT NULL");

    // (d) 0080 is recorded exactly once, and a double-apply is a no-op.
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read 0080 record");
    assert_eq!(recorded, 1, "0080 recorded as applied exactly once");
    apply_migrations(&pool).await.expect("second apply is a no-op");

    pool.close().await;
}

/// After the upgrade the config columns accept overrides and the persona bound
/// BITES, counted in octets so a multibyte persona cannot slip past it.
#[tokio::test]
async fn migration_0080_persona_bound_is_enforced_in_octets_after_upgrade() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("upgrade applies 0080");

    let set_persona = |persona: String| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "UPDATE fleet_acp_session \
                 SET model = 'claude-sonnet-4-5', reasoning_effort = 'medium', persona = ? \
                 WHERE session_key = 'acp:01J0KEY'",
            )
            .bind(persona)
            .execute(&pool)
            .await
            .map(|_| ())
        }
    };

    set_persona("a".repeat(PERSONA_MAX))
        .await
        .expect("a persona at the bound is stored");
    let stored: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT model, reasoning_effort FROM fleet_acp_session WHERE session_key = 'acp:01J0KEY'",
    )
    .fetch_one(&pool)
    .await
    .expect("read back the override");
    assert_eq!(
        stored,
        (
            Some("claude-sonnet-4-5".to_string()),
            Some("medium".to_string())
        )
    );

    let err = set_persona("a".repeat(PERSONA_MAX + 1))
        .await
        .expect_err("one octet past the bound must be refused");
    assert!(err.to_string().contains("CHECK"), "got: {err}");

    // The multibyte trap: 4096 three-byte characters is 12 KiB of storage but
    // only 4096 by SQLite's character-counting `length()`. Octet-counted, it
    // is over the bound and refused.
    let err = set_persona("あ".repeat(PERSONA_MAX / 2))
        .await
        .expect_err("a multibyte persona past the OCTET bound must be refused");
    assert!(err.to_string().contains("CHECK"), "got: {err}");

    pool.close().await;
}
