//! Integration: the `hangar/members_list`, `hangar/member_set_role`, and
//! `hangar/member_remove` RPCs over a real framed `UnixStream` (e38.11).
//!
//! The seed fixture lays down one `owner` member (`user-1`) in `WS_ID`; each test
//! adds a second member directly so it can prove set-role, remove, workspace
//! scoping, and the last-owner guard. A connection authenticates, then drives each
//! verb through the dispatcher and asserts:
//! * `members_list` returns the workspace's members with their roles;
//! * `member_set_role` changes a role + is rejected for a foreign workspace;
//! * `member_remove` drops a member;
//! * the last-owner guard rejects demoting / removing the workspace's only owner.

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

/// Insert a second member (`user-2`, an admin) into the seeded workspace so a
/// test can prove set-role / remove without touching the sole owner.
async fn seed_second_member(store: &Store) {
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-2")
        .bind("bob@example.com")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, ?)")
        .bind(WS_ID)
        .bind("user-2")
        .bind("admin")
        .execute(store.pool())
        .await
        .unwrap();
}

/// Find a member row in a `members_list`-shaped result by user id.
fn find_member<'a>(resp: &'a serde_json::Value, user_id: &str) -> &'a serde_json::Value {
    resp["result"]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("members array: {resp}"))
        .iter()
        .find(|m| m["user_id"] == user_id)
        .unwrap_or_else(|| panic!("member {user_id} present: {resp}"))
}

/// `members_list` returns the workspace's members with their roles.
#[tokio::test]
async fn members_list_returns_workspace_members() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_member(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert!(resp["error"].is_null(), "members_list must ack: {resp}");
    let members = resp["result"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 2, "both members listed: {resp}");
    assert_eq!(find_member(&resp, "user-1")["role"], "owner");
    assert_eq!(find_member(&resp, "user-1")["email"], "alice@example.com");
    assert_eq!(find_member(&resp, "user-2")["role"], "admin");
}

/// `member_set_role` changes a role and answers with the refreshed list; the
/// persisted change shows in a follow-up `members_list`.
#[tokio::test]
async fn member_set_role_changes_role_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_member(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_MEMBER_SET_ROLE,
            serde_json::json!({ "workspace_id": WS_SLUG, "user_id": "user-2", "role": "member" }),
        )
        .await;
    assert!(resp["error"].is_null(), "set_role must ack: {resp}");
    assert_eq!(
        find_member(&resp, "user-2")["role"],
        "member",
        "response refreshed"
    );

    // The edit persisted: a fresh snapshot shows the new role.
    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(
        find_member(&list, "user-2")["role"],
        "member",
        "role persisted"
    );
}

/// An illegal role token is rejected with an error, and the member is untouched.
#[tokio::test]
async fn member_set_role_rejects_illegal_role() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_member(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_MEMBER_SET_ROLE,
            serde_json::json!({ "workspace_id": WS_SLUG, "user_id": "user-2", "role": "superuser" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an illegal role must be rejected: {resp}"
    );

    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(
        find_member(&list, "user-2")["role"],
        "admin",
        "role unchanged"
    );
}

/// A set-role aimed at a foreign workspace is rejected — never a silent no-op —
/// and the member is untouched under its real workspace.
#[tokio::test]
async fn member_set_role_is_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_member(&store).await;

    // A second, real tenant workspace that does NOT own user-2's membership.
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

    // Aim user-2 (a member of the seeded workspace) through the "other" workspace.
    let resp = c
        .call(
            methods::HANGAR_MEMBER_SET_ROLE,
            serde_json::json!({ "workspace_id": "other", "user_id": "user-2", "role": "member" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "a cross-tenant member id must not be editable: {resp}"
    );

    // user-2 is untouched in its real workspace.
    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    assert_eq!(
        find_member(&list, "user-2")["role"],
        "admin",
        "cross-tenant edit blocked"
    );
}

/// `member_remove` drops a member; the refreshed list no longer carries it.
#[tokio::test]
async fn member_remove_drops_a_member() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_member(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_MEMBER_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "user_id": "user-2" }),
        )
        .await;
    assert!(resp["error"].is_null(), "remove must ack: {resp}");
    let members = resp["result"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 1, "only the owner remains: {resp}");
    assert_eq!(members[0]["user_id"], "user-1");

    // The removal persisted.
    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(list["result"]["members"].as_array().unwrap().len(), 1);
}

/// The last-owner guard: demoting the SOLE owner is rejected, and the owner keeps
/// its role (a workspace always keeps an owner).
#[tokio::test]
async fn member_set_role_rejects_demoting_the_only_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // user-1 is the seeded SOLE owner; demoting it must be rejected.
    let resp = c
        .call(
            methods::HANGAR_MEMBER_SET_ROLE,
            serde_json::json!({ "workspace_id": WS_SLUG, "user_id": "user-1", "role": "admin" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "demoting the only owner must be rejected: {resp}"
    );

    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(
        find_member(&list, "user-1")["role"],
        "owner",
        "owner role kept"
    );
}

/// The last-owner guard: removing the SOLE owner is rejected, and the owner stays.
#[tokio::test]
async fn member_remove_rejects_removing_the_only_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_MEMBER_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "user_id": "user-1" }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "removing the only owner must be rejected: {resp}"
    );

    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(
        list["result"]["members"].as_array().unwrap().len(),
        1,
        "the owner stays"
    );
}
