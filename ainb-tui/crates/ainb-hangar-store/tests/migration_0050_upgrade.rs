//! Upgrade-from-populated test for migration 0050 (agent metadata columns +
//! `UNIQUE (workspace_id, name)`, multica gap #23).
//!
//! Fresh-database coverage lives in `tripwire_migrations_apply.rs`. This file
//! proves the migration is safe on a REAL populated database — the state every
//! upgrading install carries:
//!
//! 1. apply only the PRIOR migrations (0001..0049),
//! 2. seed TWO agents named `dup` in one workspace, each with an FK-PINNED
//!    terminal task row (the exact case multica's DELETE-then-constrain would
//!    abort on, bricking daemon boot),
//! 3. apply the embedded migrations (which adds 0050),
//! 4. assert both agents SURVIVE, exactly one still reads `dup`, the loser was
//!    RENAMED with its id appended, the new columns backfill to their defaults,
//!    the constraints now bite, and a double-apply is a no-op.

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use ainb_hangar_store::apply_migrations;

/// `sqlx` version number of the migration under test (`0050_agent_metadata.sql`).
const NEW_MIGRATION_VERSION: i64 = 50;

/// Open a fresh on-disk WAL pool in `dir` and apply only the migrations PRIOR to
/// [`NEW_MIGRATION_VERSION`], reproducing the schema an existing install runs
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

/// Seed two workspaces, an owner, a runtime, and the agents the upgrade must
/// survive: two `dup`-named agents in `ws-1` (each FK-pinned by a terminal task)
/// plus a same-named agent in `ws-2` (proving the constraint is per-workspace).
async fn seed_populated(pool: &SqlitePool) {
    for id in ["ws-1", "ws-2"] {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .expect("insert workspace");
    }
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
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES ('rt-1','ws-1','d-1','claude','local',0,'online')",
    )
    .execute(pool)
    .await
    .expect("insert runtime");

    // Two agents sharing a name in ws-1, plus one in ws-2 under the same name.
    for (id, ws) in [("a-first", "ws-1"), ("a-second", "ws-1"), ("a-other", "ws-2")] {
        insert_agent(pool, id, ws, "dup").await.expect("seed agent");
    }
    // FK-pin BOTH ws-1 agents with a terminal task: a migration that DELETEd the
    // duplicate (multica 046's shape) would trip FOREIGN KEY here and abort.
    for (task, agent) in [("t-1", "a-first"), ("t-2", "a-second")] {
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, status, created_at) \
             VALUES (?, 'ws-1', 'rt-1', ?, 'done', 0)",
        )
        .bind(task)
        .bind(agent)
        .execute(pool)
        .await
        .expect("insert terminal task");
    }
}

/// Insert one agent at the pre-0050 column set; returns the store result so a
/// test can assert on either success or a constraint refusal.
async fn insert_agent(
    pool: &SqlitePool,
    id: &str,
    workspace_id: &str,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, 'rt-1', 'workspace', 'user-1')",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(name)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Read one agent's stored `name`.
async fn name_of(pool: &SqlitePool, id: &str) -> String {
    sqlx::query_scalar("SELECT name FROM agent WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read agent name")
}

/// Migration 0050 on a populated DB: nothing is deleted, the duplicate is
/// renamed, and the new columns backfill to their documented defaults.
#[tokio::test]
async fn migration_0050_renames_duplicates_and_backfills_on_a_populated_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;

    // Before: three agents, two of them colliding on `dup` inside ws-1.
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent")
        .fetch_one(&pool)
        .await
        .expect("count agents");
    assert_eq!(before, 3);

    apply_migrations(&pool).await.expect("upgrade applies 0050");

    // (a) NOTHING was deleted — the FK-pinned history is intact.
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent")
        .fetch_one(&pool)
        .await
        .expect("count agents");
    assert_eq!(
        after, 3,
        "the migration must RENAME duplicates, never delete FK-pinned rows"
    );
    let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
        .fetch_one(&pool)
        .await
        .expect("count tasks");
    assert_eq!(tasks, 2, "the pinned history survives untouched");

    // (b) Exactly one ws-1 agent still reads `dup`; the later one carries its id.
    let first = name_of(&pool, "a-first").await;
    let second = name_of(&pool, "a-second").await;
    assert_eq!(first, "dup", "the first row keeps the plain name");
    assert_eq!(
        second, "dup (a-second)",
        "the later duplicate is renamed with its id appended"
    );
    let still_dup: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent WHERE workspace_id='ws-1' AND name='dup'")
            .fetch_one(&pool)
            .await
            .expect("count dup");
    assert_eq!(still_dup, 1);

    // (c) A same-named agent in ANOTHER workspace was NOT renamed — the
    //     constraint is per-workspace, not global.
    assert_eq!(
        name_of(&pool, "a-other").await,
        "dup",
        "a same-named agent in a different workspace is untouched"
    );

    // (d) The new columns backfill to their documented defaults on old rows.
    let row: (String, Option<String>, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT description, avatar_url, kind, system_key, service_tier \
         FROM agent WHERE id = 'a-first'",
    )
    .fetch_one(&pool)
    .await
    .expect("read new columns");
    assert_eq!(row.0, "", "description defaults to the empty string");
    assert_eq!(row.1, None, "a pre-0050 row has no avatar");
    assert_eq!(row.2, "user", "every pre-0050 agent is user-kind");
    assert_eq!(row.3, None);
    assert_eq!(row.4, None);

    // (e) Migration 0050 is recorded exactly once, and a double-apply changes
    //     nothing (no second rename pass mangling the already-renamed row).
    let recorded: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = ?")
            .bind(NEW_MIGRATION_VERSION)
            .fetch_one(&pool)
            .await
            .expect("read 0050 record");
    assert_eq!(recorded, 1, "0050 recorded as applied exactly once");
    apply_migrations(&pool).await.expect("second apply is a no-op");
    assert_eq!(name_of(&pool, "a-second").await, "dup (a-second)");

    pool.close().await;
}

