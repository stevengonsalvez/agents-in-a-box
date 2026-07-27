//! Integration: the autopilot RULE-VERSION LEDGER and RUN ATTRIBUTION over a
//! real framed `UnixStream` (multica parity #14, migration 0061).
//!
//! Drives the WHOLE path the way `boot()` wires it — the real dispatch table,
//! the real handler, the real store — not the snapshot functions in isolation:
//!
//! 1. `hangar/autopilot_update` with `actor_user_id` reports
//!    `{"outcome":"updated","version":2}`, and `hangar/autopilot_versions`
//!    returns that v2 row with BOTH the raw `published_by` and the daemon-resolved
//!    `published_by_label`;
//! 2. a COSMETIC (rename-only) update reports `version: null` and leaves the
//!    ledger length unchanged — the wire-visible proof of multica's rename rule;
//! 3. params WITHOUT `actor_user_id` still parse (the append-only serde(default)
//!    guarantee) and mint an unattributed version;
//! 4. a foreign workspace reports `not_found` and writes nothing;
//! 5. `hangar/autopilot_fire_now` with `actor_user_id` attributes the run
//!    `direct_human` to THAT human, while the unattended `api` trigger
//!    attributes `rule_owner` — multica's fork, end to end.
//!
//! Mutating `update_as` to publish on a rename breaks (2); mutating `fire_now`
//! to ignore the supplied actor breaks (5).

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

    async fn versions(&mut self, autopilot_id: &str) -> Vec<serde_json::Value> {
        let r = self
            .ok(
                methods::HANGAR_AUTOPILOT_VERSIONS,
                serde_json::json!({
                    "workspace_id": WS_ID, "autopilot_id": autopilot_id, "limit": 50
                }),
            )
            .await;
        r["versions"].as_array().cloned().unwrap_or_default()
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

/// Insert one autopilot bound to the fixture agent through the REAL create path
/// (so it carries rule-version v1), published by `user-1`.
async fn seed_autopilot(store: &Store) -> String {
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::ids::{AgentId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::{
        AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
    };

    let id = AutopilotRepo::create_as(
        store.pool(),
        &SystemClock,
        &NewAutopilot {
            workspace_id: WorkspaceId::from_str(WS_ID).unwrap(),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: "daily".to_string(),
            instructions: Some("v1 instructions".to_string()),
            cron_expr: "0 9 * * *".to_string(),
            max_concurrent_runs: 4,
            execution_mode: ExecutionMode::RunOnly,
            concurrency_policy: ConcurrencyPolicy::Queue,
            api_trigger_enabled: true,
        },
        Some(&ActorRef::new(ActorKind::Member, "user-1").unwrap()),
    )
    .await
    .expect("create autopilot");
    id.to_string()
}

/// Add a SECOND user so the edit can be attributed to somebody other than the
/// rule's creator — the whole point of the ledger.
async fn seed_bob(store: &Store) {
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('user-bob','bob@example.com',0)")
        .execute(store.pool())
        .await
        .expect("insert bob");
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, 'user-bob','member')")
        .bind(WS_ID)
        .execute(store.pool())
        .await
        .expect("member bob");
}

/// C.1-C.4 — the update/versions contract in one transcript.
#[tokio::test]
async fn autopilot_update_publishes_an_attributed_version_and_a_rename_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    // v1 exists from creation, naming alice.
    let v1 = c.versions(&ap).await;
    assert_eq!(v1.len(), 1, "creation minted v1: {v1:?}");
    assert_eq!(v1[0]["version"], 1);
    assert_eq!(v1[0]["change_kind"], "created");
    assert_eq!(v1[0]["published_by"], "member:user-1");

    // C.1 — a SUBSTANTIVE edit by BOB mints v2 naming bob, with a resolved label.
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "instructions": "v2 instructions",
                "actor_user_id": "user-bob",
            }),
        )
        .await;
    assert_eq!(r["outcome"], "updated", "{r}");
    assert_eq!(r["version"], 2, "a substantive edit mints a version: {r}");

    let vs = c.versions(&ap).await;
    assert_eq!(vs.len(), 2);
    let v2 = &vs[0];
    assert_eq!(v2["version"], 2, "versions are newest-first: {v2}");
    assert_eq!(v2["change_kind"], "instructions");
    assert_eq!(
        v2["published_by"], "member:user-bob",
        "the ledger names the EDITOR, not the creator: {v2}"
    );
    assert_eq!(
        v2["published_by_label"], "bob@example.com",
        "the daemon resolves the human label so the plugin owns zero domain data: {v2}"
    );

    // C.2 — a COSMETIC edit lands but mints NO version.
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "name": "daily-renamed",
                "actor_user_id": "user-bob",
            }),
        )
        .await;
    assert_eq!(r["outcome"], "updated", "{r}");
    assert!(
        r["version"].is_null(),
        "a rename must report NO minted version: {r}"
    );
    assert_eq!(
        c.versions(&ap).await.len(),
        2,
        "the ledger must not grow for a rename"
    );
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
        .expect("listed")
        .clone();
    assert_eq!(row["name"], "daily-renamed", "the rename LANDED: {row}");
    assert_eq!(
        row["rule_version"], 2,
        "the list row carries the newest version: {row}"
    );
    assert_eq!(
        row["last_published_by"], "bob@example.com",
        "the list row carries the resolved publisher label: {row}"
    );

    // C.3 — params WITHOUT `actor_user_id` still parse (append-only) and mint an
    // UNATTRIBUTED version rather than a fabricated human.
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "cron_expr": "0 10 * * *",
            }),
        )
        .await;
    assert_eq!(r["version"], 3, "{r}");
    let vs = c.versions(&ap).await;
    assert_eq!(vs[0]["change_kind"], "schedule");
    assert!(
        vs[0]["published_by"].is_null(),
        "an omitted actor is unattributed, never fabricated: {}",
        vs[0]
    );
    assert!(vs[0]["published_by_label"].is_null());

    // C.4 — a DIFFERENT tenant reports not_found and writes nothing: the
    // autopilot id is real, it just does not belong to that workspace.
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-beta','beta','Beta',0)",
    )
    .execute(store.pool())
    .await
    .expect("second workspace");
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_UPDATE,
            serde_json::json!({
                "workspace_id": "beta",
                "autopilot_id": ap,
                "instructions": "hijacked",
                "actor_user_id": "user-bob",
            }),
        )
        .await;
    assert_eq!(r["outcome"], "not_found", "{r}");
    assert_eq!(c.versions(&ap).await.len(), 3, "nothing was written");

    // A malformed cron is a caller error reported as an OUTCOME (nothing
    // written), not an RPC fault.
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "cron_expr": "not a cron at all",
                "actor_user_id": "user-bob",
            }),
        )
        .await;
    assert_eq!(r["outcome"], "invalid_cron", "{r}");
    assert!(r["version"].is_null());
    assert_eq!(
        c.versions(&ap).await.len(),
        3,
        "a rejected cron leaves no orphan version row"
    );
}

