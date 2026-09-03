//! Track A test T1, the PROCESS half: a running task streams its transcript
//! live, in the SAME taxonomy the durable re-read returns.
//!
//! ```text
//!  seed world + issue + board card        spawn the real daemon binary
//!         │                                          │
//!         ▼                                          ▼
//!  subscribe the workspace over hangar.sock ──▶ INSERT queued task
//!         │                                          │
//!         │                          claim ─▶ running ─▶ fake claude
//!         │                                          │ stream-json lines
//!         ▼                                          ▼
//!  live: TaskMessage(kind, body)…        durable: logs/claude.jsonl
//!         └────────────── must be EQUAL ─────────────┘
//!                  (hangar/board_card_timeline, classified)
//! ```
//!
//! Both sides are the real thing: a genuine `ainb-hangar-daemon` process, a real
//! Unix-socket subscription, a real provider subprocess writing real
//! stream-json, and the real `hangar/board_card_timeline` read the TUI's
//! transcript pane backfills from.
//!
//! What this pins, and why each half is needed:
//!
//! * **the producer exists at all.** Before track A step A2, `TaskMessage` had
//!   consumers (the board timeline overlay, the run banner, task detail) and no
//!   producer anywhere in `src/`. An empty live sequence fails the emptiness
//!   assertion, not merely the equality one: two empty vectors are equal.
//! * **one taxonomy, not two.** The live sequence is compared to what
//!   `board_card_timeline` RETURNS, byte for byte — since A6 the daemon
//!   classifies the durable record itself, so this compares two products of the
//!   same code reached by two different paths, with no client-side
//!   re-classification standing between them. A live producer that classified
//!   differently (its own parser, a per-line classifier that loses a
//!   `tool_result`'s tool name) would diverge here even though both halves
//!   "work".
//!
//! # The equality holds under the read's 512 KiB tail, not beyond it
//!
//! `board_card_timeline` returns a bounded TAIL of the run's transcript, so this
//! equality is exact only for a run whose whole transcript fits it. Past that the
//! re-read starts mid-file with a fresh classifier, and a `tool_result` whose
//! `tool_use` fell outside the window degrades to the unnamed `tool` form while
//! the live line kept the name. That is a property of the READ, not of the
//! producer: "live always equals durable" is NOT an unconditional invariant, and
//! the ACP half of the read (A6) inherits the same bounded form of it.
//!
//! SKIPs cleanly when tmux is unavailable, like every other tripwire.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_proto::events::{EVENT_METHOD, MessageKind};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tripwire_support::{DaemonSession, daemon_bin, seed_world, wait_for_db};

const BOARD_ID: &str = "board-live";
const COLUMN_ID: &str = "col-live";
const ISSUE_ID: &str = "issue-live-1";
const TASK_ID: &str = "task-live-1";

