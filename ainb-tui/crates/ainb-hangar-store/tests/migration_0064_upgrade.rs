//! Upgrade-from-populated test for migration 0064 (`autopilot_subscriber` +
//! `autopilot_collaborator` + `autopilot.access_mode` — multica parity #27).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration on a REAL populated database:
//!
//! 1. apply only the PRIOR migrations (0001..0063),
//! 2. seed a workspace + agent + autopilot + a rule version,
//! 3. apply the embedded migrations (which adds 0064),
//! 4. assert the seeded autopilot survives and reads `access_mode = 'open'`,
//!    both new tables and both indexes exist, NOTHING was backfilled into
//!    either table, and the `access_mode` CHECK actually bites.

use ainb_hangar_store::apply_migrations;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// `sqlx` version number of the migration under test
/// (`0064_autopilot_subscriber_collaborator.sql`).
const NEW_MIGRATION_VERSION: i64 = 64;

async fn pool_at_prior_schema(dir: &std::path::Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(dir.join("hangar.db"))
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
    assert!(!migrator.migrations.is_empty());
    migrator.run(&pool).await.expect("prior migrations apply");
    pool
}

/// The pre-0064 world: a workspace with an agent, an autopilot, and the v1 rule
/// version that names its accountable human.
async fn seed_populated(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('u-amy','amy@x.io',10)",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-amy','owner')",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','d-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1','ws-1','builder','rt-1','workspace','u-amy')",
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, instructions, cron_expr, max_concurrent_runs, \
          execution_mode, concurrency_policy, next_tick_at, enabled, api_trigger_enabled, \
          created_at) \
         VALUES ('ap-1','ws-1','ag-1','nightly','sweep','0 3 * * *',1,'run_only','skip', \
                 999999999999,1,0,30)",
        "INSERT INTO autopilot_rule_version \
         (id, autopilot_id, workspace_id, version, change_kind, published_by, config_summary, \
          created_at) \
         VALUES ('rv-1','ap-1','ws-1',1,'created','member:u-amy','{}',30)",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

async fn object_exists(pool: &SqlitePool, kind: &str, name: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_master WHERE type = ? AND name = ?")
        .bind(kind)
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("sqlite_master")
        > 0
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

#[tokio::test]
async fn migration_0064_adds_autopilot_actor_sets_and_keeps_the_rule_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    for t in ["autopilot_subscriber", "autopilot_collaborator"] {
        assert!(
            !object_exists(&pool, "table", t).await,
            "{t} must not exist before 0064"
        );
    }

    apply_migrations(&pool).await.expect("upgrade applies 0064");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0064 recorded as applied");

    // (a) The seeded autopilot survived, and reads the PERMISSIVE default —
    //     an upgrading install must never be silently locked out of its own
    //     rules (migration decision 4).
    let mode: String = sqlx::query_scalar("SELECT access_mode FROM autopilot WHERE id = 'ap-1'")
        .fetch_one(&pool)
        .await
        .expect("read access_mode");
    assert_eq!(mode, "open", "every pre-0064 autopilot upgrades to 'open'");

    // (b) Both tables and both indexes landed.
    for t in ["autopilot_subscriber", "autopilot_collaborator"] {
        assert!(object_exists(&pool, "table", t).await, "missing table {t}");
    }
    for idx in [
        "idx_autopilot_subscriber_actor",
        "idx_autopilot_collaborator_actor",
    ] {
        assert!(
            object_exists(&pool, "index", idx).await,
            "missing index {idx}"
        );
    }

    // (c) NOTHING was backfilled — not even "the creator is a collaborator",
    //     which would be a fabricated grant record (decision 5).
    assert_eq!(count(&pool, "autopilot_subscriber").await, 0);
    assert_eq!(count(&pool, "autopilot_collaborator").await, 0);

    // (d) The access_mode CHECK is engine-enforced.
    assert!(
        sqlx::query("UPDATE autopilot SET access_mode = 'wide-open' WHERE id = 'ap-1'")
            .execute(&pool)
            .await
            .is_err(),
        "access_mode is CHECK-constrained to the closed set"
    );
    assert!(
        sqlx::query("UPDATE autopilot SET access_mode = 'restricted' WHERE id = 'ap-1'")
            .execute(&pool)
            .await
            .is_ok(),
        "'restricted' is inside the closed set"
    );

    // (e) The actor_type CHECK bites on both new tables.
    for t in ["autopilot_subscriber", "autopilot_collaborator"] {
        let extra = if t == "autopilot_collaborator" {
            ", role"
        } else {
            ""
        };
        let extra_val = if t == "autopilot_collaborator" {
            ", 'editor'"
        } else {
            ""
        };
        let sql = format!(
            "INSERT INTO {t} (autopilot_id, workspace_id, actor_type, actor_id{extra}, created_at) \
             VALUES ('ap-1','ws-1','robot','x'{extra_val}, 40)"
        );
        assert!(
            sqlx::query(&sql).execute(&pool).await.is_err(),
            "{t} actor_type CHECK"
        );
    }

    // (f) Re-applying is a ledgered no-op.
    apply_migrations(&pool).await.expect("second apply is a no-op");
}
