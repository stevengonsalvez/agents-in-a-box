//! Spine A5 e2e tripwire: the real daemon binary runs a task over ACP.
//!
//! ```text
//!   HANGAR_TASK_EXECUTOR=acp
//!         │
//!   queued ─▶ dispatched ─▶ running ──▶ acp_task::run_acp
//!                                          │  register claude-agent-acp#task:<id>
//!                                          │  ensure(scope "task:<id>") ─▶ enqueue ─▶ prompt
//!                                          ▼
//!                                   poll the delivery leg
//!                                          ▼
//!                                        done   result = the agent's final message
//!   NEGATIVE, all the way through: no provider process, no *.jsonl, no tmux pane
//! ```
//!
//! Everything runs with no adapter and no credentials: the daemon's
//! `[acp.adapters.claude-agent-acp]` entry is repointed at
//! `ainb-acp`'s `fake_acp_adapter` fixture through the same config.toml an
//! operator edits, and the fixture is SCRIPTED through the agent's own
//! `agent_env`, which is also the proof that the per-agent environment reaches
//! a per-task adapter process at all.
//!
//! The negative assertions are the ones that stop a silent fallback: a flag
//! that quietly ran the process executor would leave every positive assertion
//! about the task row green.
//!
//! Skips cleanly (never fails) when tmux is unavailable.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::path::Path;
use std::time::{Duration, Instant};

use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tripwire_support::{
    DaemonSession, daemon_bin, enqueue_task, fake_acp_adapter, seed_agent_with_env, seed_world,
    wait_for_db, wait_for_file, write_acp_adapter_config,
};

/// A task run over ACP reaches `done`, carries the agent's final message, is
/// keyed to its ACP session, and leaves NO trace of the process executor.
#[tokio::test]
async fn an_acp_task_runs_to_done_with_no_process_and_no_tmux() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp task e2e");
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
        "agent-acp",
        &serde_json::json!({
            // The prompt comes back in the final agent message, so the task's
            // stored `result` is checkable against the brief that produced it.
            "FAKE_ACP_ECHO_PROMPT": "1",
            "FAKE_ACP_RPC_LOG": rpc_log.display().to_string(),
        }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    // A fake `claude` that leaves a MARKER if it is ever spawned. Nothing under
    // `executor=acp` should reach it; the marker is what makes "no process was
    // spawned" a positive observation instead of an absence of evidence.
    let marker = home.path().join("process-executor-ran");
    let fake_claude = write_marker_binary(home.path(), &marker);

    let session = spawn_acp_daemon(home.path(), &ids, &fake_claude);
    let task_id = "task-acp-happy";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    let row = wait_for_terminal(&pool, task_id, Duration::from_mins(1)).await;
    // The daemon writes its tracing to stderr, which is the tmux pane; capture
    // it BEFORE the session is dropped so a wrong outcome reports WHY.
    let pane = session.capture_pane();
    drop(session);
    assert_eq!(
        row.get::<String, _>("status"),
        "done",
        "the acp run must reach done; reason={:?}\n{}\ndaemon:\n{pane}",
        row.get::<Option<String>, _>("failure_reason"),
        dump_legs(&pool).await,
    );

    // The brief the daemon built from the agent's instructions, echoed back by
    // the adapter and stored as the run's result.
    let result: String = row.get::<Option<String>, _>("result").expect("result json");
    assert!(
        result.contains("do the agent-acp work"),
        "the task result must carry the turn's final agent message, got: {result}"
    );

    // The run is keyed to the ACP session created under its OWN scope, so the
    // transcript is reachable from the task row.
    let session_key: String =
        sqlx::query_scalar("SELECT session_key FROM fleet_acp_session WHERE scope_key = ?")
            .bind(format!("task:{task_id}"))
            .fetch_one(&pool)
            .await
            .expect("a session under the task scope");
    assert_eq!(
        row.get::<Option<String>, _>("session_id").as_deref(),
        Some(session_key.as_str()),
        "session_id must be the acp session key"
    );

    // The durable transcript: `fleet_provider_event`, not a jsonl file.
    let messages: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM fleet_provider_event \
         WHERE session_key = ? AND event_type = 'acp.message'",
    )
    .bind(&session_key)
    .fetch_one(&pool)
    .await
    .expect("count acp.message rows");
    assert!(
        messages > 0,
        "the run must write acp.message transcript rows"
    );

    // The adapter spawned once, opened one session and took one prompt.
    let log = std::fs::read_to_string(&rpc_log).expect("the fixture adapter's rpc log");
    assert_eq!(
        log.matches("spawn").count(),
        1,
        "one adapter process: {log}"
    );
    assert!(
        log.contains("prompt"),
        "the adapter must have taken a prompt: {log}"
    );

    assert_no_process_executor_trace(&row, &marker, task_id);
}

