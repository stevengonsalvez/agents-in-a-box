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
        "agent.runtime_id FK (required by Multica pattern): {agent}"
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
    // per-(issue, agent) scope (Multica ClaimAgentTask parity), so the final
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
    // the claim ordering `ORDER BY priority DESC, created_at, id` (Multica
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
async fn all_migrations_create_exactly_seventeen_tables() {
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
        "autopilot",
        "autopilot_run",
        "beads_mapping",
        "comment",
        "daemon_socket_token",
        "daemon_token",
        "issue",
        "member",
        "pat",
        "skill",
        "skill_file",
        "user",
        "workspace",
    ];
    assert_eq!(names.len(), 17, "expected 17 v1 tables, got {names:?}");
    for table in expected {
        assert!(
            names.iter().any(|n| n == table),
            "missing table {table} in {names:?}"
        );
    }

    pool.close().await;
}