/// After the upgrade the constraints BITE: a fresh duplicate insert fails, the
/// same name in another workspace is fine, and the description cap is enforced.
#[tokio::test]
async fn migration_0050_constraints_are_enforced_after_upgrade() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("upgrade applies 0050");

    // A fresh duplicate INSERT is now refused (the name `dup` is taken in ws-1).
    let err = insert_agent(&pool, "a-third", "ws-1", "dup")
        .await
        .expect_err("a duplicate name must now be refused");
    assert!(
        err.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation),
        "the refusal is a UNIQUE violation, got: {err}"
    );
    assert!(
        ainb_hangar_store::repo::agent::is_duplicate_name(&err),
        "is_duplicate_name must classify the (workspace_id, name) violation: {err}"
    );

    // The SAME name in a DIFFERENT workspace inserts fine (per-workspace scope).
    insert_agent(&pool, "a-ws2-second", "ws-2", "fresh")
        .await
        .expect("a name free in ws-2 inserts");
    insert_agent(&pool, "a-ws1-fresh", "ws-1", "fresh")
        .await
        .expect("the same name in the other workspace inserts");

    // The description CHECK: 255 characters is accepted, 256 is refused.
    let ok = "x".repeat(255);
    let too_long = "x".repeat(256);
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id, description) \
         VALUES ('a-desc-ok', 'ws-1', 'desc-ok', 'rt-1', 'workspace', 'user-1', ?)",
    )
    .bind(&ok)
    .execute(&pool)
    .await
    .expect("a 255-character description is accepted");
    let err = sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id, description) \
         VALUES ('a-desc-bad', 'ws-1', 'desc-bad', 'rt-1', 'workspace', 'user-1', ?)",
    )
    .bind(&too_long)
    .execute(&pool)
    .await
    .expect_err("a 256-character description must be refused");
    assert!(
        err.to_string().contains("CHECK"),
        "the refusal names the CHECK constraint, got: {err}"
    );

    pool.close().await;
}

/// The partial system-identity index constrains only NON-NULL `system_key`s: two
/// system agents sharing `(workspace, owner, runtime, key)` collide, but the
/// ordinary NULL-keyed agents are unconstrained (the whole point of `WHERE
/// system_key IS NOT NULL`).
#[tokio::test]
async fn migration_0050_system_identity_index_is_partial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_prior_schema(dir.path()).await;
    seed_populated(&pool).await;
    apply_migrations(&pool).await.expect("upgrade applies 0050");

    let insert_system = |id: &'static str, name: &'static str, key: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO agent \
                 (id, workspace_id, name, runtime_id, visibility, owner_id, kind, system_key) \
                 VALUES (?, 'ws-1', ?, 'rt-1', 'workspace', 'user-1', 'system', ?)",
            )
            .bind(id)
            .bind(name)
            .bind(key)
            .execute(&pool)
            .await
            .map(|_| ())
        }
    };

    insert_system("sys-1", "carrier-1", "agent_builder").await.expect("first system agent");
    let err = insert_system("sys-2", "carrier-2", "agent_builder")
        .await
        .expect_err("a second system agent with the same key must collide");
    assert!(
        err.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation),
        "the refusal is a UNIQUE violation, got: {err}"
    );
    assert!(
        !ainb_hangar_store::repo::agent::is_duplicate_name(&err),
        "a system_key collision must NOT be misread as a duplicate NAME: {err}"
    );

    // NULL system_key rows are unconstrained — three of them already coexist.
    let null_keyed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent WHERE system_key IS NULL")
            .fetch_one(&pool)
            .await
            .expect("count null-keyed agents");
    assert!(
        null_keyed >= 3,
        "the partial index leaves NULL-keyed agents unconstrained (got {null_keyed})"
    );

    pool.close().await;
}