/// Spine A7: the `acp.usage` rows a run writes reach `task_usage`, the ledger
/// the usage dashboard rolls up.
///
/// Every hop is real: the fixture adapter emits two `usage_update`
/// notifications, the reducer classifies them, the store writer persists them
/// as `acp.usage` rows, and the finalize reads the LAST one back. Before A7 the
/// run finished with `usage: None` and this table stayed empty.
///
/// The script puts agent TEXT after the final report on purpose. That text is
/// what buries the accounting row, and it is why the read cannot be a windowed
/// tail scan.
#[tokio::test]
async fn an_acp_run_records_its_tokens_and_cost_in_the_usage_ledger() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp usage e2e");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // Two reports, because the LAST one is the run's answer: `used` is context
    // occupancy (not a delta) and `cost` is cumulative, so a reader that summed
    // the rows would bill 5 400 tokens and $0.05 for this turn.
    let script = home.path().join("turn.ndjson");
    std::fs::write(
        &script,
        concat!(
            r#"{"sessionUpdate":"usage_update","used":1200,"size":200000,"cost":{"amount":0.0125,"currency":"USD"}}"#,
            "\n",
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"working"}}"#,
            "\n",
            r#"{"sessionUpdate":"usage_update","used":4200,"size":200000,"cost":{"amount":0.0375,"currency":"USD"}}"#,
            "\n",
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":" and done"}}"#,
            "\n",
        ),
    )
    .expect("write the turn script");

    let agent = seed_agent_with_env(
        &pool,
        &ids,
        "agent-usage",
        &serde_json::json!({ "FAKE_ACP_SCRIPT": script.display().to_string() }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");
    let marker = home.path().join("process-executor-ran");
    let fake_claude = write_marker_binary(home.path(), &marker);

    let session = spawn_acp_daemon(home.path(), &ids, &fake_claude);
    let task_id = "task-acp-usage";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    let row = wait_for_terminal(&pool, task_id, Duration::from_mins(1)).await;
    let pane = session.capture_pane();
    drop(session);
    assert_eq!(
        row.get::<String, _>("status"),
        "done",
        "the scripted run must reach done; reason={:?}\n{}\ndaemon:\n{pane}",
        row.get::<Option<String>, _>("failure_reason"),
        dump_legs(&pool).await,
    );

    let usage = sqlx::query(
        "SELECT input_tokens, output_tokens, cost_usd FROM task_usage WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_optional(&pool)
    .await
    .expect("query task_usage")
    .expect("an acp run must record usage");

    assert_eq!(
        usage.get::<i64, _>("input_tokens"),
        4_200,
        "the LAST report is the run's context-token count, not the first and not their sum"
    );
    assert!(
        (usage.get::<f64, _>("cost_usd") - 0.0375).abs() < f64::EPSILON,
        "the cumulative session cost, got {}",
        usage.get::<f64, _>("cost_usd")
    );
    assert_eq!(
        usage.get::<i64, _>("output_tokens"),
        0,
        "ACP reports no completion tokens; this column stays honestly empty"
    );

    assert_no_process_executor_trace(&row, &marker, task_id);
}

/// A permission the agent raises lands as an approval row scoped to the task's
/// WORKSPACE, and answering it through `attention/answer` completes the turn.
///
/// The workspace id is the half that used to be missing: `raise_permission`
/// hardcoded `None`, so the row existed but no workspace-filtered surface (and
/// every operator surface is one) could show it.
#[tokio::test]
async fn a_task_raised_approval_is_workspace_scoped_and_answerable() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp approval e2e");
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
        "agent-approve",
        &serde_json::json!({
            "FAKE_ACP_PERMISSION_SESSIONS": "*",
            "FAKE_ACP_RPC_LOG": rpc_log.display().to_string(),
        }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");
    let marker = home.path().join("process-executor-ran");
    let fake_claude = write_marker_binary(home.path(), &marker);

    let session = spawn_acp_daemon(home.path(), &ids, &fake_claude);
    let task_id = "task-acp-approve";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    // The turn BLOCKS on the parked permission, so the row is observable while
    // the task is still `running`.
    let attention = wait_for_attention(&pool, Duration::from_mins(1)).await;
    let workspace_id: Option<String> = attention.get("workspace_id");
    assert_eq!(
        workspace_id.as_deref(),
        Some(ids.workspace_id.as_str()),
        "a task-raised approval must carry the task's workspace, or no filtered inbox shows it"
    );
    // Rooted in THIS run's directory, not the daemon's cwd. The task row's own
    // `work_dir` is still NULL here (it is written at finalize), which is
    // exactly why the approval has to carry the session's cwd itself.
    let cwd: String = attention.get::<Option<String>, _>("cwd").unwrap_or_default();
    assert!(
        cwd.contains(task_id) && cwd.ends_with("/workdir"),
        "the approval must be rooted in the run's own worktree, got {cwd:?}"
    );

    // Answer it the way Control Center does (`attention/answer`, not
    // `fleet/action`) and the turn completes.
    let attention_id: String = attention.get("id");
    let payload: String = attention.get("payload");
    let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    let option = payload["options"][0]["optionId"]
        .as_str()
        .expect("the adapter offered an option")
        .to_string();

    let mut client = Client::connect(&home.path().join("hangar.sock")).await;
    client.auth_from_file(home.path()).await;
    let resp = client
        .call(
            methods::ATTENTION_ANSWER,
            serde_json::json!({
                "attention_id": attention_id,
                "answer": option,
                "answered_by": "tripwire",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "attention/answer must ack: {resp}");

    let row = wait_for_db(&pool, task_id, "done", Duration::from_mins(1)).await;
    drop(session);

    let answered: Option<i64> =
        sqlx::query_scalar("SELECT answered_at FROM attention WHERE id = ?")
            .bind(&attention_id)
            .fetch_one(&pool)
            .await
            .expect("attention row");
    assert!(answered.is_some(), "the answered row must be closed");

    let log = std::fs::read_to_string(&rpc_log).expect("rpc log");
    assert_eq!(
        log.matches("permission").count(),
        1,
        "exactly one permission round trip: {log}"
    );
    assert_no_process_executor_trace(&row, &marker, task_id);
}

/// Cancelling a live ACP task tells the ADAPTER, not just the poll loop.
///
/// Dropping `run_acp` stops polling; the agent lives in another process. Without
/// the explicit `session/cancel` the fixture's hung turn would run on, so the
/// `cancel` line in its rpc log is the whole assertion.
#[tokio::test]
async fn cancelling_an_acp_task_cancels_the_adapter_turn() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp cancel e2e");
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
        "agent-hang",
        &serde_json::json!({
            // The turn NEVER answers, so the cancel below always races a live
            // turn rather than one that finished on its own.
            "FAKE_ACP_HANG_SESSIONS": "*",
            "FAKE_ACP_RPC_LOG": rpc_log.display().to_string(),
        }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");
    let marker = home.path().join("process-executor-ran");
    let fake_claude = write_marker_binary(home.path(), &marker);

    let issue_id = "issue-acp-cancel";
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, description, state, creator_type, \
         creator_id, created_at) VALUES (?, ?, 'cancel me', 'hang forever', 'open', \
         'member', 'user-trip', 0)",
    )
    .bind(issue_id)
    .bind(&ids.workspace_id)
    .execute(&pool)
    .await
    .expect("insert issue");

    let session = spawn_acp_daemon(home.path(), &ids, &fake_claude);
    let task_id = "task-acp-cancel";
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&agent)
    .bind(issue_id)
    .bind(tripwire_support::now_ms())
    .execute(&pool)
    .await
    .expect("enqueue hanging task");

    // The prompt has REACHED the adapter before the cancel, so the cancel below
    // stops a genuinely open turn.
    wait_for_file(&rpc_log, Duration::from_mins(1), |log| {
        log.contains("prompt")
    });

    let mut client = Client::connect(&home.path().join("hangar.sock")).await;
    client.auth_from_file(home.path()).await;
    let resp = client
        .call(
            methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            serde_json::json!({ "workspace_id": ids.workspace_id, "issue_id": issue_id }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "issue_cancel_active must ack: {resp}"
    );

    let row = wait_for_db(&pool, task_id, "cancelled", Duration::from_mins(1)).await;
    drop(session);

    let log = wait_for_file(&rpc_log, Duration::from_secs(10), |log| {
        log.contains("cancel")
    });
    assert!(
        log.contains("cancel"),
        "the adapter must be told session/cancel, not merely abandoned: {log}"
    );
    assert_no_process_executor_trace(&row, &marker, task_id);
}

/// A turn that outlives `HANGAR_PROVIDER_MAX_RUNTIME_MS` is cancelled by the
/// EXECUTOR's own deadline and finalized `timeout`, and the adapter is told.
///
/// The task budget is the one the flag does not mention: the pool applies its
/// own turn deadline to every scope, so without the boot-time reconciliation an
/// ACP task would instead die on the pool's 30-minute default while the same
/// task on the process executor ran for 2.5 h. This pins the executor end of
/// that contract: its poll bound is `max_runtime`, not the pool's.
#[tokio::test]
async fn an_acp_turn_past_the_task_budget_times_out_and_tells_the_adapter() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp deadline e2e");
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
        "agent-deadline",
        &serde_json::json!({
            "FAKE_ACP_HANG_SESSIONS": "*",
            "FAKE_ACP_RPC_LOG": rpc_log.display().to_string(),
        }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");
    let marker = home.path().join("process-executor-ran");
    let fake_claude = write_marker_binary(home.path(), &marker);

    let home_str = home.path().display().to_string();
    let claude = fake_claude.display().to_string();
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", &home_str),
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", "acp"),
            ("HANGAR_CLAUDE_PATH", &claude),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
            // Far below the pool's 30-minute deadline, so the executor's own
            // bound is unambiguously the one that fired.
            ("HANGAR_PROVIDER_MAX_RUNTIME_MS", "4000"),
        ],
    );
    let task_id = "task-acp-deadline";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    let row = wait_for_db(&pool, task_id, "failed", Duration::from_mins(1)).await;
    drop(session);

    assert_eq!(
        row.get::<Option<String>, _>("failure_reason").as_deref(),
        Some("timeout"),
        "a turn past the task budget is a timeout, not drift: {}",
        dump_legs(&pool).await
    );
    let log = std::fs::read_to_string(&rpc_log).expect("rpc log");
    assert!(
        log.contains("cancel"),
        "the deadline path must tell the adapter too, not just stop polling: {log}"
    );
    assert_no_process_executor_trace(&row, &marker, task_id);
}

