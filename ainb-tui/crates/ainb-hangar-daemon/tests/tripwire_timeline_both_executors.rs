//! Spine A6 e2e tripwire (test T8): ONE read serves both executors.
//!
//! ```text
//!   HANGAR_TASK_EXECUTOR=process        HANGAR_TASK_EXECUTOR=acp
//!         │ fake claude                        │ fake acp adapter
//!         ▼                                    ▼
//!   {logs}/claude.jsonl                  fleet_provider_event rows
//!         │                                    │
//!         └──── hangar/board_card_timeline ────┘
//!                        │
//!             the SAME (kind, body) taxonomy
//! ```
//!
//! Both halves run the same brief through the real daemon binary and the real
//! RPC, and the assertion is that the returned transcripts cover the same LANE
//! SET — an ACP run's expanded view reads like a process run's because it is
//! the same classifier, not because two implementations happen to agree.
//!
//! Each half also asserts the OTHER half's durable store is untouched. Without
//! that, an ACP branch that silently fell through to the jsonl read would leave
//! every positive assertion here green against an empty transcript, which is
//! exactly the failure this step exists to close (`fleet_provider_event` had no
//! reader at all before it).
//!
//! Skips cleanly (never fails) when tmux is unavailable, like every tripwire.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_proto::events::MessageKind;
use ainb_hangar_proto::snapshots::BoardCardTimelineResult;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tripwire_support::{
    DaemonSession, daemon_bin, fake_acp_adapter, seed_agent_with_env, seed_world, wait_for_db,
    write_acp_adapter_config,
};

const BOARD_ID: &str = "board-a6";
const COLUMN_ID: &str = "col-a6";

/// The PR URL both fixtures "open", on a line of its own exactly as `gh pr
/// create` prints it.
const FAKE_PR_URL: &str = "https://github.com/test/repo/pull/4242";

/// The lanes a run of this brief must fill under EITHER executor.
///
/// A known prior SET, not a count and not a lower bound: an executor that
/// stopped emitting tool results, or folded them into the agent lane, still
/// returns "some entries" and would pass any count-shaped guard.
fn expected_lanes() -> BTreeSet<String> {
    [
        MessageKind::Agent,
        MessageKind::ToolCall,
        MessageKind::ToolResult,
    ]
    .into_iter()
    .map(lane_name)
    .collect()
}

/// A lane's name, so the set is orderable and an assertion failure NAMES the
/// lane that went missing.
fn lane_name(kind: MessageKind) -> String {
    format!("{kind:?}")
}

/// T8: a process run and an ACP run of the same brief come back through the
/// same RPC in the same taxonomy, each read from its own durable store.
#[tokio::test]
async fn both_executors_serve_the_same_transcript_taxonomy() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping the unified timeline tripwire");
        return;
    }

    let process = run_process_executor().await;
    let acp = run_acp_executor().await;

    assert_eq!(
        lanes(&process.timeline),
        expected_lanes(),
        "the process run must fill every lane: {:#?}",
        process.timeline.entries
    );
    assert_eq!(
        lanes(&acp.timeline),
        expected_lanes(),
        "the ACP run must fill the SAME lanes, through the same classifier: {:#?}",
        acp.timeline.entries
    );

    // The tool the brief ran is named in both, so the two views are comparable
    // line for line and not merely lane for lane.
    for (executor, timeline) in [("process", &process.timeline), ("acp", &acp.timeline)] {
        assert!(
            timeline.entries.iter().any(|entry| entry.kind == MessageKind::ToolCall
                && entry.body.contains("Bash")),
            "the {executor} transcript must name the tool it ran: {:#?}",
            timeline.entries
        );
    }
}

/// The negative half, and the reason the positive half means anything: each
/// executor wrote ONLY its own durable store, so the read really did serve two
/// different sources rather than one with a silent fallback.
#[tokio::test]
async fn each_executor_leaves_the_other_durable_store_empty() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping the durable-store tripwire");
        return;
    }

    let process = run_process_executor().await;
    assert_eq!(
        acp_row_count(&process.pool, &process.task_id).await,
        0,
        "a process run must write NO fleet_provider_event rows (section 0 decision 3)"
    );
    assert!(
        process.jsonl_path.exists(),
        "a process run must tee its stream-json to {}",
        process.jsonl_path.display()
    );

    let acp = run_acp_executor().await;
    assert!(
        acp_row_count(&acp.pool, &acp.task_id).await > 0,
        "an ACP run's transcript must be fleet_provider_event rows"
    );
    assert!(
        !acp.jsonl_path.exists(),
        "an ACP run must write no jsonl; found {}",
        acp.jsonl_path.display()
    );
}

