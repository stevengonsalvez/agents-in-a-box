//! Integration: the `comment_add` RPC now REPORTS what happened to every
//! `@`-mention, and `comment_mention_preview` reports the same thing without
//! writing (multica parity #2-rest), over a real framed `UnixStream`.
//!
//! The sibling `rpc_comment_mention_spawn.rs` pins the pre-2-rest behaviour (a
//! mention spawns a task; a denied one spawns nothing). This file pins the part
//! that did not exist before: the per-target OUTCOME CODES on the wire.
//!
//! Each test asserts BOTH the side effect and the surfaced code, because
//! either one alone can pass while the feature is broken — the campaign's
//! recurring failure mode.
//!
//! RED against `main`: every assertion here reads `result.mention_outcomes`,
//! a key `main`'s `comment_add` reply does not contain at all.

#![allow(clippy::too_many_lines)]

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
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

/// Add `user_id` to the fixture workspace as a plain (non-owner) member.
async fn seed_plain_member(store: &Store, user_id: &str) {
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, 0)")
        .bind(user_id)
        .bind(format!("{user_id}@example.com"))
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'member')")
        .bind(WS_ID)
        .bind(user_id)
        .execute(store.pool())
        .await
        .unwrap();
}

/// The `mention_outcomes` array off a `comment_add` / preview reply.
fn outcomes(resp: &serde_json::Value) -> &Vec<serde_json::Value> {
    resp["result"]["mention_outcomes"]
        .as_array()
        .unwrap_or_else(|| panic!("no mention_outcomes on the reply: {resp}"))
}

/// **ACCEPTANCE 1** — `@`-mentioning a HUMAN routes to that human: zero tasks,
/// one inbox entry addressed to them, and a `notified` outcome on the wire.
#[tokio::test]
async fn mentioning_a_human_routes_to_that_human_not_an_agent() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_plain_member(&store, "bob").await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c.comment("issue-3", "[@Bob](mention://member/bob) can you look?").await;
    // The comment itself still acks exactly as before (the result FLATTENS the
    // comment row, so an old client's parse is unaffected).
    assert_eq!(resp["result"]["issue_id"], "issue-3");

    let rows = outcomes(&resp);
    assert_eq!(rows.len(), 1, "one row per target: {resp}");
    assert_eq!(rows[0]["target_type"], "member");
    assert_eq!(rows[0]["target_id"], "bob");
    assert_eq!(rows[0]["outcome"], "notified");

    // A human mention is NEVER a run.
    let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
        .bind("issue-3")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(tasks, 0, "a human mention spawns no task");

    let inbox: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_entry \
         WHERE recipient_type = 'member' AND recipient_id = 'bob' \
           AND event = 'mention' AND subject_id = 'issue-3'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(inbox, 1, "the mention landed in that human's inbox");

    let subscribed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM issue_subscriber \
         WHERE issue_id = 'issue-3' AND actor_type = 'member' \
           AND actor_id = 'bob' AND reason = 'mentioned'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(subscribed, 1, "being @-mentioned subscribes you");
}

/// **ACCEPTANCE 2** — a re-mention reports `coalesced` rather than failing
/// silently, and the pending task is re-pointed at the NEWER comment.
#[tokio::test]
async fn re_mentioning_a_pending_agent_reports_coalesced_and_repoints_the_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let first = c.comment("issue-3", "@claude-agent go").await;
    assert_eq!(outcomes(&first)[0]["outcome"], "queued", "{first}");
    let task_id = outcomes(&first)[0]["task_id"].as_str().unwrap().to_string();

    let second = c.comment("issue-3", "@claude-agent again").await;
    assert_eq!(outcomes(&second)[0]["outcome"], "coalesced", "{second}");
    assert_eq!(outcomes(&second)[0]["reason"], "coalesced");
    assert_eq!(
        task_count(&store, "agent-1", "issue-3").await,
        1,
        "the second mention folded into the pending task"
    );

    let second_comment_id = second["result"]["id"].as_str().unwrap();
    let trigger: Option<String> =
        sqlx::query_scalar("SELECT trigger_comment_id FROM agent_task_queue WHERE id = ?")
            .bind(&task_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(
        trigger.as_deref(),
        Some(second_comment_id),
        "merge-into-pending re-points the task at the newer comment"
    );
}

/// **ACCEPTANCE 3** — a private, non-allowed agent is refused WITH A CODE.
/// Against `main` the identical call answers with no `mention_outcomes` at all.
#[tokio::test]
async fn a_private_non_allowed_agent_is_refused_with_a_code() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_plain_member(&store, "bob").await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c.comment_as("member:bob", "issue-3", "@claude-agent go").await;
    let rows = outcomes(&resp);
    assert_eq!(rows.len(), 1, "{resp}");
    assert_eq!(rows[0]["outcome"], "blocked");
    assert_eq!(rows[0]["reason"], "invocation_not_allowed");
    assert_eq!(
        task_count(&store, "agent-1", "issue-3").await,
        0,
        "a refused mention writes no task"
    );
}