/// The pool's own turn deadline does NOT cap a task, because boot raises it to
/// the task budget.
///
/// The 4-second case above proves the executor's poll bound, but the raise is a
/// no-op there (30 min vs 4 s), so it exercises no reconciliation wiring at all;
/// the unit test pins the `max()` while touching neither `boot` nor
/// `sweep_once`. This is the case that fails on the unfixed code: the pool
/// deadline is 2 s and its sweep runs every 1 s, the task budget is 30 s, and
/// the turn is paced to about 4 s. Without the raise the sweep cancels the turn
/// at 2 s and the task finalizes `failed`/`timeout`; with it the turn finishes.
#[tokio::test]
async fn the_pool_deadline_does_not_cap_a_task_that_outlives_it() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping acp deadline reconciliation e2e");
        return;
    }
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    let agent = seed_agent_with_env(
        &pool,
        &ids,
        "agent-slow",
        &serde_json::json!({
            // About 4 seconds of turn: twice the pool's deadline below, so an
            // unreconciled sweep has two clean chances to kill it.
            "FAKE_ACP_CHUNKS": "8",
            "FAKE_ACP_CHUNK_DELAY_MS": "500",
        }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");
    let marker = home.path().join("process-executor-ran");
    let fake_claude = write_marker_binary(home.path(), &marker);

    let home_str = home.path().display().to_string();
    let claude = fake_claude.display().to_string();
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", &home_str),
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", "acp"),
            ("HANGAR_CLAUDE_PATH", &claude),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
            // The pool would cancel this turn at 2 s. Its sweep follows the
            // deadline down to a 1 s cadence, so it gets two passes inside the
            // turn: nothing here is a timing near-miss.
            ("AINB_ACP_TURN_DEADLINE_MS", "2000"),
            // The task budget the raise must lift the pool deadline to.
            ("HANGAR_PROVIDER_MAX_RUNTIME_MS", "30000"),
        ],
    );
    let task_id = "task-acp-outlives-pool";
    enqueue_task(&pool, &ids, task_id, &agent, "headless").await;

    let row = wait_for_terminal(&pool, task_id, Duration::from_mins(1)).await;
    // History, not the visible pane: the boot warn asserted below has scrolled
    // off by the time the run finishes.
    let pane = session.capture_pane_history();
    drop(session);

    assert_eq!(
        row.get::<String, _>("status"),
        "done",
        "a turn outliving the POOL deadline but inside the TASK budget must finish; \
         reason={:?}\n{}\ndaemon:\n{pane}",
        row.get::<Option<String>, _>("failure_reason"),
        dump_legs(&pool).await,
    );
    assert!(
        row.get::<Option<String>, _>("failure_reason").is_none(),
        "and record no failure reason"
    );
    // The raise is not silent. This daemon's chat sessions now share the longer
    // deadline, so boot says so where whoever flipped the flag will read it.
    // Newlines stripped first: tmux hard-wraps the pane at its width, which
    // splits the message mid-word.
    assert!(
        pane.replace('\n', "").contains("raised the acp turn deadline"),
        "boot must warn that it raised the deadline:\n{pane}"
    );
    assert_no_process_executor_trace(&row, &marker, task_id);
}

