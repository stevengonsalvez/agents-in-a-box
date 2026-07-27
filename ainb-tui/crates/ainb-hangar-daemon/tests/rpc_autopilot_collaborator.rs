//! Integration: the autopilot COLLABORATOR / SUBSCRIBER actor sets and the
//! RESTRICTED-MODE WRITE GATE over a real framed `UnixStream` (multica parity
//! #27, migration 0064).
//!
//! Drives the WHOLE path the way `boot()` wires it — the real dispatch table,
//! the real handlers, the real store:
//!
//! 1. `hangar/autopilot_collaborator_add` answers with the REFRESHED set, and
//!    an independent `hangar/autopilot_collaborators` read agrees;
//! 2. the load-bearing one: restrict the rule as its creator, watch a STRANGER
//!    be rejected by `hangar/autopilot_update`, grant that stranger `editor`,
//!    and watch the IDENTICAL update succeed and mint a version;
//! 3. an `access_mode = 'open'` rule is unchanged for a stranger — the
//!    no-regression guard for the #14 tests that deliberately edit as a
//!    different human than the creator;
//! 4. `hangar/autopilots_list` carries the two counts.
//!
//! Mutating the gate to always-allow breaks (2). Mutating it to always-deny
//! breaks (3).

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

/// The refreshed collaborator set an add/remove/list answers with.
async fn collaborators(c: &mut Client, ap: &str) -> Vec<serde_json::Value> {
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_COLLABORATORS,
            serde_json::json!({ "workspace_id": WS_ID, "autopilot_id": ap }),
        )
        .await;
    r["collaborators"].as_array().cloned().unwrap_or_default()
}

/// 1 — add answers with the refreshed set, and an independent read agrees.
#[tokio::test]
async fn collaborator_add_then_list_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    assert!(
        collaborators(&mut c, &ap).await.is_empty(),
        "no grants to start"
    );

    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_COLLABORATOR_ADD,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "actor": "member:user-bob",
                "role": "editor",
                "actor_user_id": "user-1",
            }),
        )
        .await;
    let added = r["collaborators"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        added.len(),
        1,
        "the add answers with the refreshed set: {r}"
    );
    assert_eq!(added[0]["actor"], "member:user-bob");
    assert_eq!(added[0]["role"], "editor");
    assert_eq!(
        added[0]["label"], "bob@example.com",
        "the daemon does the user join; the plugin owns zero domain data"
    );

    // An INDEPENDENT read agrees — the add did not just echo its own input.
    assert_eq!(collaborators(&mut c, &ap).await, added);

    // Removing is idempotent and empties the set.
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_COLLABORATOR_REMOVE,
            serde_json::json!({
                "workspace_id": WS_ID, "autopilot_id": ap, "actor": "member:user-bob",
            }),
        )
        .await;
    assert!(
        r["collaborators"].as_array().is_none_or(|a| a.is_empty()),
        "{r}"
    );
    assert!(collaborators(&mut c, &ap).await.is_empty());
}

/// 2 — THE load-bearing test: a restricted rule denies a stranger, then admits
/// them once they hold an `editor` grant.
#[tokio::test]
async fn restricted_rule_denies_a_stranger_then_admits_the_new_collaborator() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    // The creator restricts the rule. That flip is itself a publish.
    let r = c
        .ok(
            methods::HANGAR_AUTOPILOT_SET_ACCESS_MODE,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "access_mode": "restricted",
                "actor_user_id": "user-1",
            }),
        )
        .await;
    assert_eq!(r["outcome"], "updated", "{r}");
    assert_eq!(r["version"], 2, "restricting is a substantive publish: {r}");

    let update = serde_json::json!({
        "workspace_id": WS_ID,
        "autopilot_id": ap,
        "cron_expr": "0 4 * * *",
        "actor_user_id": "user-bob",
    });

    // Bob is a plain workspace member with no grant: REJECTED.
    let denied = c.call(methods::HANGAR_AUTOPILOT_UPDATE, update.clone()).await;
    assert!(
        !denied["error"].is_null(),
        "a stranger must not edit a restricted rule: {denied}"
    );
    let msg = denied["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("access_mode = restricted"), "got {msg}");
    assert_eq!(
        c.versions(&ap).await.len(),
        2,
        "a denied edit mints no version"
    );

    // Grant bob `editor`...
    c.ok(
        methods::HANGAR_AUTOPILOT_COLLABORATOR_ADD,
        serde_json::json!({
            "workspace_id": WS_ID,
            "autopilot_id": ap,
            "actor": "member:user-bob",
            "role": "editor",
            "actor_user_id": "user-1",
        }),
    )
    .await;

    // ...and the IDENTICAL update now succeeds and mints a version.
    let r = c.ok(methods::HANGAR_AUTOPILOT_UPDATE, update).await;
    assert_eq!(r["outcome"], "updated", "{r}");
    assert_eq!(r["version"], 3, "{r}");
    let vs = c.versions(&ap).await;
    assert_eq!(vs[0]["published_by"], "member:user-bob");
}

