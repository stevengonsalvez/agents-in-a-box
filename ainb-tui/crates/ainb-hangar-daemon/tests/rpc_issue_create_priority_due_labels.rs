//! Integration (parity 28): `hangar/issue_create` persists the wizard-authored
//! `priority` / `due_date` / `labels` against a REAL sqlite store over a real
//! framed `UnixStream`.
//!
//! Before this, the daemon hardcoded `priority: 0`, `due_date: None`,
//! `labels: []` on every created issue — so every issue authored from the TUI
//! was P3, deadline-less and label-less no matter what the client sent.
//!
//! Asserted here: (1) the create response row echoes all three; (2) the `issue`
//! table matches; (3) labels land in the 0016 `label` / `issue_label` JOIN (the
//! source of truth), not merely the `issue.labels` JSON read-cache; (4) an
//! out-of-vocabulary priority is REJECTED rather than clamped (multica's
//! `validateIssueEnum` contract); (5) a payload omitting all three still creates
//! (old-client back-compat); (6) a duplicate label name yields exactly one join
//! row.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// `2026-08-01` at UTC midnight in epoch milliseconds.
const DUE_2026_08_01: i64 = 1_785_542_400_000;

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

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
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
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.read_frame())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 5s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn call_ok(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let resp = self.call(method, params).await;
        assert!(resp["error"].is_null(), "{method} must succeed: {resp}");
        resp["result"].clone()
    }

    async fn read_frame(&mut self) -> serde_json::Value {
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

/// The label NAMES joined to `issue_id` through the 0016 join tables, ordered by
/// name. Deliberately does NOT read `issue.labels` — the join is the source of
/// truth and the point of the assertion is that the create wrote it.
async fn joined_labels(store: &Store, issue_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT l.name FROM label l JOIN issue_label il ON il.label_id = l.id \
         WHERE il.issue_id = ? ORDER BY l.name",
    )
    .bind(issue_id)
    .fetch_all(store.pool())
    .await
    .expect("read joined labels")
}

/// A create carrying all three new attributes persists them: the response row,
/// the `issue` row, and the 0016 label join all agree.
#[tokio::test]
async fn issue_create_persists_priority_due_date_and_joined_labels() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let row = c
        .call_ok(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "ship the deadline",
                "creator": "member:user-1",
                "priority": 3,
                "due_date": DUE_2026_08_01,
                "labels": ["bug", "p0"],
            }),
        )
        .await;
    let id = row["id"].as_str().expect("minted issue id").to_string();

    // (1) the response row echoes what was persisted.
    assert_eq!(row["priority"], 3, "response priority: {row}");
    assert_eq!(row["due_date"], DUE_2026_08_01, "response due_date: {row}");
    assert_eq!(
        row["labels"],
        serde_json::json!(["bug", "p0"]),
        "response labels: {row}"
    );

    // (2) the stored row matches (not just the wire echo).
    let (priority, due_date): (i64, Option<i64>) =
        sqlx::query_as("SELECT priority, due_date FROM issue WHERE id = ?")
            .bind(&id)
            .fetch_one(store.pool())
            .await
            .expect("read stored issue");
    assert_eq!(priority, 3, "stored priority");
    assert_eq!(due_date, Some(DUE_2026_08_01), "stored due_date");

    // (3) labels went through the 0016 JOIN, not only the JSON read-cache.
    assert_eq!(
        joined_labels(&store, &id).await,
        vec!["bug".to_string(), "p0".to_string()],
        "labels must be attached through label/issue_label"
    );

    // A later snapshot shows the same chips (the create response is not special).
    let list = c
        .call_ok(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let listed = list["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == serde_json::Value::String(id.clone()))
        .expect("created issue in the snapshot")
        .clone();
    assert_eq!(listed["priority"], 3, "snapshot priority: {listed}");
    assert_eq!(listed["due_date"], DUE_2026_08_01, "snapshot due_date");
    assert_eq!(listed["labels"], serde_json::json!(["bug", "p0"]));
}

/// An out-of-vocabulary priority is a client error, NEVER a silent clamp to 3 —
/// multica's `validateIssueEnum` contract. No row is written.
#[tokio::test]
async fn issue_create_rejects_out_of_range_priority() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(store.pool())
        .await
        .unwrap();

    for bad in [7, -1] {
        let resp = c
            .call(
                methods::HANGAR_ISSUE_CREATE,
                serde_json::json!({
                    "workspace_id": WS_SLUG,
                    "title": "bad urgency",
                    "creator": "member:user-1",
                    "priority": bad,
                }),
            )
            .await;
        assert!(
            !resp["error"].is_null(),
            "priority {bad} must be rejected, got {resp}"
        );
        assert!(
            resp["error"]["message"].as_str().unwrap_or_default().contains("priority"),
            "the rejection must name the offending field: {resp}"
        );
    }

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(after, before, "a rejected create must write no row");
}

/// An OLD client's payload — no `priority` / `due_date` / `labels` keys at all —
/// still creates, landing on the schema defaults. Append-only back-compat.
#[tokio::test]
async fn issue_create_without_the_new_keys_uses_schema_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let row = c
        .call_ok(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "plain create",
                "creator": "member:user-1",
            }),
        )
        .await;
    let id = row["id"].as_str().expect("minted issue id").to_string();

    assert_eq!(row["priority"], 0, "default priority is 0 (P3): {row}");
    assert!(row["due_date"].is_null(), "default due_date is null: {row}");
    // `IssueRow.labels` is `skip_serializing_if = "Vec::is_empty"`, so an
    // un-labelled row omits the key entirely — absent OR `[]` both mean "none".
    assert!(
        row["labels"].as_array().is_none_or(Vec::is_empty),
        "default labels must be empty: {row}"
    );

    let due_date: Option<i64> = sqlx::query_scalar("SELECT due_date FROM issue WHERE id = ?")
        .bind(&id)
        .fetch_one(store.pool())
        .await
        .expect("read stored issue");
    assert_eq!(due_date, None, "stored due_date stays NULL");
    assert!(
        joined_labels(&store, &id).await.is_empty(),
        "no join rows for a label-less create"
    );
}

/// A repeated label name is deduped at the boundary and idempotent in the join:
/// exactly one `issue_label` row, one chip on the row.
#[tokio::test]
async fn issue_create_dedupes_repeated_label_names() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let row = c
        .call_ok(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": "repeat labels",
                "creator": "member:user-1",
                // Blank + duplicate + whitespace-padded: all normalised away.
                "labels": ["bug", " bug ", "", "  "],
            }),
        )
        .await;
    let id = row["id"].as_str().expect("minted issue id").to_string();

    assert_eq!(
        row["labels"],
        serde_json::json!(["bug"]),
        "one chip survives: {row}"
    );
    let joins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue_label WHERE issue_id = ?")
        .bind(&id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(joins, 1, "exactly one join row for a repeated name");
}