/// What one executor's run left behind.
struct Run {
    pool: SqlitePool,
    task_id: String,
    /// Where the process executor's transcript is (or would be).
    jsonl_path: PathBuf,
    timeline: BoardCardTimelineResult,
    /// Kept so the tempdir outlives every assertion above.
    _home: tempfile::TempDir,
}

/// The lanes a timeline actually filled.
fn lanes(timeline: &BoardCardTimelineResult) -> BTreeSet<String> {
    timeline.entries.iter().map(|entry| lane_name(entry.kind)).collect()
}

/// `fleet_provider_event` rows written under this task's ACP scope.
async fn acp_row_count(pool: &SqlitePool, task_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM fleet_provider_event WHERE session_key IN \
         (SELECT session_key FROM fleet_acp_session WHERE scope_key = ?)",
    )
    .bind(format!("task:{task_id}"))
    .fetch_one(pool)
    .await
    .expect("count acp transcript rows")
}

/// Run the brief under `HANGAR_TASK_EXECUTOR=process` against a fake `claude`
/// that emits prose, a tool call, its result carrying the PR URL, and the
/// terminal `result` line.
async fn run_process_executor() -> Run {
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let issue_id = seed_card(&pool, &ids, "process").await;

    let fake_claude = write_executable(
        home.path(),
        "fake-claude-a6.sh",
        &[
            "#!/bin/sh".to_string(),
            echo_line(&serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": "Opening the PR."}]},
            })),
            echo_line(&serde_json::json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use", "id": "toolu_1", "name": "Bash",
                    "input": {"command": "gh pr create --fill"},
                }]},
            })),
            echo_line(&serde_json::json!({
                "type": "user",
                "message": {"content": [{
                    "type": "tool_result", "tool_use_id": "toolu_1",
                    "content": format!("Creating pull request\n{FAKE_PR_URL}"),
                }]},
            })),
            echo_line(&serde_json::json!({
                "type": "result", "subtype": "success", "result": "PR is up.",
            })),
            "exit 0".to_string(),
        ]
        .join("\n"),
    );

    let home_str = home.path().display().to_string();
    let claude = fake_claude.display().to_string();
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", &home_str),
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", "process"),
            ("HANGAR_CLAUDE_PATH", &claude),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
        ],
    );

    let task_id = "task-a6-process";
    enqueue_for_issue(&pool, &ids, task_id, &ids.agent_id, &issue_id).await;
    finish(home, pool, session, task_id, &issue_id, "claude.jsonl").await
}

/// Run the same brief under `HANGAR_TASK_EXECUTOR=acp` against the fixture
/// adapter, scripted to emit the ACP shape of the same turn.
async fn run_acp_executor() -> Run {
    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let issue_id = seed_card(&pool, &ids, "acp").await;

    // The adapter's turn, in `session/update` shape: the same prose, the same
    // tool, the same output. `gh`'s URL rides the tool's OUTPUT, never the
    // agent's closing message, so the capture cannot pass by reading the reply.
    let script = home.path().join("turn.ndjson");
    std::fs::write(
        &script,
        [
            serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "Opening the PR."},
            }),
            serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Bash",
                "kind": "execute",
                "status": "pending",
                "rawInput": {"command": "gh pr create --fill"},
            }),
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "content": [{
                    "type": "content",
                    "content": {"type": "text", "text": format!("Creating pull request\n{FAKE_PR_URL}")},
                }],
            }),
            serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "PR is up."},
            }),
        ]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n"),
    )
    .expect("write the adapter script");

    let agent = seed_agent_with_env(
        &pool,
        &ids,
        "agent-a6-acp",
        &serde_json::json!({ "FAKE_ACP_SCRIPT": script.display().to_string() }),
    )
    .await;
    write_acp_adapter_config(home.path(), &fake_acp_adapter(), "default");

    // A fake `claude` that would leave a marker if the process executor ever
    // ran; the ACP arm must never reach it.
    let fake_claude = write_executable(
        home.path(),
        "fake-claude-marker.sh",
        &format!(
            "#!/bin/sh\ntouch '{}'\n",
            home.path().join("process-executor-ran").display()
        ),
    );

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
        ],
    );

    let task_id = "task-a6-acp";
    enqueue_for_issue(&pool, &ids, task_id, &agent, &issue_id).await;
    let run = finish(home, pool, session, task_id, &issue_id, "claude.jsonl").await;
    assert!(
        !run.jsonl_path.parent().expect("logs dir").join("..").join("process-executor-ran").exists(),
        "executor=acp must never spawn the provider process"
    );
    run
}

