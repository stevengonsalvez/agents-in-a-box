//! Integration: TYPED issue links over a real framed `UnixStream` against a real
//! sqlite (multica parity #20).
//!
//! The acceptance in two halves, both proven over the wire:
//!
//! 1. **dispatch gating** — a link authored with an explicit `blocked_by`
//!    `link_type` refuses `hangar/issue_run` while the blocker is unfinished, and
//!    NO task row is written (the assertion counts rows, it does not trust the
//!    error text);
//! 2. **the negative control** — the SAME shape authored as `related` dispatches
//!    normally, so `related` is proven non-gating rather than merely untested.
//!
//! Plus the append-only wire proof: raw JSON params that OMIT `link_type`
//! entirely (a pre-#20 client) still create a GATING edge.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Seed `n` fresh issues named `link-<tag>-<i>` into the fixture workspace.
///
/// Each test owns its OWN issues on purpose: `run_card` serialises launches
/// through a PROCESS-GLOBAL slot keyed on the issue id alone, so two tests that
/// run the same seeded issue in parallel race for that slot and one gets the
/// "already active" refusal instead of the refusal it asserts on.
async fn seed_issues(store: &Store, tag: &str, n: usize) -> Vec<String> {
    use ainb_hangar_core::clock::HangarClock;
    let now = ainb_hangar_core::clock::SystemClock.now_ms();
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = format!("link-{tag}-{i}");
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, description, state, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, ?, 'open', 'member', 'user-1', ?)",
        )
        .bind(&id)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind(format!("Link fixture {i}"))
        // A non-empty brief: `run_card` refuses an empty card BEFORE it reaches
        // the dependency gate, which would mask the refusal under test.
        .bind(format!("Seeded body for link fixture {i}"))
        .bind(now)
        .execute(store.pool())
        .await
        .expect("seed issue");
        ids.push(id);
    }
    ids
}

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

    /// Author a link with an explicit kind token.
    async fn link(&mut self, from: &str, to: &str, kind: &str) -> serde_json::Value {
        self.call(
            methods::HANGAR_ISSUE_LINK_ADD,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": from,
                "other_issue_id": to,
                "link_type": kind,
            }),
        )
        .await
    }

    async fn links_of(&mut self, issue: &str) -> Vec<serde_json::Value> {
        let resp = self
            .call(
                methods::HANGAR_ISSUE_LINKS,
                serde_json::json!({ "workspace_id": WS_SLUG, "issue_id": issue }),
            )
            .await;
        assert!(resp["error"].is_null(), "issue_links must ack: {resp}");
        resp["result"]["links"].as_array().cloned().unwrap_or_default()
    }

    async fn run(&mut self, issue: &str) -> serde_json::Value {
        self.call(
            methods::HANGAR_ISSUE_RUN,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": issue,
                "mode": "headless",
            }),
        )
        .await
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

async fn task_count(store: &Store, issue: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
        .bind(issue)
        .fetch_one(store.pool())
        .await
        .expect("count tasks")
}

/// The at-rest `link_type` of one ordered pair, read with raw SQL — the reply is
/// not evidence of a write.
async fn stored_kind(store: &Store, dependent: &str, blocker: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT link_type FROM card_dependency \
         WHERE dependent_issue_id = ? AND blocker_issue_id = ?",
    )
    .bind(dependent)
    .bind(blocker)
    .fetch_optional(store.pool())
    .await
    .expect("read link_type")
}

/// The acceptance's DISPATCH-GATING half, through the NEW typed write path.
#[tokio::test]
async fn issue_link_add_blocked_by_then_run_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "gate", 2).await;
    let (dep, blocker) = (&ids[0], &ids[1]);

    let resp = c.link(dep, blocker, "blocked_by").await;
    assert!(resp["error"].is_null(), "link add must ack: {resp}");
    assert_eq!(
        stored_kind(&store, dep, blocker).await.as_deref(),
        Some("blocked_by"),
        "the link persisted with its authored kind"
    );

    let run = c.run(dep).await;
    assert!(
        !run["error"].is_null(),
        "a blocked card must refuse to run: {run}"
    );
    let message = run["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("blocked"),
        "the refusal must say the card is blocked, got {message:?}"
    );
    assert_eq!(
        task_count(&store, dep).await,
        0,
        "a refused run must enqueue nothing"
    );
}

/// The negative control: the SAME shape authored as `related` NEVER gates — the
/// run gets past the dependency check instead of being refused as blocked, and
/// the store agrees the card has no unfinished blockers.
#[tokio::test]
async fn issue_link_add_related_then_run_dispatches() {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "related-run", 2).await;
    let (a, b) = (&ids[0], &ids[1]);

    let resp = c.link(a, b, "related").await;
    assert!(resp["error"].is_null(), "related link must ack: {resp}");
    assert_eq!(stored_kind(&store, a, b).await.as_deref(), Some("related"));

    assert!(
        CardDependencyRepo::unfinished_blockers_of(store.pool(), a)
            .await
            .unwrap()
            .is_empty(),
        "a related link leaves the card with no unfinished blockers"
    );

    let run = c.run(a).await;
    let message = run["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("blocked"),
        "a related link must never refuse a run as blocked, got {message:?}"
    );
}

