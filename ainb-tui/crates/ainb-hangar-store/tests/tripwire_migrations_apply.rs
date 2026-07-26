//! Tripwire: the embedded migrations apply cleanly to a fresh `SQLite` DB and
//! create the expected v1 schema. Uses a real on-disk `tempdir/hangar.db`
//! (not `:memory:`) so `WAL` is exercised exactly like production.

use ainb_hangar_store::apply_migrations;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// Open a fresh on-disk `SQLite` pool inside a tempdir and apply all migrations.
async fn fresh_pool(dir: &std::path::Path) -> SqlitePool {
    let db_path = dir.join("hangar.db");
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .expect("valid sqlite url")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");
    apply_migrations(&pool).await.expect("migrations apply");
    pool
}

/// Return the `sql` definition recorded in `sqlite_master` for a given table,
/// with runs of whitespace collapsed to a single space so column-alignment
/// padding in the migration source does not make substring assertions brittle.
async fn table_sql(pool: &SqlitePool, name: &str) -> String {
    let row = sqlx::query("SELECT sql FROM sqlite_master WHERE type='table' AND name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("table {name} missing: {e}"));
    let raw: String = row.get("sql");
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test]
async fn migrations_apply_to_fresh_sqlite_and_create_workspace_user_member_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    // Exactly the three expected tables exist after 0001.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='table' AND name IN ('workspace','user','member')",
    )
    .fetch_one(&pool)
    .await
    .expect("count query");
    assert_eq!(count, 3, "expected workspace, user, member tables");

    // workspace columns + constraints.
    let ws = table_sql(&pool, "workspace").await;
    assert!(ws.contains("id TEXT PRIMARY KEY"), "workspace.id PK: {ws}");
    assert!(
        ws.contains("slug TEXT NOT NULL UNIQUE"),
        "workspace.slug unique: {ws}"
    );
    assert!(
        ws.contains("name TEXT NOT NULL"),
        "workspace.name not null: {ws}"
    );
    assert!(
        ws.contains("created_at INTEGER NOT NULL"),
        "workspace.created_at epoch millis: {ws}"
    );

    // user columns + constraints.
    let user = table_sql(&pool, "user").await;
    assert!(user.contains("id TEXT PRIMARY KEY"), "user.id PK: {user}");
    assert!(
        user.contains("email TEXT NOT NULL UNIQUE"),
        "user.email unique: {user}"
    );
    assert!(
        user.contains("created_at INTEGER NOT NULL"),
        "user.created_at: {user}"
    );

    // member composite PK + role CHECK.
    let member = table_sql(&pool, "member").await;
    assert!(
        member.contains("PRIMARY KEY (workspace_id, user_id)"),
        "member composite PK: {member}"
    );
    assert!(
        member.contains("role TEXT NOT NULL"),
        "member.role not null: {member}"
    );
    assert!(
        member.contains("CHECK (role IN ('owner','admin','member'))"),
        "member.role CHECK: {member}"
    );

    pool.close().await;
}

