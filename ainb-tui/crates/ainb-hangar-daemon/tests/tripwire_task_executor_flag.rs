//! Spine A5 + A8 tripwire: the executor is selected in BOTH directions by the
//! daemon-wide `HANGAR_TASK_EXECUTOR` and, per agent, by `agent.task_executor`;
//! the ACP path's per-task isolation actually holds.
//!
//! ```text
//!   =process ─▶ spawns the provider CLI   ─▶ marker + logs/claude.jsonl
//!               and NO acp session
//!   =acp     ─▶ spawns an ACP adapter     ─▶ fleet_acp_session + provider events
//!               and NO process, no jsonl
//!
//!   A8: ONE daemon, two agents, opposite executors
//!   =process + agent.task_executor='acp'      ─▶ adapter
//!   =process + agent.task_executor  IS NULL   ─▶ provider CLI  (unchanged)
//!   =acp     + agent.task_executor='process'  ─▶ provider CLI
//! ```
//!
//! Both directions are asserted because a flag that silently fell back to the
//! process executor would keep every other test in the suite green: the task
//! still reaches `done`, the card still advances, and only the absence of a
//! thing nobody looks for gives it away.
//!
//! The other two cases are the ones the ACP path can get quietly wrong:
//! `mode=interactive` under `=acp` must be REFUSED rather than downgraded to a
//! headless run in a pane nobody can attach to, and each task's adapter is its
//! OWN process, so one task's `agent_env` must never reach another's, nor the
//! daemon's ambient environment reach either.
//!
//! Skips cleanly (never fails) when tmux is unavailable.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tripwire_support::{
    DaemonSession, daemon_bin, enqueue_task, fake_acp_adapter, fake_claude_happy,
    seed_agent_with_env, seed_world, wait_for_db, wait_for_file, write_acp_adapter_config,
};

/// A secret only the DAEMON's environment carries. No child of either executor
/// may see it: it stands in for every ambient credential the daemon holds.
const AMBIENT_SECRET: &str = "AINB_PLANTED_SECRET";

/// `=process` takes the provider CLI and creates no ACP session.
#[tokio::test]
async fn the_process_executor_spawns_a_provider_and_no_acp_session() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping executor-flag tripwire");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let agent = seed_agent_with_env(&pool, &ids, "agent-process", &serde_json::json!({})).await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "process");
    let task_id = "task-flag-process";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    let row = wait_for_db(&pool, task_id, "done", Duration::from_mins(1)).await;
    drop(session);

    // POSITIVE: a real provider process ran and left its jsonl transcript.
    assert_eq!(
        row.get::<Option<String>, _>("session_id").as_deref(),
        Some("claude-sid"),
        "the process executor pins the provider's own session id"
    );
    let logs = logs_dir(&row);
    assert!(
        logs.join("claude.jsonl").exists(),
        "the process executor must write logs/claude.jsonl into {}",
        logs.display()
    );

    // NEGATIVE: nothing touched the ACP path.
    assert_eq!(
        acp_sessions_for(&pool, task_id).await,
        0,
        "no acp session under =process"
    );
}

/// `=acp` takes the adapter and spawns no provider process.
#[tokio::test]
async fn the_acp_executor_spawns_an_adapter_and_no_provider_process() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping executor-flag tripwire");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let rpc_log = home.path().join("rpc.log");
    let agent = seed_agent_with_env(
        &pool,
        &ids,
        "agent-acp-flag",
        &serde_json::json!({ "FAKE_ACP_RPC_LOG": rpc_log.display().to_string() }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    // The SAME fake claude the process case proves works, so "no jsonl" here is
    // the flag's doing rather than a provider that could not run anyway.
    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "acp");
    let task_id = "task-flag-acp";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    let row = wait_for_db(&pool, task_id, "done", Duration::from_mins(1)).await;
    drop(session);

    // POSITIVE: an adapter ran and the transcript went to the store.
    assert_eq!(
        acp_sessions_for(&pool, task_id).await,
        1,
        "one acp session under =acp"
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM fleet_provider_event e \
         JOIN fleet_acp_session s ON s.session_key = e.session_key WHERE s.scope_key = ?",
    )
    .bind(format!("task:{task_id}"))
    .fetch_one(&pool)
    .await
    .expect("count provider events");
    assert!(events > 0, "the acp run must write provider events");
    assert!(
        rpc_log.exists(),
        "the fixture adapter must have been spawned"
    );

    // NEGATIVE: the provider CLI never ran, so its transcript is absent and its
    // session id never reached the row.
    let logs = logs_dir(&row);
    assert!(
        !logs.join("claude.jsonl").exists(),
        "the acp executor must write NO provider jsonl into {}",
        logs.display()
    );
    assert_ne!(
        row.get::<Option<String>, _>("session_id").as_deref(),
        Some("claude-sid"),
        "the acp run must not be attributed to a provider session it never opened"
    );
}

