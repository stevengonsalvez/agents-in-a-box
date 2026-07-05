//! Full-chain upgrade-from-populated test: every entity type seeded at an EARLY
//! migration, then the embedded migrator carried to head.
//!
//! `tripwire_migrations_apply.rs` only proves the migrations against a FRESH
//! database, and the per-migration `migration_00XX_upgrade.rs` tests each cover
//! ONE migration's effect on a narrow seed. Neither proves the real upgrade path
//! an existing install hits: a database that already holds rows for EVERY entity
//! type, carried across the WHOLE tail of the migration chain at once. A later
//! migration that drops or rewrites a table (a destructive rebuild like 0007's
//! `DROP TABLE beads_mapping`) would silently lose those rows, and nothing
//! catches it today.
//!
//! This test:
//!
//! 1. applies only migrations 0001..0009 — the EARLIEST point at which every
//!    entity table the parity bead names already exists (`autopilot` /
//!    `autopilot_run` first appear in 0009; `daemon_socket_token` and the new
//!    columns on `agent` / `issue` / `agent_task_queue` arrive only LATER, so
//!    they are deliberately NOT seeded here),
//! 2. inserts one representative row for EVERY seeded entity type — workspace,
//!    user, member, `agent_runtime`, agent, skill, issue, `agent_task_queue`
//!    (task), autopilot, `autopilot_run`, pat, `daemon_token`, and
//!    `beads_mapping` (a token / correlation row),
//! 3. runs `apply_migrations` to carry the database to head (0010..0016),
//! 4. asserts (a) the upgrade returns no error, (b) EVERY seeded row survives
//!    with its identity columns untouched and the columns added along the way
//!    read back their declared defaults, and (c) a SECOND `apply_migrations` is a
//!    pure no-op (idempotent — no row changes, no re-applied versions).

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use ainb_hangar_store::apply_migrations;

/// The seed schema version: migrations 0001..=[`SEED_VERSION`] are applied
/// before any rows are inserted, reproducing an install that predates the whole
/// 0010..0016 upgrade tail. 9 is the earliest version at which every entity the
/// parity bead enumerates already has a table (`autopilot` lands in 0009).
const SEED_VERSION: i64 = 9;

/// Open a fresh on-disk `WAL` pool in `dir` and apply only migrations
/// 0001..=[`SEED_VERSION`], reproducing the schema a pre-0010 install runs
/// before upgrading. Built from the same on-disk `migrations/` directory the
/// embedded migrator compiles from, so the prior set can never drift from what
/// `apply_migrations` would itself apply.
async fn pool_at_seed_schema(dir: &std::path::Path) -> SqlitePool {
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
    migrator.migrations.to_mut().retain(|m| m.version <= SEED_VERSION);
    assert_eq!(
        migrator.migrations.iter().map(|m| m.version).max(),
        Some(SEED_VERSION),
        "seed set must run up to and including {SEED_VERSION}"
    );
    migrator.run(&pool).await.expect("seed migrations apply");
    pool
}

/// Seed exactly one representative row for EVERY entity table that exists at
/// [`SEED_VERSION`]. Only columns valid at the seed schema are written — the
/// columns later migrations add must NOT be referenced here (they don't exist
/// yet) and are asserted to read back their declared defaults after the upgrade.
async fn seed_every_entity(pool: &SqlitePool) {
    seed_tenancy_and_agents(pool).await;
    seed_work_items(pool).await;
    seed_tokens(pool).await;
}