/// 2b — a `viewer` grant is visibility, not permission.
#[tokio::test]
async fn a_viewer_grant_does_not_admit_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    c.ok(
        methods::HANGAR_AUTOPILOT_COLLABORATOR_ADD,
        serde_json::json!({
            "workspace_id": WS_ID,
            "autopilot_id": ap,
            "actor": "member:user-bob",
            "role": "viewer",
            "actor_user_id": "user-1",
        }),
    )
    .await;
    c.ok(
        methods::HANGAR_AUTOPILOT_SET_ACCESS_MODE,
        serde_json::json!({
            "workspace_id": WS_ID,
            "autopilot_id": ap,
            "access_mode": "restricted",
            "actor_user_id": "user-1",
        }),
    )
    .await;

    let denied = c
        .call(
            methods::HANGAR_AUTOPILOT_UPDATE,
            serde_json::json!({
                "workspace_id": WS_ID,
                "autopilot_id": ap,
                "cron_expr": "0 5 * * *",
                "actor_user_id": "user-bob",
            }),
        )
        .await;
    assert!(
        !denied["error"].is_null(),
        "a viewer may not write: {denied}"
    );
}

/// 3 — the no-regression guard: an OPEN rule (every existing rule, and every
/// pre-0064 one) is untouched for a stranger.
#[tokio::test]
async fn open_rule_is_unchanged_for_a_stranger() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

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
    assert_eq!(
        r["outcome"], "updated",
        "the gate is a no-op on an open rule: {r}"
    );
    assert_eq!(r["version"], 2, "{r}");
}

/// 4 — the list payload carries both counts and the mode.
#[tokio::test]
async fn autopilots_list_carries_the_counts() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    let row = |list: &serde_json::Value, id: &str| -> serde_json::Value {
        list["autopilots"]
            .as_array()
            .expect("autopilots array")
            .iter()
            .find(|r| r["id"] == id)
            .expect("the seeded autopilot")
            .clone()
    };

    let before = c
        .ok(
            methods::HANGAR_AUTOPILOTS_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    let r = row(&before, &ap);
    assert_eq!(r["collaborator_count"], 0);
    assert_eq!(r["subscriber_count"], 0);
    assert_eq!(r["access_mode"], "open", "a fresh rule is open");

    c.ok(
        methods::HANGAR_AUTOPILOT_COLLABORATOR_ADD,
        serde_json::json!({
            "workspace_id": WS_ID, "autopilot_id": ap, "actor": "member:user-bob",
        }),
    )
    .await;
    let subs = c
        .ok(
            methods::HANGAR_AUTOPILOT_SUBSCRIBER_ADD,
            serde_json::json!({
                "workspace_id": WS_ID, "autopilot_id": ap, "actor": "member:user-bob",
            }),
        )
        .await;
    assert_eq!(
        subs["subscribers"].as_array().map(Vec::len),
        Some(1),
        "the subscriber add answers with the refreshed list: {subs}"
    );

    let after = c
        .ok(
            methods::HANGAR_AUTOPILOTS_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    let r = row(&after, &ap);
    assert_eq!(r["collaborator_count"], 1);
    assert_eq!(r["subscriber_count"], 1);
}

/// A foreign / unknown autopilot id is reported LOUDLY, not silently swallowed
/// by the repo's tenant join.
#[tokio::test]
async fn an_unknown_autopilot_is_rejected_not_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    seed_bob(&store).await;
    let _ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_AUTOPILOT_COLLABORATOR_ADD,
            serde_json::json!({
                "workspace_id": WS_ID, "autopilot_id": "ap-nope", "actor": "member:user-bob",
            }),
        )
        .await;
    assert!(!resp["error"].is_null(), "{resp}");
    assert!(
        resp["error"]["message"].as_str().unwrap_or_default().contains("no autopilot"),
        "{resp}"
    );
}

/// An unknown `access_mode` token is rejected rather than tolerantly coerced —
/// a typo must never quietly leave a rule world-writable.
#[tokio::test]
async fn a_bogus_access_mode_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = start_server(dir.path()).await;
    let ap = seed_autopilot(&store).await;
    let mut c = Client::connect(&socket).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_AUTOPILOT_SET_ACCESS_MODE,
            serde_json::json!({
                "workspace_id": WS_ID, "autopilot_id": ap, "access_mode": "wide-open",
            }),
        )
        .await;
    assert!(!resp["error"].is_null(), "{resp}");
}