/// `mode=interactive` under `=acp` is refused at dispatch, naming the flag, and
/// opens no pane.
///
/// There is no attachable session on the ACP path, so a silent downgrade would
/// hand an operator a headless run they asked to drive. Refusing BEFORE the
/// worktree is provisioned is what makes "no pane" and "no checkout" both true.
#[tokio::test]
async fn interactive_under_acp_is_refused_and_opens_no_pane() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping interactive-under-acp refusal");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let agent = seed_agent_with_env(&pool, &ids, "agent-interactive", &serde_json::json!({})).await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "acp");
    let task_id = "task-flag-interactive";
    enqueue_task(&pool, &ids, task_id, &agent, "interactive").await;

    let row = wait_for_db(&pool, task_id, "failed", Duration::from_mins(1)).await;
    drop(session);

    assert_eq!(
        row.get::<Option<String>, _>("failure_reason").as_deref(),
        Some("provision_error"),
        "an unsupported mode is a provisioning refusal, not an agent error"
    );
    let result: String = row.get::<Option<String>, _>("result").unwrap_or_default();
    assert!(
        result.contains("HANGAR_TASK_EXECUTOR"),
        "the refusal must name the flag that caused it, got: {result}"
    );
    let pane = format!("tmux_hangar-{task_id}");
    assert!(
        !tripwire_support::tmux_session_live(&pane),
        "a refused interactive task must open no pane, found {pane}"
    );
    assert!(
        row.get::<Option<String>, _>("work_dir").is_none(),
        "the refusal must land before the worktree is provisioned"
    );
    assert_eq!(
        acp_sessions_for(&pool, task_id).await,
        0,
        "and before any acp session"
    );
}

/// Each task's adapter is its own PROCESS, so each sees only its own
/// `agent_env` and neither sees the daemon's ambient secrets.
#[tokio::test]
async fn each_tasks_adapter_sees_only_its_own_environment() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp env isolation");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    let dump_a = home.path().join("env-a.json");
    let dump_b = home.path().join("env-b.json");
    let agent_a = seed_agent_with_env(
        &pool,
        &ids,
        "agent-tenant-a",
        &serde_json::json!({
            "FAKE_ACP_ENV_DUMP": dump_a.display().to_string(),
            "TENANT_TOKEN": "tenant-a-secret",
        }),
    )
    .await;
    let agent_b = seed_agent_with_env(
        &pool,
        &ids,
        "agent-tenant-b",
        &serde_json::json!({
            "FAKE_ACP_ENV_DUMP": dump_b.display().to_string(),
            "TENANT_TOKEN": "tenant-b-secret",
        }),
    )
    .await;

    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "acp");
    enqueue_task(&pool, &ids, "task-tenant-a", &agent_a, "headless").await;
    enqueue_task(&pool, &ids, "task-tenant-b", &agent_b, "headless").await;

    wait_for_db(&pool, "task-tenant-a", "done", Duration::from_secs(90)).await;
    wait_for_db(&pool, "task-tenant-b", "done", Duration::from_secs(90)).await;
    // Written by the adapter's first line of `main`, so it exists by the time
    // its task is done; polled anyway rather than assumed.
    let a = wait_for_file(&dump_a, Duration::from_secs(10), |raw| !raw.is_empty());
    let b = wait_for_file(&dump_b, Duration::from_secs(10), |raw| !raw.is_empty());
    drop(session);

    let a: serde_json::Value = serde_json::from_str(&a).expect("env dump a");
    let b: serde_json::Value = serde_json::from_str(&b).expect("env dump b");

    // Each adapter GOT its own agent's value: the per-agent escape reaches a
    // per-task adapter process at all.
    assert_eq!(a["TENANT_TOKEN"], "tenant-a-secret");
    assert_eq!(b["TENANT_TOKEN"], "tenant-b-secret");
    // And NOT the other tenant's, which a shared pool process would have leaked.
    let a_raw = a.to_string();
    let b_raw = b.to_string();
    assert!(
        !a_raw.contains("tenant-b-secret"),
        "task A saw task B's env: {a_raw}"
    );
    assert!(
        !b_raw.contains("tenant-a-secret"),
        "task B saw task A's env: {b_raw}"
    );
    // Nor the daemon's own ambient environment.
    for (name, raw) in [("a", &a_raw), ("b", &b_raw)] {
        assert!(
            !raw.contains("planted-daemon-secret"),
            "adapter {name} saw the daemon's ambient secret: {raw}"
        );
        assert!(
            a[AMBIENT_SECRET].is_null(),
            "adapter {name} must not carry {AMBIENT_SECRET}"
        );
    }
    // Pointed at the task's own config tree, so it never merges the operator's
    // `~/.claude` (whose `settings.json` can refuse `session/new` outright).
    let config_dir = a["CLAUDE_CONFIG_DIR"].as_str().expect("CLAUDE_CONFIG_DIR is set");
    assert!(
        config_dir.contains("task-tenant-a"),
        "the config dir must be the task's own, got {config_dir}"
    );
    assert_ne!(
        a["CLAUDE_CONFIG_DIR"], b["CLAUDE_CONFIG_DIR"],
        "two tasks must not share one adapter config tree"
    );
}