#[tokio::test]
async fn a_process_run_streams_the_same_transcript_it_later_re_reads() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping live-stream tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    seed_card(&pool, &ids).await;

    let fake_claude = fake_claude_full_transcript(home.path(), "live-stream-1");
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // Subscribe BEFORE enqueueing. The live stream is fire-and-forget with no
    // replay behind it (a transcript line never lands in the durable event log,
    // see `EventSink::emit_live`), so a subscription opened after the claim would
    // legitimately miss the head of the run and this test would be asserting on
    // a truncated sequence.
    let mut sub = Client::connect(&rpc_socket(home.path())).await;
    sub.auth_from_file(home.path()).await;
    sub.subscribe(&ids.workspace_id).await;

    enqueue_task(&pool, &ids).await;

    let scale = u64::from(tripwire_support::budget_scale());
    let run = sub.collect_run(TASK_ID, Duration::from_secs(30 * scale)).await;
    let live = run.transcript.clone();

    // The FSM terminal is the signal the provider's stdout is closed and its log
    // flushed, so the durable read below sees the whole file. A run that never
    // reached `done` would leave the equality below comparing against a partial
    // file, so read the status back rather than trusting the wait.
    // `wait_for_db` returns ONLY on a match and panics on timeout, so reaching the
    // next line is the assertion.
    let _ = wait_for_db(&pool, TASK_ID, "done", Duration::from_secs(30 * scale)).await;

    let timeline = sub
        .call(
            methods::HANGAR_BOARD_CARD_TIMELINE,
            serde_json::json!({
                "workspace_id": ids.workspace_id,
                "board_id": BOARD_ID,
                "issue_id": ISSUE_ID,
            }),
        )
        .await;
    drop(session); // kill the tmux session by exact name before asserting

    assert!(
        timeline["error"].is_null(),
        "board_card_timeline must answer: {timeline}"
    );
    let durable: ainb_hangar_proto::snapshots::BoardCardTimelineResult =
        serde_json::from_value(timeline["result"].clone()).expect("decode timeline result");
    assert_eq!(
        durable.task_id.as_deref(),
        Some(TASK_ID),
        "the timeline must resolve the card's run"
    );
    let re_read: Vec<(MessageKind, String)> =
        durable.entries.iter().map(|e| (e.kind, e.body.clone())).collect();

    // NEGATIVE, and it must come first: a producer that emits nothing would make
    // the equality below trivially true against an empty re-read.
    assert!(
        !live.is_empty(),
        "a running task must stream TaskMessage events; got none.\n\
         events seen on the connection: {:?}\n\
         the durable re-read was:\n{:#?}\ndaemon logs:\n{}",
        run.seen,
        re_read,
        read_daemon_log(home.path())
    );
    // And it must be a real transcript, not one lane: the fake provider emits
    // prose, thinking, a tool call and its result, so all four lanes must show.
    for want in [
        MessageKind::Agent,
        MessageKind::Thinking,
        MessageKind::ToolCall,
        MessageKind::ToolResult,
    ] {
        assert!(
            live.iter().any(|(kind, _)| *kind == want),
            "the live stream must carry a {want:?} line; got {live:#?}"
        );
    }

    // THE ASSERTION: live and durable are the same sequence, kind for kind and
    // byte for byte. This is what makes the expanded run view identical whether
    // it filled live or backfilled from the timeline read.
    assert_eq!(
        live, re_read,
        "the live TaskMessage sequence must equal the durable re-read"
    );

    // The one deliberate design decision in this change, pinned: the transcript
    // rides `emit_live`, so it never lands an `event_log` row. Swapping it back to
    // `emit` would leave every assertion above green while putting thousands of
    // INSERTs per run through the one write lock the control plane shares.
    let logged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_log WHERE event_type IN ('task_message', 'task_progress')",
    )
    .fetch_one(&pool)
    .await
    .expect("count transcript rows in the durable event log");
    assert_eq!(
        logged, 0,
        "a transcript line must never reach the durable event log"
    );

    // The run banner's other half: the heartbeat lands, and its FINAL tally
    // counts the one tool the fake provider called. A counter that never
    // incremented (or a producer whose last tick predates the tool) reads 0 here
    // even with the transcript above perfectly correct.
    assert_eq!(
        run.last_tool_calls,
        Some(1),
        "the closing TaskProgress must report the run's one tool call"
    );
}

