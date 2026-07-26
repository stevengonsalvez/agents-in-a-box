//! Integration: the per-agent skill enable/disable RPCs over a real framed
//! `UnixStream` (multica gap #24).
//!
//! Drives the REAL dispatcher against a REAL sqlite file and asserts the
//! user-visible contract:
//!   (a) attach → toggle off → `agent_skills_list` reports `enabled: false`,
//!       and toggling back on restores it,
//!   (b) the toggle PERSISTS — read straight out of the daemon's own db file,
//!       because a wire-level assertion alone proves nothing about durability,
//!   (c) a foreign workspace id is `INVALID_PARAMS`,
//!   (d) toggling an unattached pair answers `{ "toggled": false }`, not an error.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
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


/// Fetch `agent-1`'s skill links as a `(name, enabled)` list.
async fn links(c: &mut Client) -> Vec<(String, bool)> {
    let resp = c
        .call(
            methods::HANGAR_AGENT_SKILLS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG, "agent_id": "agent-1" }),
        )
        .await;
    assert!(resp["error"].is_null(), "agent_skills_list must ack: {resp}");
    resp["result"]["links"]
        .as_array()
        .expect("links array")
        .iter()
        .map(|l| {
            (
                l["name"].as_str().unwrap().to_string(),
                l["enabled"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// Toggle one link and return the raw response frame.
async fn set_enabled(c: &mut Client, skill: &str, enabled: bool) -> serde_json::Value {
    c.call(
        methods::HANGAR_SKILL_SET_ENABLED,
        serde_json::json!({
            "workspace_id": WS_SLUG,
            "agent_id": "agent-1",
            "skill_id": skill,
            "enabled": enabled,
        }),
    )
    .await
}

/// (a) Attach, toggle off, list, toggle on, list — the full round trip.
#[tokio::test]
async fn skill_set_enabled_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // `skill-commit` is pre-attached by the fixture; attach `skill-review` too.
    let resp = c
        .call(
            methods::HANGAR_SKILL_ATTACH,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "agent_id": "agent-1",
                "skill_id": "skill-review",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "attach must ack: {resp}");
    assert_eq!(
        links(&mut c).await,
        vec![("commit".to_string(), true), ("review".to_string(), true)],
        "a fresh attachment starts enabled"
    );

    let resp = set_enabled(&mut c, "skill-review", false).await;
    assert!(resp["error"].is_null(), "toggle must ack: {resp}");
    assert_eq!(resp["result"]["toggled"], true, "an attached pair toggles");
    assert_eq!(
        links(&mut c).await,
        vec![("commit".to_string(), true), ("review".to_string(), false)],
        "the disabled link is still LISTED, just flagged"
    );

    let resp = set_enabled(&mut c, "skill-review", true).await;
    assert!(resp["error"].is_null(), "re-enable must ack: {resp}");
    assert_eq!(
        links(&mut c).await,
        vec![("commit".to_string(), true), ("review".to_string(), true)],
        "re-enabling restores the link"
    );
}

/// (b) Persistence, asserted against the daemon's OWN sqlite file rather than
/// the RPC's rendering of its own write.
#[tokio::test]
async fn skill_set_enabled_persists_to_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = set_enabled(&mut c, "skill-commit", false).await;
    assert!(resp["error"].is_null(), "toggle must ack: {resp}");

    let enabled: i64 = sqlx::query_scalar(
        "SELECT enabled FROM agent_skill WHERE agent_id = 'agent-1' AND skill_id = 'skill-commit'",
    )
    .fetch_one(store.pool())
    .await
    .expect("read enabled straight from the store");
    assert_eq!(enabled, 0, "the toggle is durable, not in-memory");
}

/// (c) A foreign workspace slug is a client error, not a silent no-op.
#[tokio::test]
async fn skill_set_enabled_cross_workspace_is_invalid_params() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_SKILL_SET_ENABLED,
            serde_json::json!({
                "workspace_id": "no-such-workspace",
                "agent_id": "agent-1",
                "skill_id": "skill-commit",
                "enabled": false,
            }),
        )
        .await;
    assert_eq!(
        resp["error"]["code"], -32602,
        "an unknown workspace is INVALID_PARAMS: {resp}"
    );

    let enabled: i64 = sqlx::query_scalar(
        "SELECT enabled FROM agent_skill WHERE agent_id = 'agent-1' AND skill_id = 'skill-commit'",
    )
    .fetch_one(store.pool())
    .await
    .expect("read enabled");
    assert_eq!(enabled, 1, "a rejected call writes nothing");
}

/// (d) An unattached pair is a no-op answer, not an error — the caller learns
/// nothing was toggled from the body.
#[tokio::test]
async fn skill_set_enabled_unattached_answers_toggled_false() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // `skill-review` exists but is NOT attached to `agent-1` in the fixture.
    let resp = set_enabled(&mut c, "skill-review", false).await;
    assert!(resp["error"].is_null(), "an unattached pair is not an error: {resp}");
    assert_eq!(
        resp["result"]["toggled"], false,
        "the body reports that nothing was toggled"
    );
}
