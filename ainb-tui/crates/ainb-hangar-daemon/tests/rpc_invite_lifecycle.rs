//! Integration: the `hangar/invite_create` / `_accept` / `_decline` / `_revoke`
//! RPCs over a real framed `UnixStream` (multica parity #18).
//!
//! Drives the whole lifecycle end-to-end against the seeded fixture (one `owner`,
//! `user-1`) and asserts the contract the settings pane depends on:
//! * an invite adds NO member — it lands in `pending_invites`;
//! * accepting answers with the REFRESHED view (2 members, 0 pending) so the pane
//!   re-renders from the response without a second round-trip;
//! * a spent invitation cannot be accepted again;
//! * `role: "owner"` is rejected;
//! * a mistyped / foreign `workspace_id` is `INVALID_PARAMS`, never a silent no-op;
//! * declining and revoking close an invite without ever adding a member.

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

fn members(resp: &serde_json::Value) -> &Vec<serde_json::Value> {
    resp["result"]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("members array: {resp}"))
}

/// `pending_invites` is `skip_serializing_if = "Vec::is_empty"`, so an absent key
/// means "no live invites".
fn invites(resp: &serde_json::Value) -> Vec<serde_json::Value> {
    resp["result"]["pending_invites"].as_array().cloned().unwrap_or_default()
}

async fn invite_dana(c: &mut Client) -> serde_json::Value {
    c.call(
        methods::HANGAR_INVITE_CREATE,
        serde_json::json!({
            "workspace_id": WS_SLUG,
            "inviter_user_id": "user-1",
            "invitee_email": "dana@example.com",
            "role": "member",
        }),
    )
    .await
}

/// The full happy path: invite → pending (still 1 member) → accept → the ACCEPT
/// response already shows 2 members and 0 pending → a second accept is rejected.
#[tokio::test]
async fn invite_create_then_accept_adds_member_and_refreshes_the_view() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = invite_dana(&mut c).await;
    assert!(
        created["error"].is_null(),
        "invite_create must ack: {created}"
    );
    assert_eq!(
        members(&created).len(),
        1,
        "an invite adds NO member: {created}"
    );
    let pending = invites(&created);
    assert_eq!(pending.len(), 1, "the invite is pending: {created}");
    assert_eq!(pending[0]["invitee_email"], "dana@example.com");
    assert_eq!(pending[0]["role"], "member");
    assert_eq!(pending[0]["status"], "pending");
    let invitation_id = pending[0]["id"].as_str().expect("invitation id").to_string();

    // A plain members_list sees the same pending row.
    let listed = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(
        invites(&listed).len(),
        1,
        "members_list carries it: {listed}"
    );
    assert_eq!(members(&listed).len(), 1);

    // Accepting answers with the refreshed view directly.
    let accepted = c
        .call(
            methods::HANGAR_INVITE_ACCEPT,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "invitation_id": invitation_id,
                "actor_email": "dana@example.com",
            }),
        )
        .await;
    assert!(
        accepted["error"].is_null(),
        "invite_accept must ack: {accepted}"
    );
    assert_eq!(
        members(&accepted).len(),
        2,
        "the accept response already shows the new member: {accepted}"
    );
    assert!(
        members(&accepted).iter().any(|m| m["email"] == "dana@example.com"),
        "dana joined: {accepted}"
    );
    assert!(
        invites(&accepted).is_empty(),
        "the accepted invite left pending_invites: {accepted}"
    );

    // A spent invitation cannot be accepted twice.
    let again = c
        .call(
            methods::HANGAR_INVITE_ACCEPT,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "invitation_id": invitation_id,
                "actor_email": "dana@example.com",
            }),
        )
        .await;
    assert!(
        !again["error"].is_null(),
        "a spent invitation must be rejected: {again}"
    );
    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(members(&list).len(), 2, "still exactly two members: {list}");
}

/// Ownership is transferred, never invited.
#[tokio::test]
async fn invite_create_rejects_owner_role() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_INVITE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "inviter_user_id": "user-1",
                "invitee_email": "dana@example.com",
                "role": "owner",
            }),
        )
        .await;
    assert!(!resp["error"].is_null(), "owner must be refused: {resp}");

    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert!(invites(&list).is_empty(), "nothing was written: {list}");
}

/// A mistyped workspace is `INVALID_PARAMS`, never a silent no-op.
#[tokio::test]
async fn invite_create_rejects_an_unknown_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_INVITE_CREATE,
            serde_json::json!({
                "workspace_id": "no-such-workspace",
                "inviter_user_id": "user-1",
                "invitee_email": "dana@example.com",
                "role": "member",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "an unknown workspace must be rejected: {resp}"
    );

    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_ID }),
        )
        .await;
    assert!(invites(&list).is_empty(), "no invite leaked: {list}");
    assert_eq!(members(&list).len(), 1);
}

/// Someone else cannot accept your invitation.
#[tokio::test]
async fn invite_accept_rejects_a_foreign_actor() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = invite_dana(&mut c).await;
    let invitation_id = invites(&created)[0]["id"].as_str().unwrap().to_string();

    let resp = c
        .call(
            methods::HANGAR_INVITE_ACCEPT,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "invitation_id": invitation_id,
                "actor_email": "eve@example.com",
            }),
        )
        .await;
    assert!(
        !resp["error"].is_null(),
        "a foreign accept is refused: {resp}"
    );

    let list = c
        .call(
            methods::HANGAR_MEMBERS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert_eq!(members(&list).len(), 1, "no member created: {list}");
    assert_eq!(
        invites(&list).len(),
        1,
        "the invite is still pending: {list}"
    );
}

/// Declining closes the invite; revoking deletes one. Neither adds a member.
#[tokio::test]
async fn invite_decline_and_revoke_close_without_adding_a_member() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Decline.
    let created = invite_dana(&mut c).await;
    let first_id = invites(&created)[0]["id"].as_str().unwrap().to_string();
    let declined = c
        .call(
            methods::HANGAR_INVITE_DECLINE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "invitation_id": first_id,
                "actor_email": "dana@example.com",
            }),
        )
        .await;
    assert!(declined["error"].is_null(), "decline must ack: {declined}");
    assert_eq!(members(&declined).len(), 1, "no member added: {declined}");
    assert!(
        invites(&declined).is_empty(),
        "no longer pending: {declined}"
    );

    // A declined invite does not block a re-invite; that one can be revoked.
    let recreated = invite_dana(&mut c).await;
    assert!(
        recreated["error"].is_null(),
        "a declined invite must not block a re-invite: {recreated}"
    );
    let second_id = invites(&recreated)[0]["id"].as_str().unwrap().to_string();

    let revoked = c
        .call(
            methods::HANGAR_INVITE_REVOKE,
            serde_json::json!({ "workspace_id": WS_SLUG, "invitation_id": second_id }),
        )
        .await;
    assert!(revoked["error"].is_null(), "revoke must ack: {revoked}");
    assert!(
        invites(&revoked).is_empty(),
        "the invite is gone: {revoked}"
    );
    assert_eq!(
        members(&revoked).len(),
        1,
        "still only the owner: {revoked}"
    );

    // A second revoke has nothing left to withdraw.
    let again = c
        .call(
            methods::HANGAR_INVITE_REVOKE,
            serde_json::json!({ "workspace_id": WS_SLUG, "invitation_id": second_id }),
        )
        .await;
    assert!(!again["error"].is_null(), "nothing left to revoke: {again}");
}
