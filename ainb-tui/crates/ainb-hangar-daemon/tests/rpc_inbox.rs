//! Integration: the notification inbox over a real framed `UnixStream` (e38.14).
//!
//! This is the user-visible proof leg for the bead. It drives the WHOLE
//! data-plane path the way `boot()` wires it:
//!
//! 1. start the real RPC server over a seeded store, sharing ONE event broker
//!    with the real inbox aggregator (`inbox_aggregator::spawn`) — the same seam
//!    `boot()` uses;
//! 2. emit live issue / comment / task `HangarEvent`s through the broker's sink
//!    (the mutation paths' publishing handle), exactly as a committed mutation
//!    does;
//! 3. assert over the socket that `hangar/inbox_list` aggregated them into the
//!    inbox with the correct UNREAD count;
//! 4. call `hangar/inbox_mark_read` and assert it set `read_at` and the unread
//!    count dropped to 0 — and that mark-read is rejected for an unknown
//!    workspace (never a silent no-op).
//!
//! The aggregation is therefore proven to TAKE EFFECT: events that fired land
//! durably, and the mark-read sweep actually flips the unread count. Mutating the
//! sweep into a no-op (returning before the UPDATE) breaks
//! `mark_read_clears_unread_count`.

use std::time::{Duration, Instant};

use ainb_hangar_core::ids::{CommentId, IssueId, TaskId};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::events::{CommentRow, HangarEvent, IssueRow, TaskResult};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// The workspace OWNER the P4 fixture seeds (`member` role `owner`, user-1).
/// Events whose participants cannot be derived fall back to this actor, so it is
/// the recipient the fixture's issue / comment / task events land on.
const OWNER: &str = "member:user-1";

/// The local human — the recipient an omitted `recipient` param defaults to.
const LOCAL_HUMAN: &str = "member:me";

/// The agent the P4 fixture assigns `issue-1` to. A comment BY the owner on that
/// issue is routed to this actor (its other participant).
const FIXTURE_AGENT: &str = "agent:agent-1";

