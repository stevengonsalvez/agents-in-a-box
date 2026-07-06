//! Integration: the `hangar/search` RPC returns ranked CROSS-ENTITY matches
//! (issues + agents + skills + autopilots) over a real framed `UnixStream`, and
//! is workspace-scoped (e38.13 — the command palette / Cmd+K backend).
//!
//! The seed fixture lays down the P4 rows in `WS_ID`; this test adds its own
//! controlled-text issue / agent / skill / autopilot all matching one query term,
//! a non-matching row, and a sibling tenant whose matching skill must never leak.
//! It verifies: (a) one hit of EVERY kind is returned, (b) ranking puts an exact
//! match ahead of a prefix match ahead of a substring match across kinds, (c)
//! each entry carries the correct jump-target `screen` token, (d) a non-match is
//! excluded, (e) a sibling tenant's matching entity is never returned, and (f) a
//! blank query matches nothing.

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
}

/// Insert one issue directly with a custom id + title.
async fn insert_issue(pool: &sqlx::SqlitePool, ws: &str, id: &str, title: &str, created_at: i64) {
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
    IssueRepo::insert(
        pool,
        &NewIssue {
            id: id.into(),
            workspace_id: ws.into(),
            title: title.into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: ActorRef::new(ActorKind::Member, "user-1").unwrap(),
            created_at,
            priority: 0,
            due_date: None,
            labels: Vec::new(),
        },
    )
    .await
    .unwrap();
}

/// Insert one agent directly (reuses the seeded `runtime-1`/`user-1` in `WS_ID`;
/// a fresh runtime + user for the sibling tenant).
async fn insert_agent(pool: &sqlx::SqlitePool, ws: &str, id: &str, name: &str, runtime: &str) {
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id, archived) \
         VALUES (?, ?, ?, ?, 'workspace', 'user-1', 0)",
    )
    .bind(id)
    .bind(ws)
    .bind(name)
    .bind(runtime)
    .execute(pool)
    .await
    .unwrap();
}

/// Insert one skill directly.
async fn insert_skill(pool: &sqlx::SqlitePool, ws: &str, id: &str, name: &str) {
    sqlx::query("INSERT INTO skill (id, workspace_id, name) VALUES (?, ?, ?)")
        .bind(id)
        .bind(ws)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
}

/// Insert one autopilot directly (the leader agent must already exist in `ws`).
async fn insert_autopilot(pool: &sqlx::SqlitePool, ws: &str, id: &str, name: &str, agent: &str) {
    sqlx::query(
        "INSERT INTO autopilot (id, workspace_id, agent_id, name, cron_expr, created_at) \
         VALUES (?, ?, ?, ?, '0 0 * * *', 1700000000000)",
    )
    .bind(id)
    .bind(ws)
    .bind(agent)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// Bind + serve the real listener over the seeded store, then add the
/// cross-entity search fixture (one matching row of every kind + a non-match + a
/// sibling tenant).
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    // --- cross-entity fixture in the seeded workspace (WS_ID) ---
    // One matching row of EVERY kind for the query "scout", chosen to exercise the
    // exact > prefix > substring ranking across kinds:
    //   - autopilot "scout"       → EXACT match
    //   - agent     "scout-bot"   → PREFIX match
    //   - skill     "scoutmaster" → PREFIX match
    //   - issue     "...scout..." → SUBSTRING match
    insert_issue(
        pool,
        WS_ID,
        "se-issue",
        "Investigate scout crash",
        1_700_000_010_000,
    )
    .await;
    insert_agent(pool, WS_ID, "se-agent", "scout-bot", "runtime-1").await;
    insert_skill(pool, WS_ID, "se-skill", "scoutmaster").await;
    insert_autopilot(pool, WS_ID, "se-pilot", "scout", "se-agent").await;
    // A non-matching skill — must be excluded.
    insert_skill(pool, WS_ID, "se-none", "totally-unrelated").await;

    // --- a sibling tenant whose matching skill must never leak ---
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("01HANGARFIXTUREWSB00000000")
        .bind("other")
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(pool)
        .await
        .unwrap();
    insert_skill(
        pool,
        "01HANGARFIXTUREWSB00000000",
        "se-foreign",
        "scout-foreign",
    )
    .await;

    rpc::auth::ensure_socket_token(pool, dir).await.expect("ensure socket token");
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
        pool.clone(),
        health,
        ainb_hangar_daemon::events::EventBroker::new(),
    ));
    (socket_path, store)
}

