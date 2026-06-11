//! Upgrade-from-populated test for migration 0017 (squads + leader routing).
//!
//! Migration 0017 adds two tables — `squad` (a workspace-scoped group with a
//! polymorphic LEADER actor-ref and a `(workspace_id, name)` UNIQUE index) and
//! `squad_member` (the membership join with a `(squad_id, member_type,
//! member_id)` composite PK) — without touching any existing row.
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0016),
//! 2. seed a live workspace + member + agent (the actors a squad references),
//! 3. apply the embedded migrations (which adds exactly 0017),
//! 4. assert the pre-existing rows survive intact, the two new tables accept
//!    writes, the `(workspace_id, name)` UNIQUE index rejects a duplicate squad
//!    name, the `leader_type` CHECK rejects a junk discriminant, the
//!    `(squad_id, member_type, member_id)` composite PK rejects a duplicate
//!    membership, and a second apply is a no-op (idempotent).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test (`0017_squad.sql`).
const NEW_MIGRATION_VERSION: i64 = 17;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only the migrations PRIOR
/// to [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install runs
/// before upgrading. Uses the same on-disk `migrations/` directory the embedded
/// migrator is compiled from, so the prior set can never drift from what
/// `apply_migrations` would itself apply.
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

/// Seed the FK graph the new tables reference: one workspace + user + member,
/// plus one runtime + agent (the leader / member actors a squad points at). The
/// rows the table additions must leave intact.
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
    .bind("Leader")
    .bind("rt-1")
    .bind(None::<String>)
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("insert agent");
}

/// Snapshot the pre-existing workspace/member/agent identity, ordered, as the
/// data the table additions must not touch.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for t in ["workspace", "user", "member", "agent_runtime", "agent"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("count {t}: {e}"));
        out.push((t.to_string(), n));
    }
    out
}

/// The new `squad` table accepts a write with an `agent` leader, enforces the
/// `(workspace_id, name)` UNIQUE index (resolve-or-reject guard), and rejects a
/// junk `leader_type` via its CHECK. Seeds `squad-1` (led by `agent-1`) for the
/// membership assertions that follow.
async fn assert_squad_table_writes_and_guards(pool: &SqlitePool) {
    let insert = |id: &'static str, name: &'static str, leader_type: &'static str, at: i64| {
        sqlx::query(
            "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind("ws-1")
        .bind(name)
        .bind(leader_type)
        .bind("agent-1")
        .bind(at)
        .execute(pool)
    };
    insert("squad-1", "alpha-squad", "agent", 10)
        .await
        .expect("insert squad with an agent leader");
    assert!(
        insert("squad-2", "alpha-squad", "agent", 11).await.is_err(),
        "the (workspace_id, name) UNIQUE index must reject a duplicate squad name"
    );
    assert!(
        insert("squad-3", "beta-squad", "robot", 12).await.is_err(),
        "the leader_type CHECK must reject a junk discriminant"
    );

    // The leader read-back the routing layer leans on.
    let leader = sqlx::query("SELECT leader_type, leader_id FROM squad WHERE id = ?")
        .bind("squad-1")
        .fetch_one(pool)
        .await
        .expect("read squad leader");
    assert_eq!(leader.get::<String, _>("leader_type"), "agent");
    assert_eq!(leader.get::<String, _>("leader_id"), "agent-1");
}

/// The `squad_member` join accepts a human + an agent membership (polymorphic
/// actor-refs) and enforces the `(squad_id, member_type, member_id)` composite PK
/// (the dedupe guard). Operates on `squad-1` seeded by
/// [`assert_squad_table_writes_and_guards`].
async fn assert_squad_member_join_writes_and_guards(pool: &SqlitePool) {
    let attach = |member_type: &'static str, member_id: &'static str| {
        sqlx::query("INSERT INTO squad_member (squad_id, member_type, member_id) VALUES (?, ?, ?)")
            .bind("squad-1")
            .bind(member_type)
            .bind(member_id)
            .execute(pool)
    };
    attach("member", "user-1").await.expect("attach a human member");
    attach("agent", "agent-1").await.expect("attach an agent member");
    assert!(
        attach("member", "user-1").await.is_err(),
        "the (squad_id, member_type, member_id) composite PK must reject a duplicate membership"
    );
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM squad_member WHERE squad_id = ?")
        .bind("squad-1")
        .fetch_one(pool)
        .await
        .expect("count members");
    assert_eq!(members, 2, "exactly two distinct memberships landed");
}

#[tokio::test]
async fn migration_0017_upgrades_populated_database_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    let before = population_snapshot(&pool).await;
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each seeded entity has one row: {before:?}"
    );

    // Upgrade: the embedded migrator skips 0001..0016 (already recorded) and
    // applies 0017.
    apply_migrations(&pool).await.expect("upgrade applies 0017");
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read migration version");
    assert_eq!(recorded, 1, "0017 recorded as applied");

    // 1. The pre-existing population survives intact — the table additions touch
    //    no existing row.
    let after = population_snapshot(&pool).await;
    assert_eq!(after, before, "upgrade must not touch existing rows");

    // 2. The new `squad` table accepts writes + enforces its name UNIQUE index and
    //    leader CHECK; 3. the `squad_member` join writes + enforces its composite
    //    PK; plus the leader read-back the routing layer leans on.
    assert_squad_table_writes_and_guards(&pool).await;
    assert_squad_member_join_writes_and_guards(&pool).await;

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