/// The APPEND-ONLY wire proof: raw params that OMIT `link_type` (a pre-#20
/// client) still create a GATING edge.
#[tokio::test]
async fn issue_link_add_without_link_type_still_creates_a_blocking_edge() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "oldclient", 2).await;
    let (dep, blocker) = (&ids[0], &ids[1]);

    let resp = c
        .call(
            methods::HANGAR_ISSUE_LINK_ADD,
            // No `link_type` key at all.
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": dep,
                "other_issue_id": blocker,
            }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "an old client's params must ack: {resp}"
    );
    assert_eq!(
        stored_kind(&store, dep, blocker).await.as_deref(),
        Some("blocked_by"),
        "an omitted link_type means the historical gating edge"
    );

    let run = c.run(dep).await;
    let message = run["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("blocked"),
        "the pre-#20 edge still gates dispatch, got {message:?}"
    );
}

/// All three kinds list back, each carrying the OTHER issue's identity, with
/// `satisfied` set only for a blocker that has finished.
#[tokio::test]
async fn issue_links_lists_all_three_kinds_with_satisfied_flags() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "list", 4).await;
    let (subject, blocker, blocked, friend) = (&ids[0], &ids[1], &ids[2], &ids[3]);

    c.link(subject, blocker, "blocked_by").await;
    c.link(subject, blocked, "blocks").await;
    c.link(subject, friend, "related").await;

    let links = c.links_of(subject).await;
    let kinds: Vec<&str> = links.iter().filter_map(|l| l["kind"].as_str()).collect();
    for want in ["blocked_by", "blocks", "related"] {
        assert!(kinds.contains(&want), "{want} missing from {links:?}");
    }

    let by_kind = |k: &str| {
        links
            .iter()
            .find(|l| l["kind"] == k)
            .unwrap_or_else(|| panic!("a {k} row in {links:?}"))
            .clone()
    };

    let b = by_kind("blocked_by");
    assert_eq!(b["issue_id"], blocker.as_str());
    assert!(
        b["title"].as_str().is_some_and(|t| !t.is_empty()),
        "the link carries the other issue's title: {b}"
    );
    assert!(
        b["display_id"].as_str().is_some_and(|d| !d.is_empty()),
        "the link carries the other issue's display id: {b}"
    );
    assert_eq!(
        b["satisfied"], false,
        "an unfinished blocker is not satisfied"
    );

    assert_eq!(by_kind("blocks")["issue_id"], blocked.as_str());
    assert_eq!(by_kind("related")["issue_id"], friend.as_str());
    assert_eq!(
        by_kind("related")["satisfied"],
        false,
        "a related link is never a satisfied blocker"
    );

    // The reverse read from the OTHER end shows the mirrored kind.
    let other = c.links_of(blocked).await;
    assert!(
        other
            .iter()
            .any(|l| l["kind"] == "blocked_by" && l["issue_id"] == subject.as_str()),
        "the blocked card reads the same relation as blocked_by: {other:?}"
    );
}

/// A `related` link is symmetric: removing it from the OTHER end deletes the one
/// stored row.
#[tokio::test]
async fn issue_link_remove_related_is_symmetric() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "rm", 2).await;
    let (a, b) = (&ids[0], &ids[1]);

    c.link(a, b, "related").await;
    let resp = c
        .call(
            methods::HANGAR_ISSUE_LINK_REMOVE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": b,
                "other_issue_id": a,
                "link_type": "related",
            }),
        )
        .await;
    assert!(resp["error"].is_null(), "remove must ack: {resp}");
    assert_eq!(
        stored_kind(&store, a, b).await,
        None,
        "the mirrored remove deleted the symmetric row"
    );
}

/// A self-link is refused with a kind-agnostic message, for every kind.
#[tokio::test]
async fn a_self_link_is_refused_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "self", 1).await;

    for kind in ["blocked_by", "blocks", "related"] {
        let resp = c.link(&ids[0], &ids[0], kind).await;
        assert!(!resp["error"].is_null(), "{kind} self-link must be refused");
        assert_eq!(resp["error"]["code"], -32602, "refused as INVALID_PARAMS");
        let message = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("itself"),
            "the refusal is kind-agnostic, got {message:?}"
        );
    }
}

