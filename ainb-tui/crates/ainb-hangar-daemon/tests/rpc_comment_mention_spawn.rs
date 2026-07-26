//! Integration: a comment that `@`-mentions a real agent spawns that agent's
//! task on the comment's issue, over a real framed `UnixStream` (e38.7).
//!
//! The seed fixture lays down the `claude-agent` agent (id `agent-1`) and three
//! open issues in `WS_ID`. Here a connection authenticates, subscribes, and posts
//! a comment `@claude-agent please do X` on `issue-3` through the real dispatcher.
//! After the comment commits, the `comment_add` path parses the mention, resolves
//! `claude-agent` to the workspace's agent, and enqueues a task bound to
//! `issue-3`.
//!
//! This is the framed-socket daemon proof leg for the bead. Two assertions carry
//! it:
//!  - POSITIVE: `@claude-agent` enqueues exactly one task for `agent-1` on
//!    `issue-3` (resolved by agent name, workspace-scoped, reusing `TaskRepo`).
//!  - NEGATIVE: an unknown `@nobody` handle AND a plain comment each enqueue
//!    NOTHING (no agent resolves, so no task spawns — an unknown handle is
//!    ignored, never an error).
//!  - GATED (gap #8): a mention by a member who may not invoke the resolved agent
//!    lands the comment but enqueues NOTHING; allow-listing that member makes the
//!    identical comment spawn exactly one task.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use sqlx::Row as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

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

    /// Send `method`, then drain frames until the response (id-bearing) lands,
    /// ignoring any interleaved event pushes.
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

    async fn subscribe(&mut self, workspace_id: &str) {
        let resp = self
            .call(
                methods::WORKSPACE_SUBSCRIBE,
                serde_json::json!({ "workspace_id": workspace_id }),
            )
            .await;
        assert!(resp["error"].is_null(), "subscribe must ack: {resp}");
    }

    async fn comment(&mut self, issue_id: &str, body: &str) -> serde_json::Value {
        self.comment_as("member:user-1", issue_id, body).await
    }

    /// Post a comment under an explicit AUTHOR ref — the gap #8 effective invoker
    /// the mention gate judges each `@`-target by.
    async fn comment_as(&mut self, author: &str, issue_id: &str, body: &str) -> serde_json::Value {
        let resp = self
            .call(
                methods::HANGAR_COMMENT_ADD,
                serde_json::json!({
                    "workspace_id": WS_SLUG,
                    "issue_id": issue_id,
                    "author": author,
                    "body": body,
                }),
            )
            .await;
        assert!(resp["error"].is_null(), "comment_add must ack: {resp}");
        resp
    }
}

/// Bind + serve the real listener over the seeded store (mirrors `boot()`).
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(
        listener,
        store.pool().clone(),
        health,
        ainb_hangar_daemon::events::EventBroker::new(),
    ));
    (socket_path, store)
}

/// Count the tasks enqueued for `agent_id` on `issue_id` in the seeded workspace.
async fn task_count(store: &Store, agent_id: &str, issue_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_task_queue \
         WHERE workspace_id = ?1 AND agent_id = ?2 AND issue_id = ?3",
    )
    .bind(WS_ID)
    .bind(agent_id)
    .bind(issue_id)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

/// POSITIVE leg: `@claude-agent` in a comment spawns exactly one task for that
/// agent (id `agent-1`), bound to the comment's issue (`issue-3`).
#[tokio::test]
async fn mentioning_a_real_agent_spawns_its_task_on_the_issue() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // issue-3 carries no seeded task (issue-1 already has the running fixture
    // task), so the spawn here is unambiguously the mention's doing.
    assert_eq!(
        task_count(&store, "agent-1", "issue-3").await,
        0,
        "issue-3 has no task before the mention"
    );

    let resp = c.comment("issue-3", "@claude-agent please do X").await;
    // The comment still persists + acks its row exactly as before.
    assert_eq!(resp["result"]["issue_id"], "issue-3");

    // The mention resolved `claude-agent` → agent-1 and enqueued its task on
    // issue-3, reusing the ordinary task-enqueue path.
    let count = task_count(&store, "agent-1", "issue-3").await;
    assert_eq!(
        count, 1,
        "a mention of claude-agent must enqueue exactly one task on issue-3"
    );

    // The enqueued task is a real issue task: queued, bound to the runtime the
    // agent runs on, with no autopilot link (it is a comment-triggered issue
    // task, not an autopilot tick).
    let row = sqlx::query(
        "SELECT status, runtime_id, autopilot_run_id FROM agent_task_queue \
         WHERE workspace_id = ?1 AND agent_id = 'agent-1' AND issue_id = 'issue-3'",
    )
    .bind(WS_ID)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "queued");
    assert_eq!(row.get::<String, _>("runtime_id"), "runtime-1");
    assert!(
        row.get::<Option<String>, _>("autopilot_run_id").is_none(),
        "a comment-triggered task carries no autopilot link"
    );
}

