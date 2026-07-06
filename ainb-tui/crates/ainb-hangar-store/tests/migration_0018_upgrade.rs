//! Upgrade-from-populated test for migration 0018 (webhook-triggered autopilots).
//!
//! Migration 0018 adds three columns to `autopilot` (`webhook_enabled`,
//! `webhook_secret_sha256`, `webhook_event_filter`) and one table
//! (`autopilot_webhook_delivery`, the delivery audit log) without touching any
//! existing row.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0017),
//! 2. seed a live workspace + member + agent + autopilot (the FK graph the new
//!    columns/table reference),
//! 3. apply the embedded migrations (which adds exactly 0018),
//! 4. assert the pre-existing rows survive intact, the new autopilot columns
//!    read their webhook-OFF defaults, the new `autopilot_webhook_delivery` table
//!    accepts writes and enforces its `outcome` CHECK, and a second apply is a
//!    no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test (`0018_autopilot_webhook.sql`).
const NEW_MIGRATION_VERSION: i64 = 18;

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

/// Seed the FK graph the new column/table reference: one workspace + user +
/// member + runtime + agent + one autopilot (the parent of a delivery). The rows
/// the additions must leave intact.
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
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("Worker")
    .bind("rt-1")
    .bind(None::<String>)
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, instructions, cron_expr, \
          max_concurrent_runs, next_tick_at, enabled, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?)",
    )
    .bind("ap-1")
    .bind("ws-1")
    .bind("agent-1")
    .bind("nightly")
    .bind(None::<String>)
    .bind("0 0 * * *")
    .bind(1_i64)
    .bind(None::<i64>)
    .bind(0_i64)
    .execute(pool)
    .await
    .expect("insert autopilot");
}

/// Snapshot the pre-existing entity identity, ordered, as the data the additions
/// must not touch.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in [
        "workspace",
        "user",
        "member",
        "agent_runtime",
        "agent",
        "autopilot",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count {t}: {e}"));
        out.push((t.to_string(), n));
    }
    out
}

#[tokio::test]
async fn migration_0018_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // Upgrade: the embedded migrator skips 0001..0017 (already recorded) and
    // applies 0018.
    apply_migrations(&pool).await.expect("upgrade applies 0018");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0018 recorded as applied");

    // 1. The pre-existing population survives intact — the additions touch no
    //    existing row.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The pre-existing autopilot reads the webhook-OFF column defaults: an
    //    upgrading install never accidentally has a firable webhook.
    let row = sqlx::query(
        "SELECT webhook_enabled, webhook_secret_sha256, webhook_event_filter \
         FROM autopilot WHERE id = ?",
    )
    .bind("ap-1")
    .fetch_one(&pool)
    .await
    .expect("read upgraded autopilot");
    assert_eq!(
        row.get::<i64, _>("webhook_enabled"),
        0,
        "webhook must default OFF after upgrade"
    );
    assert!(
        row.get::<Option<String>, _>("webhook_secret_sha256").is_none(),
        "no secret is set after upgrade"
    );
    assert!(
        row.get::<Option<String>, _>("webhook_event_filter").is_none(),
        "no event filter is set after upgrade"
    );

    // 3. The new delivery table accepts the three outcome kinds and rejects a
    //    junk outcome via its CHECK.
    let insert = |id: &'static str, outcome: &'static str, run_id: Option<&'static str>| {
        sqlx::query(
            "INSERT INTO autopilot_webhook_delivery \
             (id, autopilot_id, received_at, outcome, event, http_status, run_id, detail) \
             VALUES (?, 'ap-1', 10, ?, 'push', 200, ?, NULL)",
        )
        .bind(id)
        .bind(outcome)
        .bind(run_id)
        .execute(&pool)
    };
    insert("d-1", "fired", Some("run-1")).await.expect("a fired delivery writes");
    insert("d-2", "filtered", None).await.expect("a filtered delivery writes");
    insert("d-3", "rejected", None).await.expect("a rejected delivery writes");
    assert!(
        insert("d-4", "bogus", None).await.is_err(),
        "the outcome CHECK must reject a junk discriminant"
    );
    let deliveries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM autopilot_webhook_delivery WHERE autopilot_id = ?",
    )
    .bind("ap-1")
    .fetch_one(&pool)
    .await
    .expect("count deliveries");
    assert_eq!(deliveries, 3, "exactly three valid deliveries landed");

    // 4. Double-apply is idempotent: nothing re-runs, nothing changes.
    let snapshot = population_snapshot(&pool).await;
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(
        population_snapshot(&pool).await,
        snapshot,
        "double-apply must not change any row"
    );

    pool.close().await;
}