// ------------------------------------------------------------------ helpers

/// Spawn the real daemon under `HANGAR_TASK_EXECUTOR=acp`, with the fixture
/// adapter already configured and a fake `claude` standing by to prove it is
/// never reached.
fn spawn_acp_daemon(
    home: &Path,
    ids: &tripwire_support::SeededIds,
    fake_claude: &Path,
) -> DaemonSession {
    let home_str = home.display().to_string();
    let claude = fake_claude.display().to_string();
    DaemonSession::spawn(
        &daemon_bin(),
        home,
        &[
            ("AINB_HANGAR_HOME", &home_str),
            // The `[acp.adapters]` table is read from $HOME, not the hangar home.
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", "acp"),
            ("HANGAR_CLAUDE_PATH", &claude),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            // The fixture adapter writes its rpc log under the test's tempdir,
            // which is outside the task tree the policy confines to. The
            // confinement itself is unit-tested (`acp_task::tests`).
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
        ],
    )
}

/// The negative half, asserted on every case: `executor=acp` spawns no provider
/// process, writes no jsonl transcript and opens no tmux pane.
fn assert_no_process_executor_trace(row: &sqlx::sqlite::SqliteRow, marker: &Path, task_id: &str) {
    assert!(
        !marker.exists(),
        "executor=acp must spawn NO provider process; the fake claude left {}",
        marker.display()
    );
    if let Some(work_dir) = row.get::<Option<String>, _>("work_dir") {
        let logs = Path::new(&work_dir).parent().expect("shortID root").join("logs");
        let jsonl: Vec<_> = std::fs::read_dir(&logs)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        assert!(
            jsonl.is_empty(),
            "executor=acp must write no jsonl, found {jsonl:?}"
        );
    }
    let pane = format!("tmux_hangar-{task_id}");
    assert!(
        !tripwire_support::tmux_session_live(&pane),
        "executor=acp must open no tmux pane, found {pane}"
    );
}