/// NEGATIVE leg: an unknown `@nobody` handle AND a plain comment each enqueue
/// nothing — an unmatched handle is silently ignored, never an error, and a
/// comment with no mention never spawns a task.
#[tokio::test]
async fn unknown_handle_or_plain_comment_spawns_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let tasks_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = 'issue-2'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(tasks_before, 0, "issue-2 starts with no task");

    // An unknown handle: resolves to no agent → no task, and the comment still acks.
    let resp = c.comment("issue-2", "@nobody can you help?").await;
    assert_eq!(resp["result"]["issue_id"], "issue-2");

    // A plain comment with no mention at all.
    let resp = c.comment("issue-2", "just thinking out loud, no pings").await;
    assert_eq!(resp["result"]["issue_id"], "issue-2");

    let tasks_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = 'issue-2'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(
        tasks_after, 0,
        "an unknown @handle and a plain comment must enqueue no task"
    );
}

/// Workspace scoping: a mention only resolves an agent that belongs to the
/// comment's workspace. A foreign-tenant agent sharing the same name does NOT
/// get a task spawned in this workspace.
#[tokio::test]
async fn mention_is_workspace_scoped_no_foreign_agent_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // A second tenant with an agent whose NAME collides with `claude-agent` but
    // whose id is foreign — it must never be the target of a mention in WS_ID.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("01HANGARFIXTUREWSB00000000")
        .bind("other")
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-2")
        .bind("bob@example.com")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("runtime-2")
    .bind("01HANGARFIXTUREWSB00000000")
    .bind("daemon-2")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-foreign")
    .bind("01HANGARFIXTUREWSB00000000")
    .bind("claude-agent")
    .bind("runtime-2")
    .bind("workspace")
    .bind("user-2")
    .execute(store.pool())
    .await
    .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    c.comment("issue-3", "@claude-agent please do X").await;

    // The local agent (agent-1) got the task; the foreign same-named agent did not.
    assert_eq!(
        task_count(&store, "agent-1", "issue-3").await,
        1,
        "the workspace's own claude-agent gets the task"
    );
    let foreign: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_task_queue WHERE agent_id = 'agent-foreign'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        foreign, 0,
        "a foreign-tenant agent sharing the name must never get a mention task"
    );
}

/// gap #8 (multica `resolveMentionedAgentCommentTriggers`): the mention gate over
/// the REAL framed socket + real sqlite. A plain workspace member `@`-mentioning a
/// PRIVATE agent they do not own gets their comment persisted (the trigger is
/// best-effort by design and must never fail the write) but NO task row is
/// enqueued. Allow-listing that member on the agent then makes the identical
/// comment spawn exactly one task — proving a decision, not blanket denial.
#[tokio::test]
async fn a_non_invocable_mention_lands_the_comment_but_spawns_no_task() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // A second, NON-owner member of the fixture workspace.
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('bob', 'bob@example.com', 0)")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, 'bob', 'member')")
        .bind(WS_ID)
        .execute(store.pool())
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // DENY: `agent-1` is private and owned by `user-1`, not by bob.
    let resp = c.comment_as("member:bob", "issue-3", "@claude-agent go").await;
    assert_eq!(
        resp["result"]["issue_id"], "issue-3",
        "the comment itself must still land"
    );
    let spawned: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = 'issue-3'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(
        spawned, 0,
        "a mention by a member who cannot invoke the agent enqueues NOTHING"
    );

    // ALLOW: share the agent with bob (public_to + a `member` target).
    sqlx::query("UPDATE agent SET permission_mode = 'public_to' WHERE id = 'agent-1'")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agent_invocation_target (id, agent_id, target_type, target_id, created_at) \
         VALUES ('ait-bob', 'agent-1', 'member', 'bob', 0)",
    )
    .execute(store.pool())
    .await
    .unwrap();

    c.comment_as("member:bob", "issue-3", "@claude-agent go again").await;
    let spawned = task_count(&store, "agent-1", "issue-3").await;
    assert_eq!(
        spawned, 1,
        "the allow-listed member's identical mention spawns exactly one task"
    );
}