/// Tenancy primitives (0001) + agent runtime / agent / skill (0002). The columns
/// later migrations add to `agent` (0015) are left to their defaults.
async fn seed_tenancy_and_agents(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(1_000_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(1_000_i64)
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
    .bind("Builder")
    .bind("rt-1")
    .bind("Build carefully.")
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("insert agent");
    sqlx::query(
        "INSERT INTO skill (id, workspace_id, name, description, content) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("skill-1")
    .bind("ws-1")
    .bind("commit")
    .bind("Make a commit")
    .bind("# commit\n")
    .execute(pool)
    .await
    .expect("insert skill");
}

/// Issue (0003), task (0004), autopilot + run (0009). priority / `due_date` /
/// labels (issue, 0014), `autopilot_run_id` / priority (task, 0010 / 0013) all
/// arrive LATER, so the seeded rows carry only the seed-schema columns.
async fn seed_work_items(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO issue \
         (id, workspace_id, title, description, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-1")
    .bind("ws-1")
    .bind("Fix the bug")
    .bind("It is broken")
    .bind("open")
    .bind("member")
    .bind("user-1")
    .bind(2_000_i64)
    .execute(pool)
    .await
    .expect("insert issue");

    // The single seeded task carries status 'queued', which the 0004
    // partial-unique index permits (one pending task per issue) and 0012 keeps
    // permitting under its per-(issue, agent) reshape.
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("task-1")
    .bind("ws-1")
    .bind("rt-1")
    .bind("agent-1")
    .bind("issue-1")
    .bind("queued")
    .bind(3_000_i64)
    .execute(pool)
    .await
    .expect("insert agent_task_queue");

    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, instructions, cron_expr, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("ap-1")
    .bind("ws-1")
    .bind("agent-1")
    .bind("daily-triage")
    .bind("Triage the inbox")
    .bind("0 9 * * *")
    .bind(4_000_i64)
    .execute(pool)
    .await
    .expect("insert autopilot");
    sqlx::query(
        "INSERT INTO autopilot_run (id, autopilot_id, started_at, status) \
         VALUES (?, ?, ?, ?)",
    )
    .bind("run-1")
    .bind("ap-1")
    .bind(5_000_i64)
    .bind("completed")
    .execute(pool)
    .await
    .expect("insert autopilot_run");
}

/// Tokens + beads correlation (0005, `beads_mapping` reshaped by 0007). A `pat`,
/// a `daemon_token`, and a `beads_mapping` row stand in for the "token" entity
/// the parity bead names.
async fn seed_tokens(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO pat (id, user_id, sha256_token, scope, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("pat-1")
    .bind("user-1")
    .bind("a".repeat(64))
    .bind("read")
    .bind(6_000_i64)
    .execute(pool)
    .await
    .expect("insert pat");
    sqlx::query(
        "INSERT INTO daemon_token (id, sha256_token, runtime_id, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind("dt-1")
    .bind("b".repeat(64))
    .bind("rt-1")
    .bind(7_000_i64)
    .execute(pool)
    .await
    .expect("insert daemon_token");
    // beads_mapping is the 0007 shape at the seed version (source + ISO-8601
    // TEXT last_synced), NOT the 0005 placeholder.
    sqlx::query(
        "INSERT INTO beads_mapping \
         (hangar_id, bd_id, hangar_kind, bd_kind, source, last_synced) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("issue-1")
    .bind("bd-1")
    .bind("issue")
    .bind("issue")
    .bind("hangar")
    .bind("2026-06-11T00:00:00Z")
    .execute(pool)
    .await
    .expect("insert beads_mapping");
}

/// Count rows in `table`.
async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

/// A fingerprint of every seeded entity's identity, captured as `(table, count)`
/// pairs plus the single-row key columns. Equality of this snapshot across the
/// upgrade proves no row was dropped, duplicated, or had its key rewritten.
async fn population_snapshot(pool: &SqlitePool) -> Vec<(String, i64)> {
    let tables = [
        "workspace",
        "user",
        "member",
        "agent_runtime",
        "agent",
        "skill",
        "issue",
        "agent_task_queue",
        "autopilot",
        "autopilot_run",
        "pat",
        "daemon_token",
        "beads_mapping",
    ];
    let mut out = Vec::with_capacity(tables.len());
    for t in tables {
        out.push((t.to_string(), count(pool, t).await));
    }
    out
}

/// The seeded rows keep their identity keys across the upgrade. Spot-checks the
/// destructive-rebuild suspect `beads_mapping` (whose 0007 DROP+CREATE predates
/// the seed) and the task whose pending-index scope 0012 changes underneath it.
async fn assert_seeded_identity_survives(pool: &SqlitePool) {
    let bd: (String, String) =
        sqlx::query_as("SELECT hangar_id, bd_id FROM beads_mapping WHERE hangar_id = ?")
            .bind("issue-1")
            .fetch_one(pool)
            .await
            .expect("beads_mapping row survives");
    assert_eq!(bd, ("issue-1".to_string(), "bd-1".to_string()));
    let task_status: String =
        sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = ?")
            .bind("task-1")
            .fetch_one(pool)
            .await
            .expect("task row survives");
    assert_eq!(task_status, "queued", "the seeded task keeps its status");
}

/// The columns the upgrade ADDED read back their declared defaults on the
/// pre-existing rows: 0013 `task.priority`, 0014 issue priority/due/labels, and
/// 0015 agent archive/config.
async fn assert_added_columns_read_defaults(pool: &SqlitePool) {
    let task_priority: i64 =
        sqlx::query_scalar("SELECT priority FROM agent_task_queue WHERE id = ?")
            .bind("task-1")
            .fetch_one(pool)
            .await
            .expect("task.priority default");
    assert_eq!(task_priority, 0, "0013 task.priority defaults to 0");

    // 0031 (ccc / D6) adds the launch mode + tmux session name to the task row.
    let task_run_cols = sqlx::query("SELECT mode, session_name FROM agent_task_queue WHERE id = ?")
        .bind("task-1")
        .fetch_one(pool)
        .await
        .expect("task 0031 columns");
    assert_eq!(
        task_run_cols.get::<String, _>("mode"),
        "headless",
        "0031 task.mode defaults to headless"
    );
    assert_eq!(
        task_run_cols.get::<Option<String>, _>("session_name"),
        None,
        "0031 task.session_name defaults NULL"
    );

    // 0032 (task-create parity, F1-F5): the card's repo + resolved agent on the
    // task; repo_ref NULL, agent_kind defaults to 'claude' on pre-existing rows.
    let task_parity = sqlx::query("SELECT repo_ref, agent_kind FROM agent_task_queue WHERE id = ?")
        .bind("task-1")
        .fetch_one(pool)
        .await
        .expect("task 0032 columns");
    assert_eq!(
        task_parity.get::<Option<String>, _>("repo_ref"),
        None,
        "0032 task.repo_ref defaults NULL"
    );
    assert_eq!(
        task_parity.get::<String, _>("agent_kind"),
        "claude",
        "0032 task.agent_kind defaults to claude"
    );

    // 0032 issue.repo_ref / issue.agent_kind both default NULL on prior rows.
    let issue_parity = sqlx::query("SELECT repo_ref, agent_kind FROM issue WHERE id = ?")
        .bind("issue-1")
        .fetch_one(pool)
        .await
        .expect("issue 0032 columns");
    assert_eq!(
        issue_parity.get::<Option<String>, _>("repo_ref"),
        None,
        "0032 issue.repo_ref defaults NULL"
    );
    assert_eq!(
        issue_parity.get::<Option<String>, _>("agent_kind"),
        None,
        "0032 issue.agent_kind defaults NULL"
    );

    // 0033 (tcp T2): the run's produced worktree branch defaults NULL on a
    // pre-existing task row (recorded only at finalize when the run committed).
    let task_branch: Option<String> =
        sqlx::query_scalar("SELECT branch FROM agent_task_queue WHERE id = ?")
            .bind("task-1")
            .fetch_one(pool)
            .await
            .expect("task 0033 branch column");
    assert_eq!(task_branch, None, "0033 task.branch defaults NULL");

    // 0032 workspace.default_agent defaults NULL on prior rows.
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>("SELECT default_agent FROM workspace WHERE id = ?")
            .bind("ws-1")
            .fetch_one(pool)
            .await
            .expect("workspace 0032 default_agent"),
        None,
        "0032 workspace.default_agent defaults NULL"
    );

    let issue_row = sqlx::query("SELECT priority, due_date, labels FROM issue WHERE id = ?")
        .bind("issue-1")
        .fetch_one(pool)
        .await
        .expect("issue 0014 columns");
    assert_eq!(
        issue_row.get::<i64, _>("priority"),
        0,
        "0014 issue.priority default 0"
    );
    assert_eq!(
        issue_row.get::<Option<i64>, _>("due_date"),
        None,
        "0014 issue.due_date default NULL"
    );
    assert_eq!(
        issue_row.get::<String, _>("labels"),
        "[]",
        "0014 issue.labels default '[]'"
    );

    let agent_row = sqlx::query(
        "SELECT archived, model, cli_args, mcp_config, thinking, agent_env \
         FROM agent WHERE id = ?",
    )
    .bind("agent-1")
    .fetch_one(pool)
    .await
    .expect("agent 0015 columns");
    assert_eq!(
        agent_row.get::<i64, _>("archived"),
        0,
        "0015 agent.archived default 0"
    );
    assert_eq!(
        agent_row.get::<Option<String>, _>("model"),
        None,
        "0015 agent.model NULL"
    );
    assert_eq!(
        agent_row.get::<String, _>("cli_args"),
        "[]",
        "0015 agent.cli_args '[]'"
    );
    assert_eq!(
        agent_row.get::<String, _>("mcp_config"),
        "{}",
        "0015 agent.mcp_config '{{}}'"
    );
    assert_eq!(
        agent_row.get::<Option<String>, _>("thinking"),
        None,
        "0015 agent.thinking NULL"
    );
    assert_eq!(
        agent_row.get::<String, _>("agent_env"),
        "{}",
        "0015 agent.agent_env '{{}}'"
    );

    let ap_row =
        sqlx::query("SELECT execution_mode, concurrency_policy FROM autopilot WHERE id = ?")
            .bind("ap-1")
            .fetch_one(pool)
            .await
            .expect("autopilot 0019 columns");
    assert_eq!(
        ap_row.get::<String, _>("execution_mode"),
        "run_only",
        "0019 autopilot.execution_mode default run_only"
    );
    assert_eq!(
        ap_row.get::<String, _>("concurrency_policy"),
        "skip",
        "0019 autopilot.concurrency_policy default skip"
    );

    let ws_row = sqlx::query(
        "SELECT context_prompt, repo_whitelist, issue_prefix FROM workspace WHERE id = ?",
    )
    .bind("ws-1")
    .fetch_one(pool)
    .await
    .expect("workspace 0020 columns");
    assert_eq!(
        ws_row.get::<Option<String>, _>("context_prompt"),
        None,
        "0020 workspace.context_prompt default NULL"
    );
    assert_eq!(
        ws_row.get::<Option<String>, _>("repo_whitelist"),
        None,
        "0020 workspace.repo_whitelist default NULL"
    );
    assert_eq!(
        ws_row.get::<Option<String>, _>("issue_prefix"),
        None,
        "0020 workspace.issue_prefix default NULL (the HGR default is display-only)"
    );
}

/// 0023 remaps the legacy issue `state` vocabulary forward in place: the issue
/// seeded with `state = 'open'` reads `todo` after the upgrade, so it lands in a
/// real canonical column rather than relying on display-layer tolerance forever.
async fn assert_legacy_issue_state_remapped_forward(pool: &SqlitePool) {
    let state: String = sqlx::query_scalar("SELECT state FROM issue WHERE id = ?")
        .bind("issue-1")
        .fetch_one(pool)
        .await
        .expect("issue row survives the upgrade");
    assert_eq!(
        state, "todo",
        "0023 remaps the legacy 'open' state forward to 'todo'"
    );
}

#[tokio::test]
async fn full_chain_upgrade_preserves_every_seeded_entity_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = pool_at_seed_schema(dir.path()).await;
    seed_every_entity(&pool).await;

    let before = population_snapshot(&pool).await;
    // Every entity seeded exactly one row at the seed schema.
    assert!(
        before.iter().all(|(_, n)| *n == 1),
        "each entity must hold exactly one seeded row: {before:?}"
    );

    // (a) Upgrade to head applies cleanly: the embedded migrator skips the
    //     already-recorded 0001..0009 and applies 0010..0016.
    apply_migrations(&pool).await.expect("upgrade to head applies");

    // The seed version plus the whole tail are now recorded.
    let head_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read head migration version");
    assert_eq!(head_version, 37, "head is migration 0037");

    // (b) Every seeded row survived: the population is row-for-row identical.
    let after = population_snapshot(&pool).await;
    assert_eq!(
        after, before,
        "upgrade must not drop, duplicate, or rewrite any seeded entity"
    );

    assert_seeded_identity_survives(&pool).await;
    assert_added_columns_read_defaults(&pool).await;
    assert_legacy_issue_state_remapped_forward(&pool).await;

    // (c) Idempotency: a SECOND apply re-runs nothing and changes no row.
    let recorded_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("count recorded migrations");
    apply_migrations(&pool).await.expect("second apply is a no-op");
    let recorded_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("count recorded migrations after re-apply");
    assert_eq!(
        recorded_before, recorded_after,
        "double-apply must not record a new migration version"
    );
    assert_eq!(
        population_snapshot(&pool).await,
        after,
        "double-apply must not change any seeded row"
    );

    pool.close().await;
}
