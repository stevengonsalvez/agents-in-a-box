//! Upgrade-from-populated test for migration 0079 (chat bus: `fleet_message`,
//! `fleet_message_delivery`, `fleet_acp_session`, and the re-key of
//! `idx_fleet_provider_event_projection`).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database, the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0078),
//! 2. seed `fleet_provider_event` with pre-existing projection-source rows,
//!    including a pending-recovery row (`projection_revision IS NULL`),
//! 3. apply the embedded migrations (which adds 0079),
//! 4. assert every seeded row SURVIVES, the projection index was dropped and
//!    recreated with the `(source, provider, projection_revision)` key and the
//!    UNCHANGED `WHERE projection_revision IS NULL` predicate, the real
//!    consumer query still plans onto it with ACP rows present, the three new
//!    tables enforce their constraints, and a double-apply is a no-op.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test (`0079_chat_bus.sql`).
const NEW_MIGRATION_VERSION: i64 = 79;

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
        !migrator.migrations.is_empty(),
        "prior-migration set must not be empty"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// Insert one provider-event row at the pre-0079 schema (identical columns;
/// only the projection index changes in 0079). `raw_blake3` must be 64 chars
/// per the 0071 CHECK; a fixed digest is fine for schema tests.
async fn insert_provider_event(pool: &SqlitePool, event_id: &str, provider: &str, source: &str) {
    sqlx::query(
        "INSERT INTO fleet_provider_event \
         (event_id, provider, source, session_key, provider_session_id, observed_at, \
          received_at, event_type, raw_payload, raw_blake3) \
         VALUES (?, ?, ?, ?, NULL, 100, 101, 'evt', '{}', ?)",
    )
    .bind(event_id)
    .bind(provider)
    .bind(source)
    .bind(format!("{provider}:session-1"))
    .bind("0".repeat(64))
    .execute(pool)
    .await
    .expect("insert provider event");
}

/// Pre-existing ledger content an upgrading install carries: pending recovery
/// work for the Codex manager plus an already-noted claude hook row.
async fn seed_populated(pool: &SqlitePool) {
    insert_provider_event(pool, "codex-pending", "codex", "codex_app_server").await;
    insert_provider_event(pool, "claude-hook", "claude", "claude_hook").await;
}

/// Read the CREATE INDEX sql of one index from `sqlite_master`.
async fn index_sql(pool: &SqlitePool, name: &str) -> String {
    sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("index present")
}

/// Migration 0079 on a populated DB: the ledger survives, the projection index
/// is re-keyed with its predicate intact, and the consumer query still uses it.
#[tokio::test]
async fn migration_0079_rekeys_the_projection_index_on_a_populated_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    apply_migrations(&pool).await.expect("upgrade applies 0079");

    // (a) Every pre-existing ledger row survives, pending marker intact.
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT event_id, projection_revision FROM fleet_provider_event ORDER BY ingest_order",
    )
    .fetch_all(&pool)
    .await
    .expect("read ledger");
    assert_eq!(
        rows,
        vec![
            ("codex-pending".to_string(), None),
            ("claude-hook".to_string(), None),
        ],
        "the index swap must not touch a single ledger row"
    );

    // (b) The index was RECREATED: key becomes (source, provider,
    //     projection_revision), predicate stays `projection_revision IS NULL`.
    let sql = index_sql(&pool, "idx_fleet_provider_event_projection").await;
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("(source, provider, projection_revision)"),
        "index key must lead with source to push acp rows aside, got: {sql}"
    );
    assert!(
        flat.contains("WHERE projection_revision IS NULL"),
        "the pending-recovery predicate must be UNCHANGED, got: {sql}"
    );

    // (c) With ACP transcript rows present, the REAL consumer query
    //     (repo `unprojected`) still plans onto the partial index.
    insert_provider_event(&pool, "acp-1", "claude-agent-acp", "acp").await;
    insert_provider_event(&pool, "acp-2", "claude-agent-acp", "acp").await;
    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN \
         SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
         FROM fleet_provider_event WHERE provider = ? AND source = ? AND projection_revision IS NULL \
         ORDER BY ingest_order ASC LIMIT ?",
    )
    .bind("codex")
    .bind("codex_app_server")
    .bind(10_i64)
    .fetch_all(&pool)
    .await
    .expect("explain consumer query");
    let detail = plan
        .iter()
        .map(|row| row.try_get::<String, _>("detail").expect("detail column"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        detail.contains("idx_fleet_provider_event_projection"),
        "recovery scan must use the recreated index, plan was:\n{detail}"
    );

    // (d) 0079 is recorded exactly once, and a double-apply is a no-op.
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read 0079 record");
    assert_eq!(recorded, 1, "0079 recorded as applied exactly once");
    apply_migrations(&pool).await.expect("second apply is a no-op");

    pool.close().await;
}

/// After the upgrade the new tables exist and their constraints BITE.
#[tokio::test]
async fn migration_0079_chat_bus_constraints_are_enforced_after_upgrade() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("upgrade applies 0079");

    // fleet_message: a valid row inserts; an unknown kind is refused.
    sqlx::query(
        "INSERT INTO fleet_message (id, scope_key, sender, kind, body, created_at) \
         VALUES ('msg-1', 'session:s-1', 'operator', 'user', 'hello', 0)",
    )
    .execute(&pool)
    .await
    .expect("valid message inserts");
    let err = sqlx::query(
        "INSERT INTO fleet_message (id, scope_key, sender, kind, body, created_at) \
         VALUES ('msg-2', 'session:s-1', 'operator', 'thought', 'x', 0)",
    )
    .execute(&pool)
    .await
    .expect_err("an unknown kind must be refused");
    assert!(err.to_string().contains("CHECK"), "got: {err}");

    // fleet_message_delivery: PK is (message_id, session_key); state checked.
    sqlx::query(
        "INSERT INTO fleet_message_delivery (message_id, session_key, state) \
         VALUES ('msg-1', 'sess-1', 'PENDING')",
    )
    .execute(&pool)
    .await
    .expect("valid delivery inserts");
    let err = sqlx::query(
        "INSERT INTO fleet_message_delivery (message_id, session_key, state) \
         VALUES ('msg-1', 'sess-1', 'PENDING')",
    )
    .execute(&pool)
    .await
    .expect_err("a duplicate (message, recipient) leg must be refused");
    assert!(
        err.as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation),
        "got: {err}"
    );

    // fleet_acp_session: one LIVE session per scope, enforced by the partial
    // unique index; a non-live state under the same scope is fine.
    let insert_session = |key: &'static str, state: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO fleet_acp_session \
                 (session_key, scope_key, provider, cwd, permission_mode, state, created_at, last_active_at) \
                 VALUES (?, 'session:one', 'claude-agent-acp', '/tmp', 'default', ?, 0, 0)",
            )
            .bind(key)
            .bind(state)
            .execute(&pool)
            .await
            .map(|_| ())
        }
    };
    insert_session("acp:1", "ACTIVE").await.expect("first live session");
    let err = insert_session("acp:2", "IDLE")
        .await
        .expect_err("a second LIVE session for the scope must collide");
    assert!(
        err.as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation),
        "got: {err}"
    );
    insert_session("acp:3", "DEAD")
        .await
        .expect("a non-live state stays out of the partial index");

    pool.close().await;
}
