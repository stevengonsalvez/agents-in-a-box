//! Upgrade-from-populated test for migration 0085 (per-session model state
//! group: `model`, `reasoning_effort`, `model_updated_at`, `model_authority` on
//! `fleet_session`).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database, the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0083, the branch predecessor),
//! 2. seed a live fleet session with its event log,
//! 3. apply the embedded migrations (which adds 0085),
//! 4. assert every seeded row SURVIVES unchanged, the four new columns read back
//!    NULL / NULL / 0 / 'inferred' — "never observed", not "the default model" —
//!    and a double-apply is a no-op.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test
/// (`0085_fleet_session_model.sql`).
const NEW_MIGRATION_VERSION: i64 = 85;

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
    // Assert the prior set is non-empty, NOT that version-1 exists. Migration
    // numbers legitimately carry gaps: this file was renumbered 0084 -> 0085
    // because a concurrent branch already owned 0084, so on this branch the
    // predecessor is 0083 and version 84 does not exist until that branch lands.
    // Pinning `version - 1` makes the test a hostage to whichever migration
    // happens to merge next.
    assert!(
        !migrator.migrations.is_empty(),
        "the prior-migration set must not be empty, {NEW_MIGRATION_VERSION} alters an existing schema"
    );
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// Pre-existing roster content an upgrading install carries: one live session
/// with a populated attention group and one durable event against it.
async fn seed_populated(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO fleet_session \
         (session_key, provider, provider_session_id, cwd, display_name, lifecycle_state, \
          attention_state, management_state, transport_health, capabilities, provenance, \
          confidence, discovered_at, last_observed_at, metadata_updated_at, metadata_authority, \
          lifecycle_updated_at, lifecycle_authority, attention_updated_at, attention_authority, \
          transport_updated_at, transport_authority, version, updated_revision) \
         VALUES ('claude:sess-1', 'claude', 'sess-1', '/repo', 'feat/roster', 'RUNNING', \
                 'NONE', 'MANAGED', 'HEALTHY', '{}', 'authoritative', 'HIGH', \
                 100, 200, 200, 'authoritative', 150, 'authoritative', 120, 'authoritative', \
                 110, 'authoritative', 7, 42)",
    )
    .execute(pool)
    .await
    .expect("seed fleet session");
    sqlx::query(
        "INSERT INTO fleet_event \
         (event_id, session_key, observed_at, authority, event_type, payload, session_version, applied) \
         VALUES ('evt-1', 'claude:sess-1', 200, 'authoritative', 'Stop', '{}', 7, 1)",
    )
    .execute(pool)
    .await
    .expect("seed fleet event");
}

/// Migration 0085 on a populated DB: the session survives, and the model group
/// arrives in its "never observed" shape.
#[tokio::test]
async fn migration_0085_adds_unobserved_model_group_to_a_populated_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    apply_migrations(&pool).await.expect("upgrade applies 0085");

    let row = sqlx::query(
        "SELECT provider, provider_session_id, cwd, display_name, lifecycle_state, \
                attention_state, management_state, version, updated_revision, \
                model, reasoning_effort, model_updated_at, model_authority \
         FROM fleet_session WHERE session_key = 'claude:sess-1'",
    )
    .fetch_one(&pool)
    .await
    .expect("session survives the upgrade");

    // (a) Every seeded column is untouched by a column add.
    assert_eq!(
        row.try_get::<String, _>("provider").expect("provider"),
        "claude"
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("provider_session_id")
            .expect("provider_session_id"),
        Some("sess-1".to_string())
    );
    assert_eq!(row.try_get::<String, _>("cwd").expect("cwd"), "/repo");
    assert_eq!(
        row.try_get::<Option<String>, _>("display_name").expect("display_name"),
        Some("feat/roster".to_string())
    );
    assert_eq!(
        row.try_get::<String, _>("lifecycle_state").expect("lifecycle_state"),
        "RUNNING"
    );
    assert_eq!(row.try_get::<i64, _>("version").expect("version"), 7);
    assert_eq!(
        row.try_get::<i64, _>("updated_revision").expect("updated_revision"),
        42
    );

    // (b) NULL means NEVER OBSERVED, not "the default model". Nothing is
    //     backfilled, and the group's clock/authority start at their weakest
    //     values so the first real observation always wins `should_replace`.
    for column in ["model", "reasoning_effort"] {
        assert_eq!(
            row.try_get::<Option<String>, _>(column).expect("model column present"),
            None,
            "{column} must arrive NULL, meaning never observed"
        );
    }
    assert_eq!(
        row.try_get::<i64, _>("model_updated_at").expect("model_updated_at"),
        0,
        "the model group clock must start at 0, not at the row's last_observed_at"
    );
    assert_eq!(
        row.try_get::<String, _>("model_authority").expect("model_authority"),
        "inferred",
        "the weakest authority, so any later observation can replace it"
    );

    // (c) The event log the session owns is untouched.
    let (event_count, applied): (i64, i64) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM fleet_event")
            .fetch_one(&pool)
            .await
            .expect("count events"),
        sqlx::query_scalar("SELECT applied FROM fleet_event WHERE event_id = 'evt-1'")
            .fetch_one(&pool)
            .await
            .expect("read event"),
    );
    assert_eq!(event_count, 1);
    assert_eq!(applied, 1);

    // (d) 0085 is recorded exactly once, and a double-apply is a no-op.
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read 0085 record");
    assert_eq!(recorded, 1, "0085 recorded as applied exactly once");
    apply_migrations(&pool).await.expect("second apply is a no-op");

    pool.close().await;
}

/// The effort vocabulary is deliberately NOT constrained in the schema: it is
/// the provider's, differs between providers, and is validated at the write
/// path. A provider-specific token must store without a CHECK failure.
#[tokio::test]
async fn migration_0085_does_not_constrain_the_effort_vocabulary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("upgrade applies 0085");

    sqlx::query(
        "UPDATE fleet_session SET model = ?, reasoning_effort = ?, \
             model_updated_at = 300, model_authority = 'authoritative' \
         WHERE session_key = 'claude:sess-1'",
    )
    .bind("gpt-5.6-terra")
    .bind("xhigh")
    .execute(&pool)
    .await
    .expect("a provider-specific effort token stores without a CHECK");

    let stored: (Option<String>, Option<String>, i64, String) = sqlx::query_as(
        "SELECT model, reasoning_effort, model_updated_at, model_authority \
         FROM fleet_session WHERE session_key = 'claude:sess-1'",
    )
    .fetch_one(&pool)
    .await
    .expect("read back the model group");
    assert_eq!(
        stored,
        (
            Some("gpt-5.6-terra".to_string()),
            Some("xhigh".to_string()),
            300,
            "authoritative".to_string()
        )
    );

    pool.close().await;
}