/// Track A test T1, the ACP half: the same equality, under the other executor.
///
/// This is the half that was missing when A6 landed the durable read: an ACP
/// run's expanded view could only backfill, never fill live, so the exit
/// criterion's part (A) held for one executor of two. The assertion is
/// deliberately the SAME one the process arm makes, against the same RPC, so a
/// producer that emitted a different taxonomy than the re-read would fail here
/// exactly as it would there.
///
/// The equality is exact only while the store writer's buffer does not
/// overflow. Live is published BEFORE the durable commit, so a row the writer
/// later drops under memory pressure was already streamed, and live would carry
/// a line durable does not — an asymmetry the process side has no equivalent
/// for, since its tee to disk is unconditional. This fixture's transcript is
/// four small rows against a 1 MiB buffer, so it is nowhere near that boundary.
#[tokio::test]
async fn an_acp_run_streams_the_same_transcript_it_later_re_reads() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping the acp live-stream tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    seed_card(&pool, &ids).await;

    // The adapter's turn, in `session/update` shape: prose, a thought, a tool
    // call and its completion — the same four lanes the process fixture emits,
    // so the two arms of T1 are comparable rather than merely both green.
    let script = home.path().join("turn.ndjson");
    std::fs::write(
        &script,
        [
            serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "Reading the routes file."},
            }),
            serde_json::json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": "The handler is probably unregistered."},
            }),
            serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Edit",
                "kind": "edit",
                "status": "pending",
                "rawInput": {"file_path": "api/src/routes.ts"},
            }),
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": "1 file changed"}}],
            }),
        ]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n"),
    )
    .expect("write the adapter script");

    let agent = tripwire_support::seed_agent_with_env(
        &pool,
        &ids,
        "agent-live-acp",
        &serde_json::json!({ "FAKE_ACP_SCRIPT": script.display().to_string() }),
    )
    .await;
    tripwire_support::write_acp_adapter_config(
        home.path(),
        &tripwire_support::fake_acp_adapter(),
        "default",
    );

    let home_str = home.path().display().to_string();
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", &home_str),
            // The `[acp.adapters]` table is read from $HOME, not the hangar home.
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", "acp"),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
        ],
    );

    // Subscribe BEFORE enqueueing, for the reason the process arm gives: the
    // transcript stream has no replay behind it.
    let mut sub = Client::connect(&rpc_socket(home.path())).await;
    sub.auth_from_file(home.path()).await;
    sub.subscribe(&ids.workspace_id).await;

    let task_id = "task-live-acp";
    enqueue_task_for(&pool, &ids, task_id, &agent).await;

    let scale = u64::from(tripwire_support::budget_scale());
    let run = sub.collect_run(task_id, Duration::from_secs(60 * scale)).await;
    let live = run.transcript.clone();
    let _ = wait_for_db(&pool, task_id, "done", Duration::from_secs(60 * scale)).await;

    let timeline = sub
        .call(
            methods::HANGAR_BOARD_CARD_TIMELINE,
            serde_json::json!({
                "workspace_id": ids.workspace_id,
                "board_id": BOARD_ID,
                "issue_id": ISSUE_ID,
            }),
        )
        .await;
    let pane = session.capture_pane();
    drop(session);

    assert!(
        timeline["error"].is_null(),
        "board_card_timeline must answer: {timeline}\ndaemon:\n{pane}"
    );
    let durable: ainb_hangar_proto::snapshots::BoardCardTimelineResult =
        serde_json::from_value(timeline["result"].clone()).expect("decode timeline result");
    assert_eq!(
        durable.task_id.as_deref(),
        Some(task_id),
        "the timeline must resolve the card's run"
    );
    let re_read: Vec<(MessageKind, String)> =
        durable.entries.iter().map(|e| (e.kind, e.body.clone())).collect();

    // NEGATIVE first, for the same reason: two empty vectors are equal.
    assert!(
        !live.is_empty(),
        "an ACP run must stream TaskMessage events; got none.\n\
         events seen on the connection: {:?}\n\
         the durable re-read was:\n{:#?}\ndaemon logs:\n{}",
        run.seen,
        re_read,
        read_daemon_log(home.path())
    );
    for want in [
        MessageKind::Agent,
        MessageKind::Thinking,
        MessageKind::ToolCall,
        MessageKind::ToolResult,
    ] {
        assert!(
            live.iter().any(|(kind, _)| *kind == want),
            "the live ACP stream must carry a {want:?} line; got {live:#?}"
        );
    }

    // THE ASSERTION, identical to the process arm's.
    assert_eq!(
        live, re_read,
        "the live TaskMessage sequence must equal the durable re-read"
    );

    // Same design decision, same pin: the ACP producer publishes through the
    // same `emit_live` and must not land an `event_log` row either.
    let logged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_log WHERE event_type IN ('task_message', 'task_progress')",
    )
    .fetch_one(&pool)
    .await
    .expect("count transcript rows in the durable event log");
    assert_eq!(
        logged, 0,
        "a transcript line must never reach the durable event log"
    );

    // The closing tally counts the one tool the scripted turn called. The ACP
    // actor's heartbeat rides the writer's flush cadence, so without the closing
    // tick at turn end a turn this short would report nothing at all.
    assert_eq!(
        run.last_tool_calls,
        Some(1),
        "the closing TaskProgress must report the run's one tool call"
    );
}