/// The preview reports the SAME codes the write then produces, and writes
/// NOTHING — asserted by snapshotting every table the router can touch.
#[tokio::test]
async fn the_preview_matches_the_write_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_plain_member(&store, "bob").await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    async fn snapshot(store: &Store) -> Vec<(&'static str, i64)> {
        let mut out = Vec::new();
        for table in [
            "comment",
            "agent_task_queue",
            "inbox_entry",
            "issue_subscriber",
            "dispatch_attempt",
        ] {
            let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(store.pool())
                .await
                .unwrap();
            out.push((table, n));
        }
        out
    }

    let body = "[@Bob](mention://member/bob) and @claude-agent go";
    let before = snapshot(&store).await;
    let preview = c
        .call(
            methods::HANGAR_COMMENT_MENTION_PREVIEW,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "issue-3",
                "author": "member:user-1",
                "body": body,
            }),
        )
        .await;
    assert!(preview["error"].is_null(), "preview must ack: {preview}");
    assert_eq!(
        snapshot(&store).await,
        before,
        "a preview writes NOTHING, in any table"
    );

    let written = c.comment("issue-3", body).await;
    let codes = |resp: &serde_json::Value| {
        outcomes(resp)
            .iter()
            .map(|r| {
                (
                    r["target_type"].as_str().unwrap_or("").to_string(),
                    r["target_id"].as_str().unwrap_or("").to_string(),
                    r["outcome"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        codes(&preview),
        codes(&written),
        "preview {preview} must match write {written}"
    );
}

/// The preview applies the private-agent gate IDENTICALLY, so it can never leak
/// a private agent's readiness to a caller who may not invoke it.
#[tokio::test]
async fn the_preview_applies_the_private_gate_identically() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_plain_member(&store, "bob").await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let preview = c
        .call(
            methods::HANGAR_COMMENT_MENTION_PREVIEW,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "issue-3",
                "author": "member:bob",
                "body": "@claude-agent go",
            }),
        )
        .await;
    let rows = outcomes(&preview);
    assert_eq!(rows.len(), 1, "{preview}");
    assert_eq!(rows[0]["outcome"], "blocked");
    assert_eq!(rows[0]["reason"], "invocation_not_allowed");
}

/// A mistyped workspace is `INVALID_PARAMS` on the preview too — never a
/// silently empty preview a caller would read as "this mentions nobody".
#[tokio::test]
async fn a_mistyped_workspace_rejects_rather_than_previewing_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c
        .call(
            methods::HANGAR_COMMENT_MENTION_PREVIEW,
            serde_json::json!({
                "workspace_id": "not-a-workspace",
                "issue_id": "issue-3",
                "author": "member:user-1",
                "body": "@claude-agent go",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "a typo'd workspace must be rejected, not silently empty: {resp}"
    );
}

/// A comment that mentions NOBODY on an unassigned issue reports no outcomes at
/// all, and the reply is byte-shaped exactly as it was pre-2-rest (the array is
/// omitted, not present-and-empty).
#[tokio::test]
async fn a_comment_with_no_mentions_reports_no_outcomes_and_keeps_the_old_shape() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // issue-3 is unassigned, so there is nothing to fall back to either.
    let resp = c.comment("issue-3", "just a plain remark").await;
    assert!(
        resp["result"].get("mention_outcomes").is_none(),
        "an empty outcome set is OMITTED, keeping the wire byte-identical: {resp}"
    );
    assert_eq!(resp["result"]["body"], "just a plain remark");
}

/// One denied handle produces its own row and never suppresses the allowed one:
/// two outcome rows, exactly one task.
#[tokio::test]
async fn one_denied_handle_never_suppresses_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_plain_member(&store, "bob").await;
    // A second agent bob may NOT invoke, alongside claude-agent which he may.
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-priv', ?, 'private-bot', 'runtime-1', 'workspace', 'user-1')",
    )
    .bind(WS_ID)
    .execute(store.pool())
    .await
    .unwrap();
    {
        use ainb_hangar_core::clock::SystemClock;
        use ainb_hangar_core::idgen::SystemIdGen;
        use ainb_hangar_store::repo::agent::AgentRepo;
        use ainb_hangar_store::repo::agent_invocation_target::AgentInvocationTargetRepo;
        AgentRepo::set_permission_mode(store.pool(), "agent-1", "public_to")
            .await
            .unwrap();
        AgentInvocationTargetRepo::add(
            store.pool(),
            &SystemIdGen,
            &SystemClock,
            "agent-1",
            "member",
            "bob",
            None,
        )
        .await
        .unwrap();
    }

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    let resp = c.comment_as("member:bob", "issue-3", "@private-bot and @claude-agent go").await;
    let rows = outcomes(&resp);
    assert_eq!(rows.len(), 2, "one row per target: {resp}");
    assert_eq!(rows[0]["outcome"], "blocked", "{resp}");
    assert_eq!(rows[1]["outcome"], "queued", "{resp}");
    assert_eq!(task_count(&store, "agent-1", "issue-3").await, 1);
    assert_eq!(task_count(&store, "agent-priv", "issue-3").await, 0);
}

/// A reply whose parent was authored by an AGENT and which mentions nobody
/// falls back to that agent (multica's reply-parent leg), tagged `reply_parent`.
#[tokio::test]
async fn a_mentionless_reply_falls_back_to_the_parents_agent_author() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // The agent posts first...
    let parent = c.comment_as("agent:agent-1", "issue-3", "here is what I found").await;
    let parent_id = parent["result"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        task_count(&store, "agent-1", "issue-3").await,
        0,
        "an agent's own comment never triggers itself"
    );

    // ...and a human replies with no mention at all.
    let reply = c
        .call(
            methods::HANGAR_COMMENT_ADD,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "issue-3",
                "author": "member:user-1",
                "body": "thanks, please continue",
                "parent_id": parent_id,
            }),
        )
        .await;
    assert!(reply["error"].is_null(), "reply must ack: {reply}");
    assert_eq!(
        reply["result"]["parent_id"], parent_id,
        "the reply threads under its parent: {reply}"
    );
    let rows = outcomes(&reply);
    assert_eq!(rows.len(), 1, "{reply}");
    assert_eq!(rows[0]["source"], "reply_parent");
    assert_eq!(rows[0]["target_id"], "agent-1");
    assert_eq!(rows[0]["outcome"], "queued");
    assert_eq!(task_count(&store, "agent-1", "issue-3").await, 1);
}