/// One test client connection: a persistent buffered reader + writer half.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(socket_path: &std::path::Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(socket_path).await {
                Ok(c) => break c,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("never connected: {e}"),
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

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

    async fn read_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        tokio::time::timeout(timeout, self.read_frame_inner()).await.ok()
    }

    async fn read_frame_inner(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;
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

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        loop {
            let frame = self
                .read_frame(Duration::from_secs(5))
                .await
                .unwrap_or_else(|| panic!("no response to {method} within 5s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn auth_from_file(&mut self, dir: &std::path::Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
        let resp = self
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
    }

    /// List ONE ACTOR's inbox via RPC, returning `(entries, unread)`.
    async fn inbox_list(&mut self, ws: &str, recipient: &str) -> (Vec<serde_json::Value>, i64) {
        let resp = self
            .call(
                methods::HANGAR_INBOX_LIST,
                serde_json::json!({ "workspace_id": ws, "recipient": recipient }),
            )
            .await;
        assert!(resp["error"].is_null(), "inbox_list must ack: {resp}");
        let entries = resp["result"]["entries"].as_array().cloned().unwrap_or_default();
        let unread = resp["result"]["unread"].as_i64().expect("unread is an int");
        (entries, unread)
    }
}

/// Bind + serve the real listener over the seeded store, sharing ONE broker with
/// the real inbox aggregator (mirrors `boot()`'s wiring). Returns the socket path,
/// the live store, and a sink to emit events through.
async fn start_server_with_aggregator(
    dir: &std::path::Path,
) -> (
    std::path::PathBuf,
    Store,
    ainb_hangar_daemon::events::EventSink,
) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");

    // One broker shared between the aggregator (subscribes) and the emit sink we
    // hand back to the test — exactly how `boot()` wires it. Dropping the returned
    // JoinHandle detaches the task (tokio keeps it running), exactly as `boot()`
    // does with the `_inbox` handle, so the aggregator drains for the test's life.
    let broker = EventBroker::new();
    drop(ainb_hangar_daemon::inbox_aggregator::spawn(
        store.pool().clone(),
        broker.subscribe(),
    ));

    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    let sink = broker.sink();
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    (socket_path, store, sink)
}

/// Emit one of each aggregatable family (issue / comment / task) through the sink
/// and wait until the aggregator has written all three (the inbox count reaches
/// the expected total). Returns once aggregated or panics on timeout.
async fn wait_for_inbox_count(store: &Store, ws_id: &str, want: i64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbox_entry WHERE workspace_id = ?")
            .bind(ws_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
        if n >= want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "inbox did not reach {want} rows (saw {n}) within 5s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn issue_event() -> HangarEvent {
    HangarEvent::IssueCreated(IssueRow {
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str("issue-1").unwrap(),
        display_id: None,
        workspace_id: WS_ID.into(),
        title: "Refactor API".into(),
        description: None,
        state: "open".into(),
        assignee: None,
        creator: "member:user-1".into(),
        created_at: 1_000,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    })
}

fn comment_event() -> HangarEvent {
    HangarEvent::CommentAdded(CommentRow {
        id: CommentId::from_str("c-1").unwrap(),
        issue_id: IssueId::from_str("issue-1").unwrap(),
        author: "member:user-1".into(),
        body: "lgtm".into(),
        created_at: 2_000,
    })
}

fn task_event() -> HangarEvent {
    HangarEvent::TaskFinished {
        task_id: TaskId::from_str("t-1").unwrap(),
        result: TaskResult::Success,
        ended_at: chrono::DateTime::from_timestamp_millis(3_000).unwrap(),
    }
}

/// Emitting issue / comment / task events aggregates them into the inbox, and
/// `hangar/inbox_list` reports them with the correct unread count.
#[tokio::test]
async fn events_aggregate_into_inbox_with_unread_count() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store, sink) = start_server_with_aggregator(dir.path()).await;

    // Emit one of each aggregatable family through the broker — exactly what a
    // committed mutation path does.
    sink.emit(WS_ID, issue_event());
    sink.emit(WS_ID, comment_event());
    sink.emit(WS_ID, task_event());

    wait_for_inbox_count(&store, WS_ID, 3).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // The owner is notified of the issue they created and of the task on it; the
    // comment they wrote goes to the issue's OTHER participant, the assigned
    // agent (you are never notified of your own action). Assert per recipient
    // rather than on one flat workspace list.
    let (mine, mine_unread) = c.inbox_list(WS_SLUG, OWNER).await;
    let (theirs, theirs_unread) = c.inbox_list(WS_SLUG, FIXTURE_AGENT).await;
    assert_eq!(
        mine_unread,
        i64::try_from(mine.len()).unwrap(),
        "every fresh entry is unread: {mine:?}"
    );
    assert_eq!(
        theirs_unread,
        i64::try_from(theirs.len()).unwrap(),
        "every fresh entry is unread: {theirs:?}"
    );
    assert_eq!(
        mine_unread + theirs_unread,
        3,
        "all three events aggregated across the two recipients"
    );

    // The kinds cover all three families across the two inboxes, and every entry
    // names the actor it is addressed to.
    let all: Vec<&serde_json::Value> = mine.iter().chain(theirs.iter()).collect();
    let kinds: std::collections::HashSet<&str> =
        all.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(kinds.contains("issue"), "an issue entry landed: {kinds:?}");
    assert!(
        kinds.contains("comment"),
        "a comment entry landed: {kinds:?}"
    );
    assert!(kinds.contains("task"), "a task entry landed: {kinds:?}");
    assert!(
        mine.iter().all(|e| e["recipient"] == OWNER),
        "the owner's list carries only their own entries: {mine:?}"
    );
    assert!(
        theirs.iter().all(|e| e["recipient"] == FIXTURE_AGENT),
        "the agent's list carries only its own entries: {theirs:?}"
    );
    assert_eq!(
        theirs.iter().filter(|e| e["kind"] == "comment").count(),
        1,
        "the owner's comment is routed to the assigned agent: {theirs:?}"
    );

    // Every fresh entry is unread (no read_at key on the wire).
    assert!(
        all.iter().all(|e| e.get("read_at").is_none()),
        "fresh entries carry no read_at: {all:?}"
    );
}

