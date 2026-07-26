//! Integration: the bare programmatic `api` autopilot trigger + the `skipped`
//! run status, over a real framed `UnixStream` (multica parity item 15).
//!
//! This drives the WHOLE path the way `boot()` wires it — the real dispatch
//! table, the real handler, the real store — not the snapshot functions in
//! isolation. It proves both halves of the item end-to-end:
//!
//! 1. an autopilot with an unarmed `api` trigger reports `disabled` and writes
//!    NOTHING (the trigger does not exist, so there is nothing to skip);
//! 2. `hangar/autopilot_set_api_trigger` arms it, and `hangar/autopilots_list`
//!    reports `api_trigger_enabled: true`;
//! 3. `hangar/autopilot_trigger_api` then FIRES, and `hangar/autopilot_runs`
//!    shows a `running` run stamped `source: "api"`;
//! 4. a second fire at `max_concurrent_runs = 1` under the `skip` policy is
//!    DECLINED and RECORDED — `outcome: "skipped"` with a reason, and a
//!    `status: "skipped", source: "api"` row in the run history;
//! 5. a foreign autopilot id reports `not_found` and writes nothing.
//!
//! Mutating the handler to ignore `api_trigger_enabled` breaks (1); mutating the
//! admission gate to drop the skip row breaks (4).

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID};
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

    async fn ok(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let resp = self.call(method, params).await;
        assert!(resp["error"].is_null(), "{method} must ack: {resp}");
        resp["result"].clone()
    }

    async fn trigger_api(&mut self, autopilot_id: &str) -> serde_json::Value {
        self.ok(
            methods::HANGAR_AUTOPILOT_TRIGGER_API,
            serde_json::json!({ "workspace_id": WS_ID, "autopilot_id": autopilot_id }),
        )
        .await
    }

    async fn runs(&mut self, autopilot_id: &str) -> Vec<serde_json::Value> {
        let r = self
            .ok(
                methods::HANGAR_AUTOPILOT_RUNS,
                serde_json::json!({
                    "workspace_id": WS_ID, "autopilot_id": autopilot_id, "limit": 50
                }),
            )
            .await;
        r["runs"].as_array().cloned().unwrap_or_default()
    }
}

/// Bind + serve the real listener over a seeded store (mirrors `boot()`).
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

/// Insert one autopilot bound to the fixture agent, `max_concurrent_runs = 1`
/// and the default `skip` policy, with the api trigger UNARMED.
async fn seed_autopilot(store: &Store) -> String {
    let id = "01HANGARFIXTUREAP000000000".to_string();
    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, cron_expr, max_concurrent_runs, \
          concurrency_policy, next_tick_at, enabled, created_at) \
         VALUES (?, ?, 'agent-1', 'daily', '0 9 * * *', 1, 'skip', NULL, 1, 0)",
    )
    .bind(&id)
    .bind(WS_ID)
    .execute(store.pool())
    .await
    .expect("insert autopilot");
    id
}

/// The whole api-trigger contract in one transcript.
#[tokio::test]
async fn api_trigger_arms_fires_and_records_a_skipped_run() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    // 1. UNARMED: the trigger does not exist. Nothing is fired, nothing written.
    let r = c.trigger_api(&ap).await;
    assert_eq!(
        r["outcome"], "disabled",
        "an unarmed api trigger must decline the call: {r}"
    );
    assert!(
        c.runs(&ap).await.is_empty(),
        "a disabled trigger must write NO run — not even a skipped one"
    );

    // 2. Arm it, and see it on the list snapshot.
    let armed = c
        .ok(
            methods::HANGAR_AUTOPILOT_SET_API_TRIGGER,
            serde_json::json!({ "workspace_id": WS_ID, "autopilot_id": ap, "enabled": true }),
        )
        .await;
    assert_eq!(armed["updated"], true);
    let list = c
        .ok(
            methods::HANGAR_AUTOPILOTS_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    let row = list["autopilots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == ap.as_str())
        .expect("the autopilot is listed")
        .clone();
    assert_eq!(
        row["api_trigger_enabled"], true,
        "the armed trigger is visible on the wire row: {row}"
    );

    // 3. Armed: the dispatch fires, stamped with its api provenance.
    let fired = c.trigger_api(&ap).await;
    assert_eq!(fired["outcome"], "fired", "{fired}");
    assert!(fired["run_id"].is_string() && fired["task_id"].is_string());
    let runs = c.runs(&ap).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["status"], "running");
    assert_eq!(
        runs[0]["source"], "api",
        "the run records WHICH trigger fired it: {:?}",
        runs[0]
    );

    // 4. At the limit under `skip`: DECLINED, and recorded as a skipped run.
    let skipped = c.trigger_api(&ap).await;
    assert_eq!(
        skipped["outcome"], "skipped",
        "a dispatch at the concurrency limit is declined: {skipped}"
    );
    assert!(
        skipped["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("concurrency limit")),
        "the admission reason is reported: {skipped}"
    );
    assert!(skipped["task_id"].is_null(), "a skip enqueues no task");

    let runs = c.runs(&ap).await;
    let skip_row = runs
        .iter()
        .find(|r| r["status"] == "skipped")
        .expect("the declined dispatch is READABLE in the run history, not just logged");
    assert_eq!(skip_row["source"], "api");
    assert!(
        skip_row["failure_reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("concurrency limit")),
        "the reason is persisted: {skip_row}"
    );
    assert!(
        skip_row["completed_at"].is_i64(),
        "a skipped run is TERMINAL, or it would wedge the in-flight count: {skip_row}"
    );
    assert_eq!(
        runs.iter().filter(|r| r["status"] == "running").count(),
        1,
        "the skip created no second live run: {runs:?}"
    );
}

/// A foreign autopilot id reports `not_found` and writes nothing — the id is not
/// probed for existence in another tenant.
#[tokio::test]
async fn api_trigger_on_a_foreign_id_reports_not_found_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    let _ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    let r = c.trigger_api("01HANGARNOSUCHAUTOPILOT000").await;
    assert_eq!(r["outcome"], "not_found", "{r}");

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot_run")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(total, 0, "a foreign id must write no run row");
}