/// The acceptance's BOARD-RENDER half: a card row carries the reverse `blocks`
/// direction and the `related` set, while `blocked_by` keeps its exact meaning
/// (UNFINISHED blockers only) — and a `related` link never turns a card blocked.
#[tokio::test]
async fn board_snapshot_card_carries_blocks_and_related() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "board", 3).await;
    let (subject, blocker, friend) = (&ids[0], &ids[1], &ids[2]);

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Links" }),
        )
        .await;
    let board_id = created["result"]["boards"][0]["id"].as_str().expect("board id").to_string();
    let with_col = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }),
        )
        .await;
    let column_id = with_col["result"]["boards"][0]["columns"][0]["id"]
        .as_str()
        .expect("column id")
        .to_string();
    for issue in [subject, blocker, friend] {
        let added = c
            .call(
                methods::HANGAR_BOARD_CARD_ADD,
                serde_json::json!({
                    "workspace_id": WS_SLUG,
                    "board_id": board_id,
                    "issue_id": issue,
                    "column_id": column_id,
                }),
            )
            .await;
        assert!(added["error"].is_null(), "card_add must ack: {added}");
    }

    // subject is blocked_by blocker, blocks nobody yet, and is related to friend.
    c.link(subject, blocker, "blocked_by").await;
    c.link(subject, friend, "related").await;

    let list = c
        .call(
            methods::HANGAR_BOARDS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let cards = list["result"]["boards"][0]["columns"][0]["cards"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let card = |issue: &str| {
        cards
            .iter()
            .find(|c| c["issue_id"] == issue)
            .unwrap_or_else(|| panic!("card for {issue} in {cards:?}"))
            .clone()
    };

    let subject_card = card(subject);
    assert_eq!(
        subject_card["blocked_by"].as_array().map(Vec::len),
        Some(1),
        "the unfinished blocker still renders 🔒: {subject_card}"
    );
    assert_eq!(
        subject_card["related"].as_array().map(Vec::len),
        Some(1),
        "the related card renders on the subject: {subject_card}"
    );

    // The reverse direction renders on the BLOCKER's card, and it is not blocked.
    let blocker_card = card(blocker);
    assert_eq!(
        blocker_card["blocks"].as_array().map(Vec::len),
        Some(1),
        "the blocker card renders what it blocks: {blocker_card}"
    );
    assert!(
        blocker_card["blocked_by"].as_array().is_none_or(Vec::is_empty),
        "the blocker itself is not blocked: {blocker_card}"
    );

    // The RELATED card is neither blocked nor blocking — the whole point.
    let friend_card = card(friend);
    assert!(
        friend_card["blocked_by"].as_array().is_none_or(Vec::is_empty),
        "a related card is never blocked: {friend_card}"
    );
    assert!(
        friend_card["blocks"].as_array().is_none_or(Vec::is_empty),
        "a related card blocks nothing: {friend_card}"
    );
    assert_eq!(
        friend_card["related"].as_array().map(Vec::len),
        Some(1),
        "the relation renders from the other end too: {friend_card}"
    );
}

/// The finalize-seam NEGATIVE: a `related` card is invisible to
/// `dependents_of`, so when the other card finishes it can never be auto-run.
/// The seam itself is UNCHANGED code — this pins that the store filter is what
/// makes it kind-correct.
#[tokio::test]
async fn a_related_card_is_never_a_finalize_seam_dependent() {
    use ainb_hangar_store::repo::card_dependency::{CardDependencyRepo, LinkKind};

    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;
    let ids = seed_issues(&store, "seam", 3).await;
    let (blocker, dependent, friend) = (&ids[0], &ids[1], &ids[2]);

    c.link(dependent, blocker, "blocked_by").await;
    c.link(friend, blocker, "related").await;
    // Opt BOTH into auto-run, so only the link kind can explain the difference.
    for issue in [dependent, friend] {
        CardDependencyRepo::set_auto_run(
            store.pool(),
            &ainb_hangar_core::ids::WorkspaceId::from_str(
                ainb_hangar_daemon::seed::WS_ID.to_string(),
            )
            .unwrap(),
            issue,
            true,
        )
        .await
        .unwrap();
    }

    let dependents = CardDependencyRepo::dependents_of(store.pool(), blocker).await.unwrap();
    assert_eq!(
        dependents,
        vec![dependent.clone()],
        "only the blocked_by card is a finalize-seam dependent"
    );
    assert!(
        !dependents.contains(friend),
        "a related card is never auto-launched when the other finishes"
    );
    assert_eq!(
        CardDependencyRepo::add_link(
            store.pool(),
            &ainb_hangar_core::ids::WorkspaceId::from_str(
                ainb_hangar_daemon::seed::WS_ID.to_string()
            )
            .unwrap(),
            friend,
            blocker,
            LinkKind::Related,
            0,
        )
        .await
        .is_ok(),
        true,
        "re-authoring the same related link stays idempotent"
    );
}