/// `hangar/inbox_mark_read` sets `read_at` and the unread count drops to 0.
///
/// MUTATION GUARD: making the sweep a no-op (returning before the UPDATE) makes
/// the `unread == 0` assertion fail — the proof that mark-read actually takes
/// effect.
#[tokio::test]
async fn mark_read_clears_unread_count() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store, sink) = start_server_with_aggregator(dir.path()).await;

    sink.emit(WS_ID, issue_event());
    sink.emit(WS_ID, comment_event());
    wait_for_inbox_count(&store, WS_ID, 2).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Before: the owner has the issue entry unread (their comment went to the
    // issue's other participant, the assigned agent).
    let (_, unread_before) = c.inbox_list(WS_SLUG, OWNER).await;
    assert_eq!(unread_before, 1, "the owner's own entry is unread");
    let (_, agent_before) = c.inbox_list(WS_SLUG, FIXTURE_AGENT).await;
    assert_eq!(agent_before, 1, "the agent's entry is unread");

    // Mark read: the response reports both flipped and zero unread after.
    let resp = c
        .call(
            methods::HANGAR_INBOX_MARK_READ,
            serde_json::json!({ "workspace_id": WS_SLUG, "recipient": OWNER }),
        )
        .await;
    assert!(resp["error"].is_null(), "mark_read must ack: {resp}");
    assert_eq!(resp["result"]["marked"], 1, "the owner's entry flipped: {resp}");
    assert_eq!(resp["result"]["unread"], 0, "unread drops to 0: {resp}");

    // After: list reports zero unread, and every entry now carries a read_at.
    let (entries, unread_after) = c.inbox_list(WS_SLUG, OWNER).await;
    assert_eq!(unread_after, 0, "unread count is 0 after the sweep");
    assert!(
        entries.iter().all(|e| e["read_at"].is_i64()),
        "every entry has a read_at stamp after mark-read: {entries:?}"
    );

    // The read_at is persisted in the store (not just the wire) for the swept
    // recipient, and only for them.
    let null_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry \
         WHERE workspace_id = ? AND recipient_type = 'member' AND recipient_id = 'user-1' \
         AND read_at IS NULL",
    )
    .bind(WS_ID)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(null_count, 0, "no unread rows remain for the swept actor");
    let (_, agent_after) = c.inbox_list(WS_SLUG, FIXTURE_AGENT).await;
    assert_eq!(
        agent_after, 1,
        "the agent's unread survives another actor's sweep"
    );
}

/// `hangar/inbox_mark_read` rejects an unknown workspace (never a silent no-op).
#[tokio::test]
async fn mark_read_rejects_unknown_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store, _sink) = start_server_with_aggregator(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_INBOX_MARK_READ,
            serde_json::json!({ "workspace_id": "nope-not-a-workspace" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must be rejected, not a silent no-op: {resp}"
    );
}

/// Seed an issue created by the LOCAL HUMAN and assigned to `agent:a1`, so a
/// comment on it has two distinct participants to route between.
///
/// Written with raw SQL rather than the repo so the fixture states exactly the
/// `(creator, assignee)` pair the routing depends on. The recipient columns are
/// FK-less by design, so no `agent` row is required.
async fn seed_two_party_issue(store: &Store) {
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, \
         assignee_type, assignee_id, created_at) \
         VALUES ('i1', ?, 'Two-party issue', 'open', 'member', 'me', 'agent', 'a1', 1000)",
    )
    .bind(WS_ID)
    .execute(store.pool())
    .await
    .expect("seed two-party issue");
}

fn comment_on_i1(id: &str, author: &str) -> HangarEvent {
    HangarEvent::CommentAdded(CommentRow {
        id: CommentId::from_str(id).unwrap(),
        issue_id: IssueId::from_str("i1").unwrap(),
        author: author.into(),
        body: "over to you".into(),
        created_at: 2_000,
    })
}

