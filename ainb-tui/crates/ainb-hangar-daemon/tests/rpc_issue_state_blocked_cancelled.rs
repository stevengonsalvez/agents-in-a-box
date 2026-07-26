//! Integration: an issue can MOVE to `blocked` / `cancelled`, the move PERSISTS,
//! and the moved row still reaches the client — over a real framed `UnixStream`
//! against a real sqlite (multica gap #19).
//!
//! Three distinct failure modes are pinned here, all of which a reply-only
//! assertion would miss:
//!
//! 1. **The snapshot union.** `hangar/issues_list` lists issues PER STATE and
//!    concatenates. A state missing from `ISSUE_STATES` means the row never
//!    reaches the client at all — it vanishes from the board rather than landing
//!    in the wrong column. Revert that edit and
//!    [`blocked_and_cancelled_move_persist_and_stay_in_the_snapshot`] goes red
//!    on both the row lookup AND the row count.
//! 2. **Persistence vs the reply.** Every move is re-read with raw SQL against
//!    the same pool, so a handler that answered optimistically without writing
//!    could not pass.
//! 3. **A cancelled card never dispatches.** `hangar/issue_run` — the board-less
//!    sibling that shares `run_card`'s launch core — refuses it with a
//!    message distinct from the dependency-blocked refusal, and the assertion
//!    counts task rows — zero written — rather than trusting the error text.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// The seeded issue every test moves around.
const ISSUE: &str = "issue-1";

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
            id: RpcId::Number(11),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
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

    /// Send `method` and return the first frame carrying an `id` (interleaved
    /// event notifications are discarded).
    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.read_frame_inner())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 5s"));
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

    /// Move [`ISSUE`] to `state` and return the raw reply frame.
    async fn move_issue(&mut self, state: &str) -> serde_json::Value {
        self.call(
            methods::HANGAR_ISSUE_UPDATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": ISSUE,
                "state": state,
            }),
        )
        .await
    }

    /// The `hangar/issues_list` snapshot rows the CLIENT actually receives.
    async fn issues_snapshot(&mut self) -> Vec<serde_json::Value> {
        let list = self
            .call(
                methods::HANGAR_ISSUES_LIST,
                serde_json::json!({ "workspace_id": WS_SLUG }),
            )
            .await;
        assert!(list["error"].is_null(), "issues_list must ack: {list}");
        list["result"]["issues"].as_array().cloned().unwrap_or_default()
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

/// The STORED state, read with raw SQL — the reply is not evidence of a write.
async fn stored_state(store: &Store, issue_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM issue WHERE id = ?")
        .bind(issue_id)
        .fetch_one(store.pool())
        .await
        .expect("read issue state")
}

/// Both new states move, persist to the DB, AND survive the per-state snapshot
/// union the client reads.
#[tokio::test]
async fn blocked_and_cancelled_move_persist_and_stay_in_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // How many issues the board carries BEFORE any move — a row that vanishes
    // from the union must not be able to pass as "not found is fine".
    let baseline = c.issues_snapshot().await.len();
    assert!(baseline > 0, "the seed lays down at least one issue");

    for state in ["blocked", "cancelled"] {
        let resp = c.move_issue(state).await;
        assert!(resp["error"].is_null(), "move to {state} must ack: {resp}");
        assert_eq!(resp["result"]["state"], state, "the reply carries the move");

        // PERSISTENCE: the row really changed in sqlite, not just in the reply.
        assert_eq!(
            stored_state(&store, ISSUE).await,
            state,
            "move to {state} must persist to the database"
        );

        // THE UNION: the row is still in the snapshot the client receives. With
        // `{state}` missing from ISSUE_STATES the row disappears entirely.
        let rows = c.issues_snapshot().await;
        assert_eq!(
            rows.len(),
            baseline,
            "a {state} issue must not vanish from the board (rows={rows:?})"
        );
        let row = rows
            .iter()
            .find(|i| i["id"] == ISSUE)
            .unwrap_or_else(|| panic!("{ISSUE} missing from the snapshot after moving to {state}"));
        assert_eq!(row["state"], state, "the snapshot carries the moved state");
    }
}

/// A non-canonical state is rejected at the RPC boundary with INVALID_PARAMS,
/// and the stored state is untouched (no partial write).
#[tokio::test]
async fn a_non_canonical_state_is_rejected_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    c.move_issue("cancelled").await;
    assert_eq!(stored_state(&store, ISSUE).await, "cancelled");

    let resp = c.move_issue("nonsense").await;
    assert!(
        !resp["error"].is_null(),
        "a garbage state must be refused: {resp}"
    );
    // JSON-RPC INVALID_PARAMS — a clean 400-equivalent, not the internal-error
    // a migration-0049 trigger ABORT would bubble up as.
    assert_eq!(
        resp["error"]["code"], -32602,
        "refused as INVALID_PARAMS, not an internal store error: {resp}"
    );
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    for token in ["blocked", "cancelled"] {
        assert!(
            message.contains(token),
            "the refusal must enumerate the valid states, got {message:?}"
        );
    }

    assert_eq!(
        stored_state(&store, ISSUE).await,
        "cancelled",
        "a refused edit must not partially write"
    );
}

/// A CANCELLED card never dispatches: `hangar/issue_run` refuses it, the refusal
/// reads differently from the dependency-blocked one, and NO task row is
/// written.
#[tokio::test]
async fn a_cancelled_card_refuses_to_run_and_enqueues_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let tasks_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
            .bind(ISSUE)
            .fetch_one(store.pool())
            .await
            .expect("count tasks");

    c.move_issue("cancelled").await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_RUN,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": ISSUE,
                "mode": "headless",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "running a cancelled card must be refused: {resp}"
    );
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("cancelled"),
        "the refusal must say the card is cancelled, got {message:?}"
    );
    assert!(
        !message.contains("blocked by unfinished cards"),
        "the cancelled refusal must read differently from the dependency-blocked \
         one, got {message:?}"
    );

    let tasks_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
            .bind(ISSUE)
            .fetch_one(store.pool())
            .await
            .expect("count tasks");
    assert_eq!(
        tasks_after, tasks_before,
        "a refused run must enqueue no task row"
    );
}

/// A BLOCKED card stays runnable — `blocked` in hangar is a human annotation,
/// not the dependency gate (that is `card_dependency`). Whatever the run does
/// next, it must not be refused FOR BEING BLOCKED.
#[tokio::test]
async fn a_blocked_card_is_still_runnable() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    c.move_issue("blocked").await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_RUN,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": ISSUE,
                "mode": "headless",
            }),
        )
        .await;
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("cancelled"),
        "a blocked card must not be refused as cancelled, got {message:?}"
    );
}
