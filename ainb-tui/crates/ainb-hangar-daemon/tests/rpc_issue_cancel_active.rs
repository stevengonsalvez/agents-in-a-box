//! Integration: the `hangar/issue_cancel_active` RPC cancels every active task on
//! one issue over a real framed `UnixStream`, WITHOUT any board coordinates — the
//! board-less "cancel the run(s) & delete" affordance.
//!
//! The end-to-end story: an issue with a live task refuses `issue_delete` with a
//! machine-readable `data.reason = "active_tasks"` marker; `issue_cancel_active`
//! then clears the run (`{ cancelled: 1 }`), after which the same `issue_delete`
//! succeeds. Also asserts a no-active-task issue is a clean `{ cancelled: 0 }` and
//! a mistyped workspace is rejected, never a silent cancel.

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

/// Enqueue a task on `issue_id` forced to `status` (an active status), returning
/// nothing — the row exists after this.
async fn seed_active_task(store: &Store, task_id: &str, issue_id: &str, status: &str) {
    let (runtime_id, agent_id): (String, String) =
        sqlx::query_as("SELECT a.runtime_id, a.id FROM agent a WHERE a.workspace_id = ? LIMIT 1")
            .bind(WS_ID)
            .fetch_one(store.pool())
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(task_id)
    .bind(WS_ID)
    .bind(&runtime_id)
    .bind(&agent_id)
    .bind(issue_id)
    .bind(status)
    .execute(store.pool())
    .await
    .unwrap();
}

/// The full board-less flow: a live task blocks delete (with a machine-readable
/// `active_tasks` marker), `issue_cancel_active` cancels it, and the retried
/// delete then succeeds.
#[tokio::test]
async fn cancel_active_unblocks_a_delete_refused_for_active_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_active_task(&store, "t-live", "issue-2", "running").await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // 1. The delete is refused, and the error carries the machine-readable marker
    //    the TUI keys its "cancel & delete" offer off.
    let refused = c
        .call(
            methods::HANGAR_ISSUE_DELETE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-2" }),
        )
        .await;
    assert!(
        !refused["error"].is_null(),
        "active task must refuse delete: {refused}"
    );
    assert_eq!(
        refused["error"]["data"]["reason"], "active_tasks",
        "refusal carries the active_tasks marker: {refused}"
    );
    assert_eq!(
        refused["error"]["data"]["active"], 1,
        "one active task reported"
    );

    // 2. Cancel the active run(s) for the issue — no board coordinates.
    let cancelled = c
        .call(
            methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-2" }),
        )
        .await;
    assert!(cancelled["error"].is_null(), "cancel must ack: {cancelled}");
    assert_eq!(cancelled["result"]["cancelled"], 1, "one task cancelled");

    // The task row is now terminal (`cancelled`), so nothing is active.
    let status: String =
        sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = 't-live'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(status, "cancelled", "the run is cancelled in the store");

    // 3. The retried delete now succeeds and pushes issue_deleted.
    let deleted = c
        .call(
            methods::HANGAR_ISSUE_DELETE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-2" }),
        )
        .await;
    assert!(
        deleted["error"].is_null(),
        "delete after cancel must ack: {deleted}"
    );

    // The issue is gone from a fresh snapshot.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let issues = list["result"]["issues"].as_array().unwrap();
    assert!(
        issues.iter().all(|i| i["id"] != "issue-2"),
        "issue-2 deleted after the runs were cancelled"
    );
}

/// An issue with no active task is a clean `{ cancelled: 0 }` — a no-op the caller
/// surfaces as a note, never an error.
#[tokio::test]
async fn cancel_active_is_a_clean_noop_with_no_active_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // issue-3 carries no task in the fixture.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-3" }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "a no-active-task cancel is not an error: {resp}"
    );
    assert_eq!(resp["result"]["cancelled"], 0, "nothing to cancel");
}

/// A mistyped / foreign workspace is rejected — never a silent cross-tenant
/// cancel.
#[tokio::test]
async fn cancel_active_rejects_unknown_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_active_task(&store, "t-live", "issue-1", "running").await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            serde_json::json!({ "workspace_id": "nope-not-a-workspace", "issue_id": "issue-1" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must be rejected, not a silent cancel: {resp}"
    );

    // The task is untouched (still active) under its real workspace.
    let status: String =
        sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = 't-live'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(status, "running", "rejected cancel left the run running");
}

/// EVERY active sibling on one issue is cancelled, not just the newest.
///
/// The cancel path used to resolve a SINGLE task (`LIMIT 1`), so with several
/// concurrent runs on one card it cancelled the newest and left the rest burning
/// tokens, which later re-moved the "cancelled" card.
///
/// This property was previously proven end-to-end by
/// `tripwire_tcp_squad_card_fanout_e2e`, whose three live siblings came from the
/// squad BROADCAST. A squad card now carries ONE run, so that tripwire can no
/// longer build the case, and the coverage is re-homed here rather than being
/// allowed to evaporate along with the defect. Several concurrent runs on one
/// card are still reachable deliberately, via `--redundant N`.
#[tokio::test]
async fn cancel_active_cancels_every_sibling_not_just_the_newest() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // Three concurrent runs on ONE issue, in the three states the cancel must
    // sweep, seeded oldest-first so a LIMIT 1 resolver would take `t-newest`.
    //
    // Each sits on its OWN agent, which is both the real `--redundant` shape and
    // a hard requirement: `idx_one_pending_task_per_issue_agent` (migration 0012)
    // forbids two PENDING rows per (issue, agent), so seeding all three on one
    // agent trips a UNIQUE violation rather than building the case.
    let (runtime_id, base_agent): (String, String) =
        sqlx::query_as("SELECT a.runtime_id, a.id FROM agent a WHERE a.workspace_id = ? LIMIT 1")
            .bind(WS_ID)
            .fetch_one(store.pool())
            .await
            .unwrap();
    for (task_id, agent_suffix, status) in [
        ("t-oldest", "sib-a", "running"),
        ("t-middle", "sib-b", "dispatched"),
        ("t-newest", "sib-c", "queued"),
    ] {
        let agent_id = format!("{base_agent}-{agent_suffix}");
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             SELECT ?1, workspace_id, ?1, runtime_id, visibility, owner_id \
               FROM agent WHERE id = ?2",
        )
        .bind(&agent_id)
        .bind(&base_agent)
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at, run_group) \
             VALUES (?1, ?2, ?3, ?4, 'issue-2', ?5, 0, 'rg-1')",
        )
        .bind(task_id)
        .bind(WS_ID)
        .bind(&runtime_id)
        .bind(&agent_id)
        .bind(status)
        .execute(store.pool())
        .await
        .unwrap();
    }

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let cancelled = c
        .call(
            methods::HANGAR_ISSUE_CANCEL_ACTIVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": "issue-2" }),
        )
        .await;
    assert!(
        cancelled["error"].is_null(),
        "cancel_active must ack: {cancelled}"
    );

    let states: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, status FROM agent_task_queue WHERE issue_id = 'issue-2' ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(states.len(), 3, "all three rows survive as rows");
    for (id, status) in &states {
        assert_eq!(
            status, "cancelled",
            "sibling {id} must be cancelled, not left active: {states:?}"
        );
    }

    let still_active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_task_queue \
          WHERE issue_id = 'issue-2' AND status IN ('queued','dispatched','running')",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(still_active, 0, "no sibling may be left burning tokens");
}