/// THE ACCEPTANCE PROOF (multica parity #1): an inbox entry targets a specific
/// actor and only that actor sees it — over the real socket, through the real
/// aggregator.
///
/// MUTATION GUARD: dropping the recipient predicate from the store's `list` /
/// `mark_all_read` makes each actor see BOTH comments and makes the human's
/// sweep clear the agent's unread, failing steps 4-6.
#[tokio::test]
async fn inbox_entries_are_visible_only_to_their_recipient() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store, sink) = start_server_with_aggregator(dir.path()).await;
    seed_two_party_issue(&store).await;

    // The agent comments (the human hears about it), then the human comments
    // (the agent hears about it). Neither is notified of their own comment.
    sink.emit(WS_ID, comment_on_i1("c-agent", "agent:a1"));
    sink.emit(WS_ID, comment_on_i1("c-human", "member:me"));
    wait_for_inbox_count(&store, WS_ID, 2).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // 4. The human sees exactly the AGENT's comment.
    let (mine, mine_unread) = c.inbox_list(WS_SLUG, LOCAL_HUMAN).await;
    assert_eq!(mine.len(), 1, "the human sees exactly one entry: {mine:?}");
    assert_eq!(
        mine[0]["subject_id"], "c-agent",
        "the human is notified of the agent's comment, not their own: {mine:?}"
    );
    assert_eq!(mine[0]["recipient"], LOCAL_HUMAN, "{mine:?}");
    assert_eq!(mine_unread, 1);

    // 5. The agent sees exactly the HUMAN's comment.
    let (theirs, theirs_unread) = c.inbox_list(WS_SLUG, "agent:a1").await;
    assert_eq!(theirs.len(), 1, "the agent sees exactly one entry: {theirs:?}");
    assert_eq!(
        theirs[0]["subject_id"], "c-human",
        "the agent is notified of the human's comment: {theirs:?}"
    );
    assert_eq!(theirs[0]["recipient"], "agent:a1", "{theirs:?}");
    assert_eq!(theirs_unread, 1);

    // 6. The human's mark-read sweep does not cross actors.
    let resp = c
        .call(
            methods::HANGAR_INBOX_MARK_READ,
            serde_json::json!({ "workspace_id": WS_SLUG, "recipient": LOCAL_HUMAN }),
        )
        .await;
    assert!(resp["error"].is_null(), "mark_read must ack: {resp}");
    assert_eq!(resp["result"]["marked"], 1, "only the human's row flips: {resp}");
    assert_eq!(resp["result"]["unread"], 0, "{resp}");

    let (_, mine_after) = c.inbox_list(WS_SLUG, LOCAL_HUMAN).await;
    assert_eq!(mine_after, 0, "the human's inbox is clear");
    let (theirs_after, theirs_unread_after) = c.inbox_list(WS_SLUG, "agent:a1").await;
    assert_eq!(
        theirs_unread_after, 1,
        "the agent's unread survives another actor's sweep"
    );
    assert!(
        theirs_after.iter().all(|e| e.get("read_at").is_none()),
        "the agent's entry is still unread: {theirs_after:?}"
    );
}

/// A malformed `recipient` is `INVALID_PARAMS` — a typo must never silently
/// fall back to reading someone else's inbox.
#[tokio::test]
async fn inbox_list_rejects_a_malformed_recipient() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store, _sink) = start_server_with_aggregator(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    for bad in ["not-an-actor", "frog:a1", "member:"] {
        let resp = c
            .call(
                methods::HANGAR_INBOX_LIST,
                serde_json::json!({ "workspace_id": WS_SLUG, "recipient": bad }),
            )
            .await;
        assert!(
            !resp["error"].is_null(),
            "a malformed recipient {bad:?} must be rejected: {resp}"
        );
    }
}

/// Wire back-compat: a caller that omits `recipient` (a pre-0060 surface) reads
/// the LOCAL HUMAN's inbox — not the union of every actor's entries.
#[tokio::test]
async fn inbox_list_without_recipient_defaults_to_the_local_human() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store, sink) = start_server_with_aggregator(dir.path()).await;
    seed_two_party_issue(&store).await;

    sink.emit(WS_ID, comment_on_i1("c-agent", "agent:a1"));
    sink.emit(WS_ID, comment_on_i1("c-human", "member:me"));
    wait_for_inbox_count(&store, WS_ID, 2).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_INBOX_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert!(resp["error"].is_null(), "an omitted recipient must ack: {resp}");
    let entries = resp["result"]["entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        entries.len(),
        1,
        "an omitted recipient reads ONE actor's inbox, not everyone's: {entries:?}"
    );
    assert_eq!(entries[0]["recipient"], LOCAL_HUMAN, "{entries:?}");
    assert_eq!(resp["result"]["unread"], 1, "{resp}");
}