/// Write an executable that records having run and exits 0.
fn write_marker_binary(dir: &Path, marker: &Path) -> std::path::PathBuf {
    let path = dir.join("fake-claude-marker.sh");
    std::fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", marker.display()))
        .expect("write marker binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod marker binary");
    }
    path
}

/// Poll until the task reaches ANY terminal state, so a wrong outcome is
/// reported with its reason and the leg that produced it rather than as a bare
/// "did not reach done".
async fn wait_for_terminal(
    pool: &SqlitePool,
    task_id: &str,
    budget: Duration,
) -> sqlx::sqlite::SqliteRow {
    tripwire_support::wait_until(pool, task_id, budget, |row| {
        matches!(
            row.get::<String, _>("status").as_str(),
            "done" | "failed" | "cancelled"
        )
    })
    .await
}

/// Every delivery leg in the store, for a failure message: the leg's `state`
/// and `detail` are exactly what the outcome mapping read.
async fn dump_legs(pool: &SqlitePool) -> String {
    let rows = sqlx::query(
        "SELECT d.session_key, d.state, d.detail FROM fleet_message_delivery d \
         JOIN fleet_message m ON m.id = d.message_id",
    )
    .fetch_all(pool)
    .await
    .expect("read delivery legs");
    if rows.is_empty() {
        return "no delivery legs at all".to_string();
    }
    rows.iter()
        .map(|row| {
            format!(
                "leg session={} state={} detail={:?}",
                row.get::<String, _>("session_key"),
                row.get::<String, _>("state"),
                row.get::<Option<String>, _>("detail")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Poll for the one approval attention row this run raises.
async fn wait_for_attention(pool: &SqlitePool, budget: Duration) -> sqlx::sqlite::SqliteRow {
    let deadline = Instant::now() + budget;
    loop {
        let row = sqlx::query("SELECT * FROM attention WHERE kind = 'approval' LIMIT 1")
            .fetch_optional(pool)
            .await
            .expect("query attention");
        if let Some(row) = row {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "no approval row within {budget:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}

/// A minimal framed JSON-RPC client over the daemon's `hangar.sock`.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(socket_path: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(30);
        let stream = loop {
            match UnixStream::connect(socket_path).await {
                Ok(c) => break c,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("never connected to {}: {e}", socket_path.display()),
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let req = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(7),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).expect("encode request");
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.expect("write frame");
        self.writer.flush().await.expect("flush");
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), self.read_frame())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 10s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn read_frame(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt as _;
        let mut len: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await.expect("read header");
            assert!(n > 0, "connection closed while awaiting a frame");
            let trimmed = line.trim_end_matches("\r\n");
            if trimmed.is_empty() {
                let mut body = vec![0u8; len.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.expect("read body");
                return serde_json::from_slice(&body).expect("decode frame");
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    len = value.trim().parse().ok();
                }
            }
        }
    }

    async fn auth_from_file(&mut self, dir: &Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let deadline = Instant::now() + Duration::from_secs(30);
        let token = loop {
            if let Ok(token) = std::fs::read_to_string(&token_path) {
                break token;
            }
            assert!(
                Instant::now() < deadline,
                "no daemon token at {}",
                token_path.display()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let resp = self
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
    }
}