/// T1 for a run that is CUT SHORT, which is where criterion (A) was still
/// false at `925b83a3`.
///
/// `converge_dirty_session` mints the `turn_interrupted` marker with
/// `FleetProviderEventRepo::append`, past the store writer and past every wrap
/// around it, so the marker landed durably and never streamed. Unlike the two
/// bookkeeping markers, this one RENDERS: the live pane ended without the
/// interruption and re-opening the ticket showed a line nobody saw arrive.
///
/// Convergence is the operator-stop and adapter-death path, so this is exactly
/// the run an operator is watching when they press stop.
#[tokio::test]
async fn a_cancelled_acp_run_streams_its_interruption() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping the acp cancel live-stream tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let pool = open_pool(&home.path().join("hangar.db")).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    seed_card(&pool, &ids).await;

    // The turn never answers, so the cancel always races a genuinely open turn
    // and convergence is the path that ends it.
    let rpc_log = home.path().join("rpc.log");
    let agent = tripwire_support::seed_agent_with_env(
        &pool,
        &ids,
        "agent-live-cancel",
        &serde_json::json!({
            "FAKE_ACP_HANG_SESSIONS": "*",
            "FAKE_ACP_RPC_LOG": rpc_log.display().to_string(),
        }),
    )
    .await;
    tripwire_support::write_acp_adapter_config(
        home.path(),
        &tripwire_support::fake_acp_adapter(),
        "default",
    );

    let home_str = home.path().display().to_string();
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", &home_str),
            ("HOME", &home_str),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_TASK_EXECUTOR", "acp"),
            ("HANGAR_DAEMON_POLL_MS", "200"),
            ("HANGAR_DAEMON_DISABLE_SANDBOX", "1"),
        ],
    );

    let mut sub = Client::connect(&rpc_socket(home.path())).await;
    sub.auth_from_file(home.path()).await;
    sub.subscribe(&ids.workspace_id).await;

    let task_id = "task-live-cancel";
    enqueue_task_for(&pool, &ids, task_id, &agent).await;

    let scale = u64::from(tripwire_support::budget_scale());
    // The prompt has REACHED the adapter, so the cancel stops an open turn and
    // convergence has a `turn_interrupted` marker to mint.
    tripwire_support::wait_for_file(&rpc_log, Duration::from_secs(60 * scale), |log| {
        log.contains("prompt")
    });

    let mut client = Client::connect(&rpc_socket(home.path())).await;
    client.auth_from_file(home.path()).await;
    let resp = client
        .call(
            methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            serde_json::json!({ "workspace_id": ids.workspace_id, "issue_id": ISSUE_ID }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "issue_cancel_active must ack: {resp}"
    );

    let run = sub.collect_run(task_id, Duration::from_secs(60 * scale)).await;
    let _ = wait_for_db(&pool, task_id, "cancelled", Duration::from_secs(60 * scale)).await;
    let mut live = run.transcript.clone();
    live.extend(sub.drain_until_message(task_id, Duration::from_secs(15 * scale)).await);

    let timeline = sub
        .call(
            methods::HANGAR_BOARD_CARD_TIMELINE,
            serde_json::json!({
                "workspace_id": ids.workspace_id,
                "board_id": BOARD_ID,
                "issue_id": ISSUE_ID,
            }),
        )
        .await;
    let pane = session.capture_pane();
    drop(session);

    assert!(
        timeline["error"].is_null(),
        "board_card_timeline must answer: {timeline}\ndaemon:\n{pane}"
    );
    let durable: ainb_hangar_proto::snapshots::BoardCardTimelineResult =
        serde_json::from_value(timeline["result"].clone()).expect("decode timeline result");
    let re_read: Vec<(MessageKind, String)> =
        durable.entries.iter().map(|e| (e.kind, e.body.clone())).collect();

    // POSITIVE and specific: the durable read really did get an interruption
    // marker, so the equality below is asserting about the row this test exists
    // for and not about an empty transcript on both sides.
    assert!(
        re_read
            .iter()
            .any(|(kind, body)| *kind == MessageKind::Error && body.contains("turn_interrupted")),
        "the cancelled run's durable transcript must carry the interruption: {re_read:#?}"
    );
    assert_eq!(
        live, re_read,
        "a cancelled run's live stream must equal its durable re-read too\nseen: {:?}",
        run.seen
    );
}