/// Return the `sql` definition recorded in `sqlite_master` for a named index,
/// with runs of whitespace collapsed so assertions are not padding-sensitive.
async fn index_sql(pool: &SqlitePool, name: &str) -> String {
    let row = sqlx::query("SELECT sql FROM sqlite_master WHERE type='index' AND name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("index {name} missing: {e}"));
    let raw: String = row.get("sql");
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tokio::test]
async fn migration_0002_creates_agent_runtime_table_and_unique_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let rt = table_sql(&pool, "agent_runtime").await;
    assert!(
        rt.contains("id TEXT PRIMARY KEY"),
        "agent_runtime.id PK: {rt}"
    );
    assert!(
        rt.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "agent_runtime.workspace_id FK: {rt}"
    );
    assert!(
        rt.contains("daemon_id TEXT NOT NULL"),
        "agent_runtime.daemon_id: {rt}"
    );
    assert!(
        rt.contains("provider TEXT NOT NULL"),
        "agent_runtime.provider: {rt}"
    );
    assert!(
        rt.contains("runtime_mode TEXT NOT NULL CHECK (runtime_mode IN ('local','cloud'))"),
        "agent_runtime.runtime_mode CHECK: {rt}"
    );
    assert!(
        rt.contains("last_seen_at INTEGER"),
        "agent_runtime.last_seen_at: {rt}"
    );
    assert!(
        rt.contains("status TEXT NOT NULL DEFAULT 'offline'"),
        "agent_runtime.status default: {rt}"
    );

    let idx = index_sql(&pool, "idx_agent_runtime_workspace_daemon_provider").await;
    assert!(
        idx.contains("UNIQUE"),
        "idx_agent_runtime_workspace_daemon_provider is UNIQUE: {idx}"
    );
    assert!(
        idx.contains("agent_runtime") && idx.contains("(workspace_id, daemon_id, provider)"),
        "idx_agent_runtime_workspace_daemon_provider columns: {idx}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0002_creates_agent_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let agent = table_sql(&pool, "agent").await;
    assert!(
        agent.contains("id TEXT PRIMARY KEY"),
        "agent.id PK: {agent}"
    );
    assert!(
        agent.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "agent.workspace_id FK: {agent}"
    );
    assert!(agent.contains("name TEXT NOT NULL"), "agent.name: {agent}");
    assert!(
        agent.contains("runtime_id TEXT NOT NULL REFERENCES agent_runtime(id)"),
        "agent.runtime_id FK (required by reference pattern): {agent}"
    );
    assert!(
        agent.contains("instructions TEXT"),
        "agent.instructions: {agent}"
    );
    assert!(
        agent.contains("visibility TEXT NOT NULL CHECK (visibility IN ('workspace','private'))"),
        "agent.visibility CHECK: {agent}"
    );
    assert!(
        agent.contains("owner_id TEXT NOT NULL REFERENCES user(id)"),
        "agent.owner_id FK: {agent}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0002_creates_skill_tables_with_composite_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let skill = table_sql(&pool, "skill").await;
    assert!(
        skill.contains("id TEXT PRIMARY KEY"),
        "skill.id PK: {skill}"
    );
    assert!(
        skill.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "skill.workspace_id FK: {skill}"
    );
    assert!(skill.contains("name TEXT NOT NULL"), "skill.name: {skill}");
    assert!(
        skill.contains("description TEXT"),
        "skill.description: {skill}"
    );
    assert!(skill.contains("content TEXT"), "skill.content: {skill}");

    let skill_file = table_sql(&pool, "skill_file").await;
    assert!(
        skill_file.contains("skill_id TEXT NOT NULL REFERENCES skill(id)"),
        "skill_file.skill_id FK: {skill_file}"
    );
    assert!(
        skill_file.contains("path TEXT NOT NULL"),
        "skill_file.path: {skill_file}"
    );
    assert!(
        skill_file.contains("content TEXT"),
        "skill_file.content: {skill_file}"
    );
    assert!(
        skill_file.contains("PRIMARY KEY (skill_id, path)"),
        "skill_file composite PK: {skill_file}"
    );

    let agent_skill = table_sql(&pool, "agent_skill").await;
    assert!(
        agent_skill.contains("agent_id TEXT NOT NULL REFERENCES agent(id)"),
        "agent_skill.agent_id FK: {agent_skill}"
    );
    assert!(
        agent_skill.contains("skill_id TEXT NOT NULL REFERENCES skill(id)"),
        "agent_skill.skill_id FK: {agent_skill}"
    );
    assert!(
        agent_skill.contains("PRIMARY KEY (agent_id, skill_id)"),
        "agent_skill composite PK: {agent_skill}"
    );
    // Migration 0051 (parity #24): per-agent enable/disable toggle. `ALTER TABLE
    // ... ADD COLUMN` rewrites `sqlite_master.sql`, so the added column is
    // visible in the stored DDL text.
    assert!(
        agent_skill.contains("enabled INTEGER NOT NULL DEFAULT 1"),
        "agent_skill.enabled default-true (0051): {agent_skill}"
    );

    // Migration 0051 (parity #24, multica 206): per-agent runtime-level skill
    // suppression list, JSON array text defaulting to the empty array.
    let agent = table_sql(&pool, "agent").await;
    assert!(
        agent.contains("disabled_runtime_skills TEXT NOT NULL DEFAULT '[]'"),
        "agent.disabled_runtime_skills (0051): {agent}"
    );

    // Migration 0052 (parity #26, multica 031/085): the archive AUDIT sidecar.
    // Both columns are NULLABLE with no default — a pre-0052 archived row keeps
    // NULL (an honest unknown), and there is deliberately no CHECK tying
    // `archived = 1` to a non-null stamp.
    assert!(
        agent.contains("archived_at INTEGER"),
        "agent.archived_at (0052): {agent}"
    );
    assert!(
        agent.contains("archived_by TEXT"),
        "agent.archived_by (0052): {agent}"
    );

    let squad = table_sql(&pool, "squad").await;
    assert!(
        squad.contains("archived INTEGER NOT NULL DEFAULT 0"),
        "squad.archived default-false (0052): {squad}"
    );
    assert!(
        squad.contains("archived_at INTEGER"),
        "squad.archived_at (0052): {squad}"
    );
    assert!(
        squad.contains("archived_by TEXT"),
        "squad.archived_by (0052): {squad}"
    );

    // Migration 0053 (parity #25, multica 084/088): per-member ROLE + per-squad
    // INSTRUCTIONS. Both are NOT NULL DEFAULT '' — "unset" is the empty string,
    // which is also the "omit this fragment" sentinel the leader briefing
    // renders against. Free text, no CHECK (role is a label, not a vocabulary).
    assert!(
        squad.contains("instructions TEXT NOT NULL DEFAULT ''"),
        "squad.instructions (0053): {squad}"
    );
    let squad_member = table_sql(&pool, "squad_member").await;
    assert!(
        squad_member.contains("role TEXT NOT NULL DEFAULT ''"),
        "squad_member.role (0053): {squad_member}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0003_creates_issue_comment_with_polymorphic_actors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let issue = table_sql(&pool, "issue").await;
    assert!(
        issue.contains("id TEXT PRIMARY KEY"),
        "issue.id PK: {issue}"
    );
    assert!(
        issue.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "issue.workspace_id FK: {issue}"
    );
    assert!(
        issue.contains("title TEXT NOT NULL"),
        "issue.title: {issue}"
    );
    assert!(
        issue.contains("description TEXT"),
        "issue.description: {issue}"
    );
    assert!(
        issue.contains("state TEXT NOT NULL DEFAULT 'open'"),
        "issue.state default: {issue}"
    );
    assert!(
        issue.contains("assignee_type TEXT CHECK (assignee_type IN ('member','agent'))"),
        "issue.assignee_type CHECK: {issue}"
    );
    assert!(
        issue.contains("assignee_id TEXT"),
        "issue.assignee_id: {issue}"
    );
    assert!(
        issue.contains("creator_type TEXT NOT NULL CHECK (creator_type IN ('member','agent'))"),
        "issue.creator_type CHECK: {issue}"
    );
    assert!(
        issue.contains("creator_id TEXT NOT NULL"),
        "issue.creator_id: {issue}"
    );
    assert!(
        issue.contains("created_at INTEGER NOT NULL"),
        "issue.created_at: {issue}"
    );

    let comment = table_sql(&pool, "comment").await;
    assert!(
        comment.contains("id TEXT PRIMARY KEY"),
        "comment.id PK: {comment}"
    );
    assert!(
        comment.contains("issue_id TEXT NOT NULL REFERENCES issue(id) ON DELETE CASCADE"),
        "comment.issue_id FK cascade: {comment}"
    );
    assert!(
        comment.contains("author_type TEXT NOT NULL CHECK (author_type IN ('member','agent'))"),
        "comment.author_type CHECK: {comment}"
    );
    assert!(
        comment.contains("author_id TEXT NOT NULL"),
        "comment.author_id: {comment}"
    );
    assert!(
        comment.contains("body TEXT NOT NULL"),
        "comment.body: {comment}"
    );
    assert!(
        comment.contains("created_at INTEGER NOT NULL"),
        "comment.created_at: {comment}"
    );

    let idx = index_sql(&pool, "idx_issue_workspace_state").await;
    assert!(
        idx.contains("issue") && idx.contains("(workspace_id, state)"),
        "idx_issue_workspace_state columns: {idx}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0004_creates_agent_task_queue_with_partial_unique() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let tq = table_sql(&pool, "agent_task_queue").await;
    assert!(
        tq.contains("id TEXT PRIMARY KEY"),
        "agent_task_queue.id PK: {tq}"
    );
    assert!(
        tq.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "agent_task_queue.workspace_id FK: {tq}"
    );
    assert!(
        tq.contains("runtime_id TEXT NOT NULL REFERENCES agent_runtime(id)"),
        "agent_task_queue.runtime_id FK: {tq}"
    );
    assert!(
        tq.contains("agent_id TEXT NOT NULL REFERENCES agent(id)"),
        "agent_task_queue.agent_id FK: {tq}"
    );
    assert!(
        tq.contains("issue_id TEXT REFERENCES issue(id)"),
        "agent_task_queue.issue_id nullable FK: {tq}"
    );
    assert!(
        tq.contains(
            "status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN \
             ('queued','dispatched','running','done','failed','cancelled'))"
        ),
        "agent_task_queue.status default + CHECK: {tq}"
    );
    assert!(tq.contains("result TEXT"), "agent_task_queue.result: {tq}");
    assert!(
        tq.contains("session_id TEXT"),
        "agent_task_queue.session_id: {tq}"
    );
    assert!(
        tq.contains("work_dir TEXT"),
        "agent_task_queue.work_dir: {tq}"
    );
    assert!(
        tq.contains("attempt INTEGER NOT NULL DEFAULT 1"),
        "agent_task_queue.attempt default: {tq}"
    );
    assert!(
        tq.contains("max_attempts INTEGER NOT NULL DEFAULT 2"),
        "agent_task_queue.max_attempts default: {tq}"
    );
    assert!(
        tq.contains("parent_task_id TEXT REFERENCES agent_task_queue(id)"),
        "agent_task_queue.parent_task_id self-FK: {tq}"
    );
    assert!(
        tq.contains("failure_reason TEXT"),
        "agent_task_queue.failure_reason: {tq}"
    );
    assert!(
        tq.contains("created_at INTEGER NOT NULL"),
        "agent_task_queue.created_at: {tq}"
    );
    assert!(
        tq.contains("started_at INTEGER"),
        "agent_task_queue.started_at: {tq}"
    );
    assert!(
        tq.contains("finished_at INTEGER"),
        "agent_task_queue.finished_at: {tq}"
    );

    // Migration 0012 replaces the 0004 global-per-issue index with the
    // per-(issue, agent) scope (reference ClaimAgentTask parity), so the final
    // schema carries `idx_one_pending_task_per_issue_agent` and the old name
    // must be gone.
    let old_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='index' AND name='idx_one_pending_task_per_issue'",
    )
    .fetch_one(&pool)
    .await
    .expect("count old index");
    assert_eq!(
        old_count, 0,
        "the 0004 global-per-issue index must be dropped by 0012"
    );

    let idx = index_sql(&pool, "idx_one_pending_task_per_issue_agent").await;
    assert!(
        idx.contains("UNIQUE"),
        "idx_one_pending_task_per_issue_agent is UNIQUE: {idx}"
    );
    assert!(
        idx.contains("agent_task_queue") && idx.contains("(issue_id, agent_id)"),
        "idx_one_pending_task_per_issue_agent on agent_task_queue(issue_id, agent_id): {idx}"
    );
    assert!(
        idx.contains("WHERE") && idx.contains("status IN ('queued','dispatched')"),
        "idx_one_pending_task_per_issue_agent partial predicate: {idx}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0005_creates_pat_daemon_token_beads_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    // pat: personal access tokens, hashed.
    let pat = table_sql(&pool, "pat").await;
    assert!(pat.contains("id TEXT PRIMARY KEY"), "pat.id PK: {pat}");
    assert!(
        pat.contains("user_id TEXT NOT NULL REFERENCES user(id)"),
        "pat.user_id FK: {pat}"
    );
    assert!(
        pat.contains("sha256_token TEXT NOT NULL UNIQUE"),
        "pat.sha256_token unique: {pat}"
    );
    assert!(pat.contains("scope TEXT"), "pat.scope: {pat}");
    assert!(
        pat.contains("created_at INTEGER NOT NULL"),
        "pat.created_at: {pat}"
    );
    assert!(pat.contains("last_used INTEGER"), "pat.last_used: {pat}");

    // daemon_token: per-runtime daemon bearer tokens, hashed.
    let dt = table_sql(&pool, "daemon_token").await;
    assert!(
        dt.contains("id TEXT PRIMARY KEY"),
        "daemon_token.id PK: {dt}"
    );
    assert!(
        dt.contains("sha256_token TEXT NOT NULL UNIQUE"),
        "daemon_token.sha256_token unique: {dt}"
    );
    assert!(
        dt.contains("runtime_id TEXT NOT NULL REFERENCES agent_runtime(id)"),
        "daemon_token.runtime_id FK: {dt}"
    );
    assert!(
        dt.contains("created_at INTEGER NOT NULL"),
        "daemon_token.created_at: {dt}"
    );

    // beads_mapping: P0 placeholder (0005). Its final P2-ready shape is
    // re-built by 0007 and asserted in
    // `migration_0007_reshapes_beads_mapping` below — `table_sql` reflects the
    // schema after ALL migrations, so the 0005 placeholder columns no longer
    // exist by the time this pool is queried.

    pool.close().await;
}

#[tokio::test]
async fn migration_0007_reshapes_beads_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    // 0007 rebuilds beads_mapping for the P2 sync adapter: adds `source`,
    // splits the composite PK into an independent UNIQUE per id side, and stores
    // `last_synced` as ISO-8601 TEXT (not INTEGER ms).
    let bm = table_sql(&pool, "beads_mapping").await;
    assert!(
        bm.contains("hangar_id TEXT NOT NULL PRIMARY KEY"),
        "beads_mapping.hangar_id PK: {bm}"
    );
    assert!(
        bm.contains("bd_id TEXT NOT NULL UNIQUE"),
        "beads_mapping.bd_id UNIQUE: {bm}"
    );
    assert!(
        bm.contains("hangar_kind TEXT NOT NULL CHECK (hangar_kind IN ('issue','task'))"),
        "beads_mapping.hangar_kind CHECK: {bm}"
    );
    assert!(
        bm.contains("bd_kind TEXT NOT NULL CHECK (bd_kind IN ('issue','task'))"),
        "beads_mapping.bd_kind CHECK: {bm}"
    );
    assert!(
        bm.contains("source TEXT NOT NULL CHECK (source IN ('hangar','swarm'))"),
        "beads_mapping.source CHECK: {bm}"
    );
    assert!(
        bm.contains("last_synced TEXT NOT NULL"),
        "beads_mapping.last_synced TEXT: {bm}"
    );
    assert!(
        !bm.contains("PRIMARY KEY (hangar_id, bd_id)"),
        "the 0005 composite PK must be gone: {bm}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0009_creates_autopilot_tables_with_scoping_indexes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let ap = table_sql(&pool, "autopilot").await;
    assert!(ap.contains("id TEXT PRIMARY KEY"), "autopilot.id PK: {ap}");
    assert!(
        ap.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "autopilot.workspace_id FK: {ap}"
    );
    assert!(
        ap.contains("agent_id TEXT NOT NULL REFERENCES agent(id)"),
        "autopilot.agent_id FK: {ap}"
    );
    assert!(ap.contains("name TEXT NOT NULL"), "autopilot.name: {ap}");
    assert!(
        ap.contains("instructions TEXT"),
        "autopilot.instructions: {ap}"
    );
    assert!(
        ap.contains("cron_expr TEXT NOT NULL"),
        "autopilot.cron_expr: {ap}"
    );
    assert!(
        ap.contains("max_concurrent_runs INTEGER NOT NULL DEFAULT 1"),
        "autopilot.max_concurrent_runs default: {ap}"
    );
    assert!(
        ap.contains("next_tick_at INTEGER"),
        "autopilot.next_tick_at epoch-ms INTEGER (nullable): {ap}"
    );
    assert!(
        ap.contains("enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))"),
        "autopilot.enabled 0/1 INTEGER: {ap}"
    );
    assert!(
        ap.contains("created_at INTEGER NOT NULL"),
        "autopilot.created_at: {ap}"
    );

    let name_idx = index_sql(&pool, "idx_autopilot_workspace_name").await;
    assert!(
        name_idx.contains("UNIQUE"),
        "idx_autopilot_workspace_name UNIQUE: {name_idx}"
    );
    assert!(
        name_idx.contains("autopilot") && name_idx.contains("(workspace_id, name)"),
        "idx_autopilot_workspace_name columns: {name_idx}"
    );

    let tick_idx = index_sql(&pool, "idx_autopilot_next_tick").await;
    assert!(
        tick_idx.contains("autopilot") && tick_idx.contains("(workspace_id, next_tick_at)"),
        "idx_autopilot_next_tick columns: {tick_idx}"
    );
    assert!(
        tick_idx.contains("WHERE enabled = 1"),
        "idx_autopilot_next_tick is partial on enabled rows: {tick_idx}"
    );

    let run = table_sql(&pool, "autopilot_run").await;
    assert!(
        run.contains("id TEXT PRIMARY KEY"),
        "autopilot_run.id PK: {run}"
    );
    assert!(
        run.contains("autopilot_id TEXT NOT NULL REFERENCES autopilot(id)"),
        "autopilot_run.autopilot_id FK: {run}"
    );
    assert!(
        run.contains("started_at INTEGER NOT NULL"),
        "autopilot_run.started_at: {run}"
    );
    assert!(
        run.contains("completed_at INTEGER"),
        "autopilot_run.completed_at nullable: {run}"
    );
    assert!(
        run.contains(
            "status TEXT NOT NULL DEFAULT 'running' CHECK (status IN \
             ('running', 'completed', 'failed', 'cancelled'))"
        ),
        "autopilot_run.status default + CHECK: {run}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0013_adds_task_priority_column() {
    // `priority` (0..3 = P3..P0, higher = more urgent; default 0 = P3) feeds
    // the claim ordering `ORDER BY priority DESC, created_at, id` (reference
    // ordering parity). ALTER TABLE ADD COLUMN rewrites the catalog SQL, so
    // the column shows up in `sqlite_master` like the originals.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let tq = table_sql(&pool, "agent_task_queue").await;
    assert!(
        tq.contains("priority INTEGER NOT NULL DEFAULT 0"),
        "agent_task_queue.priority default 0: {tq}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0014_adds_issue_priority_due_date_labels_columns() {
    // The issue create flow (parity-review gap: "create-issue partial — no
    // priority/dates/labels") needs the issue itself to carry urgency, a due
    // date, and labels. Three columns land on `issue`:
    //   - `priority INTEGER NOT NULL DEFAULT 0` (0..3 = P3..P0, higher = more
    //     urgent), mirroring `agent_task_queue.priority` (migration 0013);
    //   - `due_date INTEGER` (epoch millis, nullable — no due date by default);
    //   - `labels TEXT NOT NULL DEFAULT '[]'` (a JSON array of label strings;
    //     the full labels table + attach/detach RPC is a SEPARATE bead).
    // ALTER TABLE ADD COLUMN rewrites the catalog SQL, so each shows up in
    // `sqlite_master` like the originals.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let issue = table_sql(&pool, "issue").await;
    assert!(
        issue.contains("priority INTEGER NOT NULL DEFAULT 0"),
        "issue.priority default 0: {issue}"
    );
    assert!(
        issue.contains("due_date INTEGER"),
        "issue.due_date nullable epoch-ms: {issue}"
    );
    assert!(
        issue.contains("labels TEXT NOT NULL DEFAULT '[]'"),
        "issue.labels default empty JSON array: {issue}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0015_adds_agent_archive_and_config_columns() {
    // The agent edit/archive surface (parity-review gap: "create/edit/archive
    // agents + configure runtime/model/args partial") needs the `agent` table to
    // carry an archive flag and the per-agent runtime config knobs. Six columns
    // land on `agent`:
    //   - `archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))`;
    //   - `model TEXT` (nullable provider model override);
    //   - `cli_args TEXT NOT NULL DEFAULT '[]'` (JSON array of CLI args);
    //   - `mcp_config TEXT NOT NULL DEFAULT '{}'` (JSON object MCP config);
    //   - `thinking TEXT` (nullable reasoning level);
    //   - `agent_env TEXT NOT NULL DEFAULT '{}'` (JSON object env map).
    // Wiring these into the provider EXEC is a SEPARATE bead (e38.16); this
    // migration only lands the columns the edit/archive RPCs write. ALTER TABLE
    // ADD COLUMN rewrites the catalog SQL, so each shows up in `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let agent = table_sql(&pool, "agent").await;
    assert!(
        agent.contains("archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))"),
        "agent.archived 0/1 flag with CHECK: {agent}"
    );
    assert!(
        agent.contains("model TEXT"),
        "agent.model nullable: {agent}"
    );
    assert!(
        agent.contains("cli_args TEXT NOT NULL DEFAULT '[]'"),
        "agent.cli_args default empty JSON array: {agent}"
    );
    assert!(
        agent.contains("mcp_config TEXT NOT NULL DEFAULT '{}'"),
        "agent.mcp_config default empty JSON object: {agent}"
    );
    assert!(
        agent.contains("thinking TEXT"),
        "agent.thinking nullable: {agent}"
    );
    assert!(
        agent.contains("agent_env TEXT NOT NULL DEFAULT '{}'"),
        "agent.agent_env default empty JSON object: {agent}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0016_creates_label_and_issue_label_tables() {
    // The issue-label surface (parity-review gap: "no label table or issue-label
    // join") needs a first-class workspace-scoped label entity plus an attach
    // join. Two tables land:
    //   - `label` (id PK, workspace_id FK, name, nullable color) with a
    //     `(workspace_id, name)` UNIQUE index (the resolve-or-reject guard);
    //   - `issue_label` (issue_id, label_id) composite-PK join (the attach link),
    //     with a reverse-lookup index on `label_id`.
    // The `issue.labels` JSON column (migration 0014) is kept as a denormalized
    // read-cache the attach/detach RPCs sync; the `label` table is the source of
    // truth. `CREATE TABLE`/`CREATE INDEX` are catalog-only, so each shows up in
    // `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let label = table_sql(&pool, "label").await;
    assert!(
        label.contains("id TEXT PRIMARY KEY"),
        "label.id is the PK: {label}"
    );
    assert!(
        label.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "label.workspace_id is a non-null workspace FK: {label}"
    );
    assert!(
        label.contains("name TEXT NOT NULL"),
        "label.name is non-null: {label}"
    );
    assert!(
        label.contains("color TEXT"),
        "label.color is a nullable hex color: {label}"
    );

    let issue_label = table_sql(&pool, "issue_label").await;
    assert!(
        issue_label.contains("issue_id TEXT NOT NULL REFERENCES issue(id)"),
        "issue_label.issue_id references issue: {issue_label}"
    );
    assert!(
        issue_label.contains("label_id TEXT NOT NULL REFERENCES label(id)"),
        "issue_label.label_id references label: {issue_label}"
    );
    assert!(
        issue_label.contains("PRIMARY KEY (issue_id, label_id)"),
        "issue_label has a composite PK: {issue_label}"
    );

    // The resolve-or-reject UNIQUE index makes `(workspace_id, name)` map to one
    // label per tenant.
    let idx: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type='index' AND name = 'idx_label_workspace_name'",
    )
    .fetch_optional(&pool)
    .await
    .expect("query index")
    .flatten();
    let idx = idx.expect("idx_label_workspace_name present");
    let idx = idx.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        idx.contains("UNIQUE INDEX idx_label_workspace_name ON label(workspace_id, name)"),
        "label name is unique per workspace: {idx}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0017_creates_squad_and_squad_member_tables() {
    // The squads surface (parity-review gap: "no squad construct — ActorKind was
    // only {Member, Agent}, no squad table") needs a first-class workspace-scoped
    // squad entity with a designated leader plus a membership join. Two tables
    // land:
    //   - `squad` (id PK, workspace_id FK, name, polymorphic leader actor-ref
    //     `leader_type`/`leader_id` with a `('member','agent')` CHECK, created_at)
    //     with a `(workspace_id, name)` UNIQUE index (the resolve-or-reject guard);
    //   - `squad_member` (squad_id FK, polymorphic member actor-ref) with a
    //     `(squad_id, member_type, member_id)` composite PK (the dedupe guard).
    // Leader routing reuses the leader actor-ref (no new ActorKind): a
    // squad-assigned task resolves to the leader's `agent_id` and rides the
    // existing claim/dispatch path. `CREATE TABLE`/`CREATE INDEX` are catalog-only,
    // so each shows up in `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let squad = table_sql(&pool, "squad").await;
    assert!(
        squad.contains("id TEXT PRIMARY KEY"),
        "squad.id is the PK: {squad}"
    );
    assert!(
        squad.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "squad.workspace_id is a non-null workspace FK: {squad}"
    );
    assert!(
        squad.contains("name TEXT NOT NULL"),
        "squad.name is non-null: {squad}"
    );
    assert!(
        squad.contains("leader_type TEXT NOT NULL CHECK (leader_type IN ('member','agent'))"),
        "squad.leader_type CHECK: {squad}"
    );
    assert!(
        squad.contains("leader_id TEXT NOT NULL"),
        "squad.leader_id is non-null: {squad}"
    );
    assert!(
        squad.contains("created_at INTEGER NOT NULL"),
        "squad.created_at epoch millis: {squad}"
    );

    let name_idx = index_sql(&pool, "idx_squad_workspace_name").await;
    assert!(
        name_idx.contains("UNIQUE"),
        "idx_squad_workspace_name is UNIQUE: {name_idx}"
    );
    assert!(
        name_idx.contains("squad") && name_idx.contains("(workspace_id, name)"),
        "idx_squad_workspace_name columns: {name_idx}"
    );

    let squad_member = table_sql(&pool, "squad_member").await;
    assert!(
        squad_member.contains("squad_id TEXT NOT NULL REFERENCES squad(id)"),
        "squad_member.squad_id references squad: {squad_member}"
    );
    assert!(
        squad_member
            .contains("member_type TEXT NOT NULL CHECK (member_type IN ('member','agent'))"),
        "squad_member.member_type CHECK: {squad_member}"
    );
    assert!(
        squad_member.contains("member_id TEXT NOT NULL"),
        "squad_member.member_id is non-null: {squad_member}"
    );
    assert!(
        squad_member.contains("PRIMARY KEY (squad_id, member_type, member_id)"),
        "squad_member has a composite PK: {squad_member}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0019_adds_autopilot_execution_mode_and_concurrency_policy_columns() {
    // The autopilot execution-mode + concurrency-policy surface (parity-review
    // gap: "only skip-when-running implemented; queue/replace collapsed; fire path
    // hardcodes issue_id=NULL") needs the `autopilot` table to carry two config
    // discriminants:
    //   - `execution_mode TEXT NOT NULL DEFAULT 'run_only'
    //      CHECK (execution_mode IN ('create_issue', 'run_only'))` — what the fire
    //     path materialises (run_only = the v1 issue_id=NULL task; create_issue =
    //     an issue + a task against it);
    //   - `concurrency_policy TEXT NOT NULL DEFAULT 'skip'
    //      CHECK (concurrency_policy IN ('skip', 'queue', 'replace'))` — what the
    //     scheduler does when a tick comes due at the in-flight limit.
    // Both default to the v1 behaviour (run_only + skip). ALTER TABLE ADD COLUMN
    // rewrites the catalog SQL, so each shows up in `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let ap = table_sql(&pool, "autopilot").await;
    assert!(
        ap.contains(
            "execution_mode TEXT NOT NULL DEFAULT 'run_only' CHECK (execution_mode IN \
             ('create_issue', 'run_only'))"
        ),
        "autopilot.execution_mode default run_only + CHECK: {ap}"
    );
    assert!(
        ap.contains(
            "concurrency_policy TEXT NOT NULL DEFAULT 'skip' CHECK (concurrency_policy IN \
             ('skip', 'queue', 'replace'))"
        ),
        "autopilot.concurrency_policy default skip + CHECK: {ap}"
    );

    pool.close().await;
}

#[tokio::test]
async fn all_migrations_preserve_every_core_table() {
    // Guards against a migration DROPPING or renaming a core table. This used
    // to assert an exact table count, which broke on every PR that added a
    // migration (the multica-parity work adds new tables continuously): the
    // exact count was never the point, catching a lost table is. So this
    // checks that every table in `expected` still exists by name (a floor,
    // not a ceiling): new tables are fine, a missing one fails loudly.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type='table' AND name NOT LIKE 'sqlite_%' \
           AND name <> '_sqlx_migrations' \
         ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("table list");

    let expected = [
        "agent",
        "agent_runtime",
        "agent_skill",
        "agent_task_queue",
        "atc_instance",
        "atc_retry",
        "attention",
        "autopilot",
        "autopilot_run",
        "autopilot_webhook_delivery",
        "beads_mapping",
        // P4 / D8: user-defined kanban boards (migration 0027).
        "board",
        "board_card",
        "board_column",
        // tcp T4 / F7: beads-style card dependency edges (migration 0036).
        "card_dependency",
        "comment",
        "daemon_config",
        "daemon_socket_token",
        "daemon_token",
        "event_log",
        "inbox_entry",
        "issue",
        "issue_label",
        "label",
        "member",
        // tcp T5: per-attention-kind notification routing rules (migration 0037).
        "notify_rule",
        "pat",
        // P5 / D14-D16: the fs-watch-maintained INDEX over the on-disk agent
        // profile masters (migration 0030). The bodies live on disk; this table
        // is only the index.
        "profile",
        // P10 / D19: durable per-run observability history (migration 0029). The
        // sibling `cost_rollup` is a VIEW, not a table, so it is not counted here.
        "run_history",
        "skill",
        "skill_file",
        "squad",
        "squad_member",
        "standup_session",
        "task_usage",
        "user",
        "workspace",
    ];
    // Floor, not a frozen ceiling: new migrations are free to add tables, but
    // the table count must never drop below the known core set.
    assert!(
        names.len() >= expected.len(),
        "expected at least {} core tables, got {} ({names:?})",
        expected.len(),
        names.len()
    );
    for table in expected {
        assert!(
            names.iter().any(|n| n == table),
            "missing table {table} in {names:?}"
        );
    }

    pool.close().await;
}

#[tokio::test]
async fn migration_0035_adds_issue_squad_id_column() {
    // tcp T4 / F7: a card's assignee can be a whole SQUAD, persisted as a nullable
    // `issue.squad_id` (NULL = single-agent run; set = fan out across the squad).
    // ALTER TABLE ADD COLUMN with no default rewrites the catalog SQL, so the new
    // column shows up in `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let issue = table_sql(&pool, "issue").await;
    assert!(
        issue.contains("squad_id TEXT"),
        "issue.squad_id nullable squad ref: {issue}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0036_creates_card_dependency_and_auto_run() {
    // tcp T4 / F7: beads-style card dependencies. A `card_dependency` edge table
    // (dependent -> blocker, composite PK, blocker reverse-lookup index) plus a
    // per-card `issue.auto_run` flag (default OFF) governing auto-launch when the
    // last blocker completes. `CREATE TABLE`/`CREATE INDEX` and the defaulted
    // ALTER are catalog-only, so each shows up in `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let dep = table_sql(&pool, "card_dependency").await;
    assert!(
        dep.contains("workspace_id TEXT NOT NULL REFERENCES workspace(id)"),
        "card_dependency.workspace_id FK: {dep}"
    );
    assert!(
        dep.contains("dependent_issue_id TEXT NOT NULL"),
        "card_dependency.dependent_issue_id: {dep}"
    );
    assert!(
        dep.contains("blocker_issue_id TEXT NOT NULL"),
        "card_dependency.blocker_issue_id: {dep}"
    );
    assert!(
        dep.contains("PRIMARY KEY (dependent_issue_id, blocker_issue_id)"),
        "card_dependency composite PK (a card depends on another at most once): {dep}"
    );

    let idx = index_sql(&pool, "idx_card_dependency_blocker").await;
    assert!(
        idx.contains("card_dependency") && idx.contains("(blocker_issue_id)"),
        "idx_card_dependency_blocker serves the finalize-seam reverse lookup: {idx}"
    );

    let issue = table_sql(&pool, "issue").await;
    assert!(
        issue.contains("auto_run INTEGER NOT NULL DEFAULT 0"),
        "issue.auto_run default OFF (explicit run stays the default): {issue}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0020_adds_workspace_config_columns() {
    // The per-workspace config surface (parity-review gap: "per-workspace context
    // prompt + repo whitelist + issue prefix") needs the `workspace` table to
    // carry three nullable config columns:
    //   - `context_prompt TEXT` — injected into a task's execenv as a `CLAUDE.md`;
    //   - `repo_whitelist TEXT` — a JSON array of allowed repos (the checkout gate);
    //   - `issue_prefix TEXT`   — prepended to a newly-created issue's title.
    // All three default to NULL ("not configured"), so an upgrading workspace
    // keeps the exact pre-0020 behaviour. ALTER TABLE ADD COLUMN rewrites the
    // catalog SQL, so each shows up in `sqlite_master`.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let ws = table_sql(&pool, "workspace").await;
    assert!(
        ws.contains("context_prompt TEXT"),
        "workspace.context_prompt nullable TEXT: {ws}"
    );
    assert!(
        ws.contains("repo_whitelist TEXT"),
        "workspace.repo_whitelist nullable TEXT: {ws}"
    );
    assert!(
        ws.contains("issue_prefix TEXT"),
        "workspace.issue_prefix nullable TEXT: {ws}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0031_adds_task_mode_and_session_name_columns() {
    // ccc / D6: a board card launches `headless` or `interactive`; the runner
    // needs the mode on the row and, for interactive, records the exact tmux
    // session name it spawned. Two columns land on `agent_task_queue`:
    //   - `mode TEXT NOT NULL DEFAULT 'headless'
    //      CHECK (mode IN ('headless', 'interactive'))` — defaults to the
    //     pre-0031 headless behaviour;
    //   - `session_name TEXT` — the interactive tmux session name (nullable;
    //     NULL for a headless task or one not yet dispatched).
    // ALTER TABLE ADD COLUMN rewrites the catalog SQL, so each shows up in
    // `sqlite_master` like the originals.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let tq = table_sql(&pool, "agent_task_queue").await;
    assert!(
        tq.contains(
            "mode TEXT NOT NULL DEFAULT 'headless' CHECK (mode IN ('headless', 'interactive'))"
        ),
        "agent_task_queue.mode default headless + CHECK: {tq}"
    );
    assert!(
        tq.contains("session_name TEXT"),
        "agent_task_queue.session_name nullable TEXT: {tq}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0025_creates_attention_with_kind_state_checks_and_open_index() {
    // The control-plane inbox (architecture §4.3, spec P2): every input request
    // from every session lands in `attention`, answered exactly once via the
    // first-answer-wins conditional flip. The columns the answer router and the
    // surfaces depend on must exist with their CHECK guards, and the hot "open"
    // read must be served by a partial index.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let att = table_sql(&pool, "attention").await;
    assert!(
        att.contains("id TEXT PRIMARY KEY"),
        "attention.id PK: {att}"
    );
    assert!(
        att.contains("session_id TEXT NOT NULL"),
        "attention.session_id NOT NULL: {att}"
    );
    // cwd carries the raising session's dir so the answer router runs the C1
    // guard without re-discovery.
    assert!(
        att.contains("cwd TEXT NOT NULL DEFAULT ''"),
        "attention.cwd NOT NULL default empty: {att}"
    );
    // workspace_id is a NULLABLE FK — fleet (no-workspace) sessions insert freely.
    assert!(
        att.contains("workspace_id TEXT REFERENCES workspace(id)"),
        "attention.workspace_id nullable FK: {att}"
    );
    // kind is CHECK-constrained to the six spec families.
    for k in [
        "ask_user_question",
        "approval",
        "codex_request_user",
        "error",
        "waiting",
        "escalation",
    ] {
        assert!(att.contains(k), "attention.kind CHECK includes {k}: {att}");
    }
    // state defaults to open + CHECK-guards the two-value set.
    assert!(
        att.contains("state        TEXT NOT NULL DEFAULT 'open'")
            || att.contains("state TEXT NOT NULL DEFAULT 'open'"),
        "attention.state defaults open: {att}"
    );
    assert!(
        att.contains("state IN ('open', 'answered')"),
        "attention.state CHECK: {att}"
    );
    assert!(
        att.contains("degraded"),
        "attention.degraded pane-fallback flag: {att}"
    );

    // The hot read is exclusively open rows — a PARTIAL index over just that set.
    let open_idx = index_sql(&pool, "idx_attention_open").await;
    assert!(
        open_idx.contains("WHERE") && open_idx.contains("state = 'open'"),
        "idx_attention_open is partial over open rows: {open_idx}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0028_creates_atc_standup_and_daemon_config_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    // atc_instance: name PK + heartbeat_cron + the retry cap moved off the JSON.
    let atc = table_sql(&pool, "atc_instance").await;
    assert!(
        atc.contains("name TEXT PRIMARY KEY"),
        "atc_instance.name PK: {atc}"
    );
    assert!(
        atc.contains("heartbeat_cron TEXT NOT NULL"),
        "heartbeat_cron: {atc}"
    );
    assert!(
        atc.contains("err_retry_cap INTEGER NOT NULL DEFAULT 3"),
        "err_retry_cap: {atc}"
    );
    assert!(
        atc.contains("enabled IN (0, 1)"),
        "atc_instance.enabled CHECK: {atc}"
    );
    // The heartbeat cron's hot path is a PARTIAL index over enabled rows.
    let idx = index_sql(&pool, "idx_atc_instance_next_tick").await;
    assert!(
        idx.contains("WHERE") && idx.contains("enabled = 1"),
        "idx_atc_instance_next_tick is partial over enabled rows: {idx}"
    );

    // atc_retry: composite (instance, session) PK + escalated CHECK.
    let retry = table_sql(&pool, "atc_retry").await;
    assert!(
        retry.contains("PRIMARY KEY (instance_name, session_id)"),
        "atc_retry composite PK: {retry}"
    );
    assert!(
        retry.contains("escalated IN (0, 1)"),
        "atc_retry.escalated CHECK: {retry}"
    );

    // standup_session: per-session opt-out + cooldown anchor + in-flight marker.
    let su = table_sql(&pool, "standup_session").await;
    assert!(
        su.contains("session_id TEXT PRIMARY KEY"),
        "standup_session.session_id PK: {su}"
    );
    assert!(
        su.contains("opted_out INTEGER NOT NULL DEFAULT 0"),
        "opted_out default: {su}"
    );
    assert!(
        su.contains("in_flight INTEGER NOT NULL DEFAULT 0"),
        "in_flight default: {su}"
    );
    // The max-concurrent guardrail counts in-flight rows — a PARTIAL index serves it.
    let su_idx = index_sql(&pool, "idx_standup_in_flight").await;
    assert!(
        su_idx.contains("WHERE") && su_idx.contains("in_flight = 1"),
        "idx_standup_in_flight is partial over in-flight rows: {su_idx}"
    );

    // daemon_config: a generic string kv for host-wide daemon knobs.
    let cfg = table_sql(&pool, "daemon_config").await;
    assert!(
        cfg.contains("key TEXT PRIMARY KEY"),
        "daemon_config.key PK: {cfg}"
    );
    assert!(
        cfg.contains("value TEXT NOT NULL"),
        "daemon_config.value: {cfg}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0034_adds_board_card_ord_defaulting_zero() {
    // tcp T3 / F6: cards reorder WITHIN a column, which needs a mutable per-card
    // position. Migration 0034 adds `board_card.ord` (NOT NULL DEFAULT 0), so an
    // upgrading board keeps the pre-0034 `(added_at, issue_id)` order until the
    // user reorders. ALTER TABLE ADD COLUMN rewrites the catalog SQL, so the new
    // column shows up in `sqlite_master` like the originals.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let bc = table_sql(&pool, "board_card").await;
    assert!(
        bc.contains("ord INTEGER NOT NULL DEFAULT 0"),
        "board_card.ord NOT NULL default 0: {bc}"
    );
    // The stable card identity a reorder keys off is still the (board_id, issue_id)
    // composite PK — no surrogate id was added.
    assert!(
        bc.contains("PRIMARY KEY (board_id, issue_id)"),
        "board_card keeps its (board_id, issue_id) PK: {bc}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0047_adds_permission_mode_and_invocation_target() {
    // Gap #8 / multica 130: agent invocation-permission. `agent.permission_mode`
    // is the authoritative deny-by-default invoke source; `agent_invocation_target`
    // is the FK-less allow-list. A fresh DB (no legacy rows) has the column with a
    // 'private' default and an empty target table.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let agent = table_sql(&pool, "agent").await;
    assert!(
        agent.contains(
            "permission_mode TEXT NOT NULL DEFAULT 'private' CHECK (permission_mode IN ('private', 'public_to'))"
        ),
        "agent.permission_mode column + CHECK: {agent}"
    );

    let target = table_sql(&pool, "agent_invocation_target").await;
    assert!(
        target.contains("id TEXT PRIMARY KEY"),
        "agent_invocation_target.id PK: {target}"
    );
    assert!(
        target.contains("agent_id TEXT NOT NULL"),
        "agent_invocation_target.agent_id NOT NULL (FK-less): {target}"
    );
    assert!(
        target.contains(
            "target_type TEXT NOT NULL CHECK (target_type IN ('workspace', 'member', 'team'))"
        ),
        "agent_invocation_target.target_type CHECK: {target}"
    );
    assert!(
        target.contains("UNIQUE (agent_id, target_type, target_id)"),
        "agent_invocation_target dedup UNIQUE: {target}"
    );
    // No FK on agent_id (matches the (actor_type, actor_id) FK-less convention).
    assert!(
        !target.contains("REFERENCES"),
        "agent_invocation_target is FK-less by design: {target}"
    );

    pool.close().await;
}

#[tokio::test]
async fn migration_0055_types_card_dependency_links() {
    // multica parity #20: `issue_dependency.type IN ('blocks','blocked_by','related')`.
    // hangar's row IS the blocked_by relation, so 0055 adds the KIND column with a
    // constant DEFAULT 'blocked_by' — every pre-0055 row backfills to today's
    // semantics with no table rewrite — plus the (workspace_id, link_type) index the
    // typed graph read uses. 0036 is UNTOUCHED (an applied migration is immutable).
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = fresh_pool(dir.path()).await;

    let cols = sqlx::query("PRAGMA table_info(card_dependency)")
        .fetch_all(&pool)
        .await
        .expect("table_info");
    let link_type = cols
        .iter()
        .find(|r| {
            let name: String = r.get("name");
            name == "link_type"
        })
        .expect("card_dependency.link_type exists");
    let notnull: i64 = link_type.get("notnull");
    let dflt: Option<String> = link_type.get("dflt_value");
    assert_eq!(notnull, 1, "link_type is NOT NULL");
    assert_eq!(
        dflt.as_deref().map(|d| d.trim_matches('\'')),
        Some("blocked_by"),
        "the constant default backfills every pre-0055 row as a gating edge"
    );

    let idx = index_sql(&pool, "idx_card_dependency_ws_type").await;
    assert!(
        idx.contains("card_dependency") && idx.contains("link_type"),
        "idx_card_dependency_ws_type serves the typed graph read: {idx}"
    );

    pool.close().await;
}