/// D — multica's ATTRIBUTION FORK end to end: a named human's "run now" is
/// `direct_human`, an unattended `api` trigger is `rule_owner`.
#[tokio::test]
async fn fire_now_attributes_to_the_clicking_human_while_api_attributes_the_rule_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    // 1. BOB clicks "run now".
    c.ok(
        methods::HANGAR_AUTOPILOT_FIRE_NOW,
        serde_json::json!({
            "workspace_id": WS_ID, "autopilot_id": ap, "actor_user_id": "user-bob",
        }),
    )
    .await;

    // 2. The UNATTENDED api trigger fires.
    let t = c
        .ok(
            methods::HANGAR_AUTOPILOT_TRIGGER_API,
            serde_json::json!({ "workspace_id": WS_ID, "autopilot_id": ap }),
        )
        .await;
    assert_eq!(t["outcome"], "fired", "{t}");

    let runs = c
        .ok(
            methods::HANGAR_AUTOPILOT_RUNS,
            serde_json::json!({ "workspace_id": WS_ID, "autopilot_id": ap, "limit": 50 }),
        )
        .await;
    let runs = runs["runs"].as_array().cloned().unwrap_or_default();
    assert_eq!(runs.len(), 2, "two runs: {runs:?}");

    let manual = runs
        .iter()
        .find(|r| r["source"] == "manual")
        .expect("the manual run is present");
    assert_eq!(
        manual["attribution"], "direct_human",
        "a named human's run now is direct_human: {manual}"
    );
    assert_eq!(
        manual["accountable_actor"], "member:user-bob",
        "BOB is accountable, not the rule's owner (user-1): {manual}"
    );

    let api = runs.iter().find(|r| r["source"] == "api").expect("the api run is present");
    assert_eq!(
        api["attribution"], "rule_owner",
        "an unattended fire resolves the RULE OWNER: {api}"
    );
    assert_eq!(
        api["accountable_actor"], "member:user-1",
        "the api run names the rule's publisher: {api}"
    );
}