/// Wait for the run to land, read its timeline through the real RPC, and tear
/// the daemon down by exact session name.
async fn finish(
    home: tempfile::TempDir,
    pool: SqlitePool,
    session: DaemonSession,
    task_id: &str,
    issue_id: &str,
    log_file: &str,
) -> Run {
    let scale = u64::from(tripwire_support::budget_scale());
    let row = wait_for_db(&pool, task_id, "done", Duration::from_secs(60 * scale)).await;

    let mut client = Client::connect(&home.path().join("hangar.sock")).await;
    client.auth_from_file(home.path()).await;
    let workspace_id: String = sqlx::query_scalar("SELECT workspace_id FROM agent_task_queue WHERE id = ?")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .expect("the task's workspace");
    let reply = client
        .call(
            methods::HANGAR_BOARD_CARD_TIMELINE,
            serde_json::json!({
                "workspace_id": workspace_id,
                "board_id": BOARD_ID,
                "issue_id": issue_id,
            }),
        )
        .await;
    let pane = session.capture_pane();
    drop(session);

    assert!(
        reply["error"].is_null(),
        "board_card_timeline must answer: {reply}\ndaemon:\n{pane}"
    );
    let timeline: BoardCardTimelineResult =
        serde_json::from_value(reply["result"].clone()).expect("decode timeline result");
    assert_eq!(
        timeline.task_id.as_deref(),
        Some(task_id),
        "the timeline must resolve this run"
    );

    let work_dir: String = row.get::<Option<String>, _>("work_dir").expect("work_dir");
    let jsonl_path = Path::new(&work_dir)
        .parent()
        .expect("the run root above workdir")
        .join("logs")
        .join(log_file);

    Run {
        pool,
        task_id: task_id.to_string(),
        jsonl_path,
        timeline,
        _home: home,
    }
}

/// One `echo '<json>'` line for a `/bin/sh` fixture. Single-quoted so the JSON's
/// double quotes survive; the payloads carry no single quote of their own.
fn echo_line(value: &serde_json::Value) -> String {
    let line = value.to_string();
    assert!(!line.contains('\''), "fixture payload must be quote-free");
    format!("echo '{line}'")
}

/// Seed the issue + board + column + card `board_card_timeline` resolves
/// through (it refuses an issue that is not a card on the named board).
async fn seed_card(pool: &SqlitePool, ids: &tripwire_support::SeededIds, tag: &str) -> String {
    let issue_id = format!("issue-a6-{tag}");
    let ws = WorkspaceId::from_str(ids.workspace_id.clone()).expect("ws id");
    let now = tripwire_support::now_ms();
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: issue_id.clone(),
            workspace_id: ids.workspace_id.clone(),
            title: "Open the PR".into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: ainb_hangar_core::actor::ActorRef::new(
                ainb_hangar_core::actor::ActorKind::Member,
                "user-trip",
            )
            .unwrap(),
            created_at: now,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            parent_issue_id: None,
            stage: None,
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
        },
    )
    .await
    .expect("insert issue");
    BoardRepo::create(pool, &ws, BOARD_ID, "Delivery", now)
        .await
        .expect("create board");
    BoardRepo::column_add(pool, &ws, BOARD_ID, COLUMN_ID, "Todo", None, false)
        .await
        .expect("add column");
    BoardRepo::card_add(pool, &ws, BOARD_ID, &issue_id, Some(COLUMN_ID), now)
        .await
        .expect("place card");
    issue_id
}

/// Enqueue a headless run of `issue_id`. The shared helper takes no issue, and
/// `board_card_timeline` resolves the card's newest task BY issue.
async fn enqueue_for_issue(
    pool: &SqlitePool,
    ids: &tripwire_support::SeededIds,
    task_id: &str,
    agent_id: &str,
    issue_id: &str,
) {
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, mode, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(agent_id)
    .bind(issue_id)
    .bind("headless")
    .bind(tripwire_support::now_ms())
    .execute(pool)
    .await
    .expect("enqueue task");
}

/// Write `body` to `dir/name` and make it executable.
fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
    }
    path
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}

/// A minimal framed RPC client over the daemon's control socket.
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
            let frame = tokio::time::timeout(Duration::from_secs(15), self.read_frame())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 15s"));
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