/// The ordered `(kind, id, screen)` triples in a search result.
fn entries_in(resp: &serde_json::Value) -> Vec<(String, String, String)> {
    resp["result"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("search result has no entries array: {resp}"))
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_string(),
                e["id"].as_str().unwrap().to_string(),
                e["screen"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// One matching entity of every kind is returned, ranked exact-first then by kind
/// order; each entry carries the right jump-target screen; the non-match is
/// excluded and a sibling tenant's match never leaks.
#[tokio::test]
async fn search_returns_ranked_cross_entity_results_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_SEARCH,
            serde_json::json!({ "workspace_id": WS_SLUG, "query": "scout" }),
        )
        .await;
    assert!(resp["error"].is_null(), "search must ack: {resp}");

    let entries = entries_in(&resp);
    let ids: Vec<&str> = entries.iter().map(|(_, id, _)| id.as_str()).collect();

    // Exactly the four matching fixture rows — the non-match (se-none), the
    // foreign tenant's skill (se-foreign), and the seeded P4 rows are all absent.
    assert_eq!(
        ids.len(),
        4,
        "exactly one hit of each of the four kinds: {entries:?}"
    );
    assert!(!ids.contains(&"se-none"), "non-match excluded: {entries:?}");
    assert!(
        !ids.contains(&"se-foreign"),
        "sibling tenant's matching skill must never leak: {entries:?}"
    );

    // Match strength dominates kind order:
    //   - autopilot "scout"       → EXACT     (3) → ranks first
    //   - agent     "scout-bot"   → PREFIX    (2) ┐ tie on strength, so kind
    //   - skill     "scoutmaster" → PREFIX    (2) ┘ order breaks it: agent<skill
    //   - issue     "...scout..." → SUBSTRING (1) → ranks last
    // Each entry carries the jump-target screen derived from its kind.
    let triples: Vec<(&str, &str, &str)> =
        entries.iter().map(|(k, i, s)| (k.as_str(), i.as_str(), s.as_str())).collect();
    assert_eq!(
        triples,
        [
            ("autopilot", "se-pilot", "autopilots"),
            ("agent", "se-agent", "issue_list"),
            ("skill", "se-skill", "skill_manager"),
            ("issue", "se-issue", "issue_list"),
        ],
        "exact > prefix(agent<skill) > substring ranking, each with its \
         jump-target screen: {entries:?}"
    );
}

/// A blank query matches nothing — the palette must not dump the whole workspace.
#[tokio::test]
async fn search_blank_query_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_SEARCH,
            serde_json::json!({ "workspace_id": WS_SLUG, "query": "   " }),
        )
        .await;
    assert!(resp["error"].is_null(), "search must ack: {resp}");
    assert!(
        entries_in(&resp).is_empty(),
        "a blank query must match nothing, not dump the workspace"
    );
}

/// An unknown workspace yields an empty result, not an error (a read, like
/// `issues_list`).
#[tokio::test]
async fn search_unknown_workspace_is_empty_not_error() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c
        .call(
            methods::HANGAR_SEARCH,
            serde_json::json!({ "workspace_id": "nope-not-a-workspace", "query": "scout" }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "an unknown workspace is an empty read, not an error: {resp}"
    );
    assert!(
        entries_in(&resp).is_empty(),
        "unknown workspace yields no entries"
    );
}