/// A8: on ONE daemon defaulted to `process`, an agent that asks for `acp` gets
/// the adapter and an agent that asks for nothing gets the provider CLI.
///
/// The whole point of per-agent selection is that these two coexist, so both
/// halves are asserted against the SAME running daemon rather than two runs
/// that happen to differ. The inheriting agent is the regression half: it must
/// dispatch exactly as it did before the column existed, and it would still
/// reach `done` if per-agent selection leaked onto it — only the absence of an
/// ACP session and the presence of the provider's jsonl tell the two apart.
#[tokio::test]
async fn a_per_agent_acp_override_runs_beside_an_inheriting_agent() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping per-agent executor tripwire");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let rpc_log = home.path().join("rpc.log");
    let overrider = seed_agent_with_env(
        &pool,
        &ids,
        "agent-wants-acp",
        &serde_json::json!({ "FAKE_ACP_RPC_LOG": rpc_log.display().to_string() }),
    )
    .await;
    set_agent_executor(&pool, &overrider, "acp").await;
    // No `task_executor` at all: every agent that predates migration 0095.
    let inheritor =
        seed_agent_with_env(&pool, &ids, "agent-inherits", &serde_json::json!({})).await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    // The daemon default is `process`, so ONLY the agent's own column can move
    // its task onto the adapter.
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "process");
    enqueue_task(&pool, &ids, "task-agent-acp", &overrider, "headless").await;
    enqueue_task(&pool, &ids, "task-agent-inherits", &inheritor, "headless").await;

    let acp_row = wait_for_db(&pool, "task-agent-acp", "done", Duration::from_mins(1)).await;
    let inherit_row =
        wait_for_db(&pool, "task-agent-inherits", "done", Duration::from_mins(1)).await;
    drop(session);

    // The overriding agent took the ACP path on a `process` daemon.
    assert_eq!(
        acp_sessions_for(&pool, "task-agent-acp").await,
        1,
        "agent.task_executor='acp' must run on the adapter even though the daemon defaults \
         to process"
    );
    assert!(
        rpc_log.exists(),
        "the fixture adapter must have been spawned for the overriding agent"
    );
    assert!(
        !logs_dir(&acp_row).join("claude.jsonl").exists(),
        "the overriding agent must spawn no provider CLI"
    );

    // The inheriting agent is untouched: provider CLI, its jsonl, its session id.
    assert_eq!(
        acp_sessions_for(&pool, "task-agent-inherits").await,
        0,
        "an agent with no task_executor must NOT be moved onto the adapter"
    );
    assert!(
        logs_dir(&inherit_row).join("claude.jsonl").exists(),
        "the inheriting agent must still write the provider's jsonl transcript"
    );
    assert_eq!(
        inherit_row.get::<Option<String>, _>("session_id").as_deref(),
        Some("claude-sid"),
        "and still pin the provider's own session id"
    );
}

/// A8, the other direction: on a daemon defaulted to `acp`, an agent that pins
/// `process` runs the provider CLI, and its INTERACTIVE task is not refused.
///
/// Both halves are the same bug seen twice. If anything downstream of
/// resolution still reads the daemon-wide value, this agent runs on the adapter
/// it opted out of; if the interactive refusal reads it, an operator who
/// deliberately kept an attachable agent on an `acp` daemon loses every session
/// they try to drive.
#[tokio::test]
async fn a_per_agent_process_override_opts_out_of_an_acp_daemon() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping per-agent opt-out tripwire");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let agent =
        seed_agent_with_env(&pool, &ids, "agent-pins-process", &serde_json::json!({})).await;
    set_agent_executor(&pool, &agent, "process").await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "acp");
    enqueue_task(&pool, &ids, "task-pinned-process", &agent, "headless").await;
    enqueue_task(
        &pool,
        &ids,
        "task-pinned-interactive",
        &agent,
        "interactive",
    )
    .await;

    let row = wait_for_db(&pool, "task-pinned-process", "done", Duration::from_mins(1)).await;
    let interactive = tripwire_support::wait_until(
        &pool,
        "task-pinned-interactive",
        Duration::from_mins(1),
        |row| {
            matches!(
                row.get::<String, _>("status").as_str(),
                "done" | "failed" | "cancelled"
            )
        },
    )
    .await;
    drop(session);

    assert_eq!(
        acp_sessions_for(&pool, "task-pinned-process").await,
        0,
        "agent.task_executor='process' must keep its tasks off the adapter on an acp daemon"
    );
    assert!(
        logs_dir(&row).join("claude.jsonl").exists(),
        "and must spawn the provider CLI, which writes its jsonl transcript"
    );

    // The refusal follows the TASK's executor, so it must not fire here. Asserted
    // on the message rather than on the status: this agent's interactive run may
    // end any number of ways in a test tmux, but "refused for being acp" is the
    // one outcome that would mean the refusal read the daemon default.
    let detail: String = interactive.get::<Option<String>, _>("result").unwrap_or_default();
    assert!(
        !detail.contains("mode=interactive is not supported"),
        "an agent pinned to the process executor must not be refused an interactive run: \
         {detail}"
    );
}