/// A fake `claude` emitting a stream-json transcript that exercises every lane
/// the classifier has: the `system` handle, assistant prose, a thinking block, a
/// `tool_use` and its matching `tool_result` (which resolves its tool NAME only
/// from the earlier line, so this also proves the producer keeps ONE classifier
/// for the whole run), and the terminal `result`.
fn fake_claude_full_transcript(dir: &Path, session_id: &str) -> PathBuf {
    let lines = [
        format!(r#"{{"type":"system","subtype":"init","session_id":"{session_id}"}}"#),
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading the routes file."}]}}"#.to_string(),
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"The handler is probably unregistered."}]}}"#.to_string(),
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"api/src/routes.ts"}}]}}"#.to_string(),
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"1 file changed"}]}}"#.to_string(),
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Route registered."}]}}"#.to_string(),
        r#"{"type":"result","subtype":"success","content":"ok"}"#.to_string(),
    ];
    // Single-quoted so the JSON's double quotes survive `sh`; the payloads carry
    // no single quote of their own.
    let body = lines.iter().fold("#!/bin/sh\n".to_string(), |mut acc, line| {
        acc.push_str(&format!("echo '{line}'\n"));
        acc
    }) + "exit 0\n";
    write_executable(dir, "fake-claude-transcript.sh", &body)
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

/// Seed the issue + board + column + card `board_card_timeline` resolves through
/// (it refuses an issue that is not a card on the named board).
async fn seed_card(pool: &SqlitePool, ids: &tripwire_support::SeededIds) {
    let ws = WorkspaceId::from_str(ids.workspace_id.clone()).expect("ws id");
    let now = tripwire_support::now_ms();
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: ISSUE_ID.into(),
            workspace_id: ids.workspace_id.clone(),
            title: "Register the route".into(),
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
    BoardRepo::card_add(pool, &ws, BOARD_ID, ISSUE_ID, Some(COLUMN_ID), now)
        .await
        .expect("place card");
}

/// Enqueue the card's run.
async fn enqueue_task(pool: &SqlitePool, ids: &tripwire_support::SeededIds) {
    enqueue_task_for(pool, ids, TASK_ID, &ids.agent_id).await;
}

/// Enqueue a headless run of [`ISSUE_ID`] under a named task + agent.
async fn enqueue_task_for(
    pool: &SqlitePool,
    ids: &tripwire_support::SeededIds,
    task_id: &str,
    agent_id: &str,
) {
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, mode, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(agent_id)
    .bind(ISSUE_ID)
    .bind("headless")
    .bind(tripwire_support::now_ms())
    .execute(pool)
    .await
    .expect("enqueue task");
}

