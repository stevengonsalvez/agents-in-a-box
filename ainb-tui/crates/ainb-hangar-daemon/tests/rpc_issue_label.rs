//! Integration: the `hangar/issue_label_attach` / `hangar/issue_label_detach`
//! RPCs mutate an issue's labels over a real framed `UnixStream`, are
//! workspace-scoped, and push the matching `hangar/event` (`issue_updated`) to
//! subscribed connections (e38.10).
//!
//! The seed fixture lays down three open issues in `WS_ID` (all label-less);
//! here a connection authenticates, subscribes the workspace, attaches a label,
//! then asserts (a) the response carries the row with the new label, (b) a
//! follow-up `hangar/issues_list` shows the persisted chip, (c) the subscribed
//! connection received an `issue_updated` event, (d) a detach removes it, and
//! (e) the same attach aimed at a *foreign* workspace / cross-tenant issue is
//! rejected with an error and touches no row.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::events::EVENT_METHOD;
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

    async fn next_event(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let frame = self.read_frame(remaining).await?;
            if frame.get("id").is_none() && frame["method"] == EVENT_METHOD {
                return Some(frame["params"].clone());
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

/// Read the `labels` array for an issue out of a fresh `issues_list` snapshot.
fn labels_in_list(list: &serde_json::Value, issue_id: &str) -> Vec<String> {
    list["result"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == issue_id)
        .and_then(|i| i["labels"].as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// An authenticated, subscribed connection attaches then detaches a label on a
/// seeded issue: the response row carries the new chip, a follow-up snapshot
/// shows the persisted change, the connection receives the `issue_updated` push,
/// and the detach removes the chip again.
#[tokio::test]
async fn issue_label_attach_then_detach_persist_and_push_event() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    c.subscribe(WS_SLUG).await;

    // Attach `bug` to issue-1 (a label-less seeded issue).
    let resp = c
        .call(
            methods::HANGAR_ISSUE_LABEL_ATTACH,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "issue-1",
                "name": "bug",
                "color": "#ff0000",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "attach must ack: {resp}");
    // The response carries the refreshed IssueRow with the new label chip.
    let row = &resp["result"];
    assert_eq!(row["id"], "issue-1");
    let row_labels: Vec<String> = row["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert_eq!(row_labels, vec!["bug".to_string()], "row carries the chip");

    // The subscribed connection received the issue_updated push. Drain it BEFORE
    // issuing another request — `call()` discards interleaved notifications.
    let event = c
        .next_event(Duration::from_secs(5))
        .await
        .expect("a committed attach must push an issue_updated event");
    assert_eq!(event["event"], "issue_updated", "wrong event: {event}");
    assert_eq!(event["id"], "issue-1");

    // The attach persisted: a fresh issues_list snapshot shows the chip.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(
        labels_in_list(&list, "issue-1"),
        vec!["bug".to_string()],
        "attach persisted into the issues_list snapshot"
    );

    // Detach removes the chip again.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_LABEL_DETACH,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": "issue-1",
                "name": "bug",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "detach must ack: {resp}");
    // IssueRow.labels is `skip_serializing_if = Vec::is_empty`, so an empty label
    // set is OMITTED from the wire entirely (the key is absent, not `[]`).
    let row_labels: Vec<String> = resp["result"]["labels"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    assert!(
        row_labels.is_empty(),
        "detach cleared the chip: {row_labels:?}"
    );
    // Drain the detach's issue_updated push so it doesn't leak into the next list.
    let _ = c.next_event(Duration::from_secs(5)).await;

    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert!(
        labels_in_list(&list, "issue-1").is_empty(),
        "detach persisted (no chip in the snapshot)"
    );
}

/// A mistyped / unknown workspace is rejected with an error — never a silent
/// no-op — and the issue gains no label (mirrors `handle_issue_update`).
#[tokio::test]
async fn issue_label_attach_rejects_unknown_workspace_and_touches_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_ISSUE_LABEL_ATTACH,
            serde_json::json!({
                "workspace_id": "nope-not-a-workspace",
                "issue_id": "issue-1",
                "name": "bug",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must be rejected, not a silent no-op: {resp}"
    );

    // issue-1 gained no label under its real workspace.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    assert!(
        labels_in_list(&list, "issue-1").is_empty(),
        "rejected attach must not add a label"
    );
}

/// An issue id that exists in another tenant cannot be (de)labelled through this
/// workspace: the attach touches no row, and the issue gains no label (workspace
/// isolation — the bead's "no cross-tenant attach" requirement).
#[tokio::test]
async fn issue_label_attach_is_workspace_scoped_no_cross_tenant_attach() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // A second tenant workspace, real (so it resolves) but NOT owning issue-1.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("01HANGARFIXTUREWSB00000000")
        .bind("other")
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Aim issue-1 (owned by the seeded workspace) through the "other" workspace.
    let resp = c
        .call(
            methods::HANGAR_ISSUE_LABEL_ATTACH,
            serde_json::json!({
                "workspace_id": "other",
                "issue_id": "issue-1",
                "name": "bug",
            }),
        )
        .await;
    // The workspace resolves, but the (id, workspace) pair matches no row: the
    // handler reports a not-found error rather than labelling another tenant's row.
    assert!(
        !resp["error"].is_null(),
        "a cross-tenant issue id must not be labelled through another tenant: {resp}"
    );

    // issue-1 carries no label in its real workspace.
    let list = c
        .call(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    assert!(
        labels_in_list(&list, "issue-1").is_empty(),
        "cross-tenant attach must not add a label"
    );
}