/// A8: the interactive refusal fires for an agent that selected `acp` ITSELF,
/// on a daemon whose default is `process`.
///
/// The A5 case above proves the refusal under the flag. This proves it follows
/// the agent, which is the only way it can still be true once the flag stops
/// deciding: the task is refused before a pane or a worktree exists, exactly as
/// under the flag.
#[tokio::test]
async fn interactive_is_refused_for_an_agent_that_selected_acp() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping per-agent interactive refusal");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let agent =
        seed_agent_with_env(&pool, &ids, "agent-acp-interactive", &serde_json::json!({})).await;
    set_agent_executor(&pool, &agent, "acp").await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    let fake_claude = fake_claude_happy(home.path(), "claude-sid");
    let session = spawn_daemon(home.path(), &ids, &fake_claude, "process");
    let task_id = "task-agent-acp-interactive";
    enqueue_task(&pool, &ids, task_id, &agent, "interactive").await;

    let row = wait_for_db(&pool, task_id, "failed", Duration::from_mins(1)).await;
    drop(session);

    assert_eq!(
        row.get::<Option<String>, _>("failure_reason").as_deref(),
        Some("provision_error"),
        "an unsupported mode is a provisioning refusal, not an agent error"
    );
    let result: String = row.get::<Option<String>, _>("result").unwrap_or_default();
    assert!(
        result.contains("task_executor"),
        "the refusal must name the per-agent setting that caused it, got: {result}"
    );
    let pane = format!("tmux_hangar-{task_id}");
    assert!(
        !tripwire_support::tmux_session_live(&pane),
        "a refused interactive task must open no pane, found {pane}"
    );
    assert!(
        row.get::<Option<String>, _>("work_dir").is_none(),
        "the refusal must land before the worktree is provisioned"
    );
    assert_eq!(
        acp_sessions_for(&pool, task_id).await,
        0,
        "and before any acp session"
    );
}

// ------------------------------------------------------------------ helpers

/// Record a per-agent executor override on an already-seeded agent (migration
/// 0095), the way `ainb hangar agent create --executor` does at create time.
async fn set_agent_executor(pool: &SqlitePool, agent_id: &str, executor: &str) {
    let updated = sqlx::query("UPDATE agent SET task_executor = ? WHERE id = ?")
        .bind(executor)
        .bind(agent_id)
        .execute(pool)
        .await
        .expect("set agent task_executor");
    assert_eq!(
        updated.rows_affected(),
        1,
        "the seeded agent {agent_id} must exist before its executor is pinned"
    );
}

fn spawn_daemon(
    home: &Path,
    ids: &tripwire_support::SeededIds,
    fake_claude: &Path,
    executor: &str,
) -> DaemonSession {
    let home_str = home.display().to_string();
    let claude = fake_claude.display().to_string();
    DaemonSession::spawn(
        &daemon_bin(),
        home,
        &[
            ("AINB_HANGAR_HOME", &home_str),
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", executor),
            ("HANGAR_CLAUDE_PATH", &claude),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
            // The canary the child env must not carry.
            (AMBIENT_SECRET, "planted-daemon-secret"),
        ],
    )
}

/// How many ACP sessions exist under `task_id`'s scope.
async fn acp_sessions_for(pool: &SqlitePool, task_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM fleet_acp_session WHERE scope_key = ?")
        .bind(format!("task:{task_id}"))
        .fetch_one(pool)
        .await
        .expect("count acp sessions")
}

/// The run's `logs/` dir, beside the work dir the finalize recorded.
fn logs_dir(row: &sqlx::sqlite::SqliteRow) -> PathBuf {
    let work_dir: String = row.get::<Option<String>, _>("work_dir").expect("work_dir populated");
    Path::new(&work_dir).parent().expect("shortID root").join("logs")
}

async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