/// The daemon's own structured log under the test's `AINB_HANGAR_HOME` (NOT
/// `$HOME`, which is where `tripwire_support::dump_daemon_logs` looks and why it
/// found nothing here).
fn read_daemon_log(home: &Path) -> String {
    let dir = home.join("hangar").join("logs");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return format!("(no daemon logs at {})", dir.display());
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The daemon's control socket under `AINB_HANGAR_HOME`.
fn rpc_socket(home: &Path) -> PathBuf {
    home.join("hangar.sock")
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}

/// One subscribed client connection: a persistent buffered reader half plus a
/// writer half.
///
/// The reader half MUST live for the whole connection: a fresh `BufReader` per
/// call would buffer-and-drop pushed notification frames arriving between a
/// request and its response, which on this test is the entire subject matter.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    /// Poll-connect until the daemon's accept loop is up, then split the stream.
    async fn connect(socket_path: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(30);
        let stream = loop {
            match UnixStream::connect(socket_path).await {
                Ok(c) => break c,
                Err(e) => {
                    assert!(
                        Instant::now() < deadline,
                        "daemon socket never came up at {}: {e}",
                        socket_path.display()
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

    /// Send one Content-Length framed request.
    async fn send(&mut self, method: &str, params: serde_json::Value) {
        let req = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(7),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    /// Read one frame body as raw JSON (response OR pushed notification), or
    /// `None` on timeout.
    async fn read_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        tokio::time::timeout(timeout, self.read_frame_inner()).await.ok()
    }

    async fn read_frame_inner(&mut self) -> serde_json::Value {
        let mut len: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await.unwrap();
            assert!(n > 0, "connection closed while awaiting a frame");
            let t = line.trim_end_matches("\r\n");
            if t.is_empty() {
                let mut body = vec![0u8; len.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.unwrap();
                return serde_json::from_slice(&body).unwrap();
            }
            if let Some((name, v)) = t.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    len = v.trim().parse().ok();
                }
            }
        }
    }

    /// Send `method` and read frames until its response (a frame with an `id`)
    /// arrives. Notifications interleaved before it are discarded.
    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        loop {
            let frame = self
                .read_frame(Duration::from_secs(15))
                .await
                .unwrap_or_else(|| panic!("no response to {method} within 15s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    /// Authenticate exactly as a real client does: read the plaintext the daemon
    /// minted at boot and present it on the first `auth/hello` frame.
    async fn auth_from_file(&mut self, home: &Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(home);
        let deadline = Instant::now() + Duration::from_secs(30);
        let token = loop {
            if let Ok(t) = std::fs::read_to_string(&token_path) {
                break t;
            }
            assert!(
                Instant::now() < deadline,
                "daemon never minted {}",
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

    /// Subscribe a workspace and assert the ack.
    async fn subscribe(&mut self, workspace_id: &str) {
        let resp = self
            .call(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({ "workspace_id": workspace_id }),
            )
            .await;
        assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
    }

    /// Collect this run's live stream: every `TaskMessage` for [`TASK_ID`] in
    /// arrival order, plus the tool count off the LAST `TaskProgress`, up to the
    /// run's `TaskFinished`.
    ///
    /// Terminating on `TaskFinished` (rather than a silence window) is what keeps
    /// this honest against the durable read: it is the same boundary the durable
    /// file has, so a producer that kept emitting past the terminal would be
    /// caught by the equality assertion rather than hidden by a short timeout.
    async fn collect_run(&mut self, task_id: &str, budget: Duration) -> LiveRun {
        let deadline = Instant::now() + budget;
        let mut run = LiveRun::default();
        loop {
            let frame = match deadline.checked_duration_since(Instant::now()) {
                Some(remaining) => self.read_frame(remaining).await,
                None => None,
            };
            let Some(frame) = frame else {
                panic!("the run never reported TaskFinished within {budget:?}; got {run:#?}")
            };
            if frame.get("id").is_some() || frame["method"] != EVENT_METHOD {
                continue;
            }
            let event = &frame["params"];
            run.seen.push(format!(
                "{}({})",
                event["event"].as_str().unwrap_or("?"),
                event["task_id"].as_str().unwrap_or("-")
            ));
            if event["task_id"] != task_id {
                continue;
            }
            match event["event"].as_str() {
                Some("task_message") => {
                    let kind: MessageKind =
                        serde_json::from_value(event["kind"].clone()).expect("decode kind");
                    let body = event["body"].as_str().expect("body is a string").to_string();
                    run.transcript.push((kind, body));
                }
                Some("task_progress") => {
                    run.last_tool_calls =
                        Some(event["tool_calls"].as_u64().expect("tool_calls is a number"));
                }
                Some("task_finished") => return run,
                _ => {}
            }
        }
    }

    /// Keep collecting this task's `TaskMessage`s PAST its terminal event,
    /// until one arrives or `budget` expires.
    ///
    /// Only the cancelled arm needs this. A cancelled run's interruption marker
    /// is published by `converge_dirty_session`, which runs in the pool actor
    /// and races the cancel RPC's own terminal event, so `TaskFinished` can
    /// legitimately overtake the last transcript line. Benign in the UI, where
    /// the pane is still open and a late line lands; but a collector that
    /// stopped dead at the terminal would report a stream that is merely
    /// unfinished as one that is wrong.
    async fn drain_until_message(
        &mut self,
        task_id: &str,
        budget: Duration,
    ) -> Vec<(MessageKind, String)> {
        let deadline = Instant::now() + budget;
        let mut extra = Vec::new();
        while extra.is_empty() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let Some(frame) = self.read_frame(remaining).await else {
                break;
            };
            if frame.get("id").is_some() || frame["method"] != EVENT_METHOD {
                continue;
            }
            let event = &frame["params"];
            if event["task_id"] != task_id || event["event"] != "task_message" {
                continue;
            }
            let kind: MessageKind =
                serde_json::from_value(event["kind"].clone()).expect("decode kind");
            let body = event["body"].as_str().expect("body is a string").to_string();
            extra.push((kind, body));
        }
        extra
    }
}

/// What one run pushed over the live stream before its terminal.
#[derive(Debug, Default)]
struct LiveRun {
    /// Every `TaskMessage` `(kind, body)`, in arrival order.
    transcript: Vec<(MessageKind, String)>,
    /// The tool count off the LAST `TaskProgress`: the run's final tally, since
    /// the producer emits a closing tick at stdout EOF.
    last_tool_calls: Option<u64>,
    /// Every event tag seen on the connection, ANY task, in arrival order. Only
    /// read when an assertion fails: it separates "the producer emitted nothing"
    /// from "the subscription went live too late to see it".
    seen: Vec<String>,
}
