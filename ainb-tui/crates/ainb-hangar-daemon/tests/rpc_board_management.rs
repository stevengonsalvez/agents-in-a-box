//! Integration: the `hangar/boards_list` + `hangar/board_*` (board / column /
//! card) RPCs over a real framed `UnixStream` (P4 / D8).
//!
//! A connection authenticates, then drives each verb through the dispatcher and
//! asserts:
//! * `board_create` + `board_column_add` + `board_card_add` build a board with
//!   ordered columns and a placed card, and `boards_list` reads it back — with
//!   the card enriched by its issue title + latest task status;
//! * a duplicate board name is rejected (resolve-or-reject guard);
//! * board mutations are workspace-scoped (a sibling tenant cannot see / mutate);
//! * `board_column_reorder` rewrites the order without moving the card, and
//!   `board_column_delete` parks the card UNMAPPED (no data loss).

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
            id: RpcId::Number(21),
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

/// The id of the (single) board in a `boards_list`-shaped result.
fn only_board_id(resp: &serde_json::Value) -> String {
    let boards = resp["result"]["boards"].as_array().unwrap_or_else(|| panic!("boards array: {resp}"));
    assert_eq!(boards.len(), 1, "exactly one board: {resp}");
    boards[0]["id"].as_str().unwrap().to_string()
}

/// The (single) board in a `boards_list`-shaped result.
fn only_board(resp: &serde_json::Value) -> &serde_json::Value {
    let boards = resp["result"]["boards"].as_array().unwrap_or_else(|| panic!("boards array: {resp}"));
    assert_eq!(boards.len(), 1, "exactly one board: {resp}");
    &boards[0]
}

/// `board_create` + `board_column_add` + `board_card_add` build a board; a
/// follow-up `boards_list` reads it back with columns ordered and the card
/// enriched by its issue title.
#[tokio::test]
async fn board_create_columns_cards_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Sprint" }),
        )
        .await;
    assert!(created["error"].is_null(), "board_create must ack: {created}");
    let board_id = only_board_id(&created);

    // Two columns: a manual "Todo" and an auto-move "Done" mapped to `done`.
    let with_todo = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }),
        )
        .await;
    assert!(with_todo["error"].is_null(), "column_add must ack: {with_todo}");
    let with_done = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Done", "fsm_state": "done", "auto_move": true }),
        )
        .await;
    let board = only_board(&with_done);
    let cols = board["columns"].as_array().unwrap();
    assert_eq!(cols.len(), 2, "two columns: {with_done}");
    assert_eq!(cols[0]["name"], "Todo");
    assert_eq!(cols[1]["name"], "Done");
    assert_eq!(cols[1]["fsm_state"], "done");
    assert_eq!(cols[1]["auto_move"], true);
    let todo_id = cols[0]["id"].as_str().unwrap().to_string();

    // Place issue-2 in Todo.
    let with_card = c
        .call(
            methods::HANGAR_BOARD_CARD_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": "issue-2", "column_id": todo_id }),
        )
        .await;
    assert!(with_card["error"].is_null(), "card_add must ack: {with_card}");

    // boards_list reads it back, with the card enriched by its issue title.
    let list = c
        .call(
            methods::HANGAR_BOARDS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let board = only_board(&list);
    let todo = &board["columns"].as_array().unwrap()[0];
    let cards = todo["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1, "one card in Todo: {list}");
    assert_eq!(cards[0]["issue_id"], "issue-2");
    assert_eq!(
        cards[0]["title"], "Fix flaky test",
        "the card carries the issue title, not the raw id: {list}"
    );

    // A duplicate board name is rejected (resolve-or-reject).
    let dup = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Sprint" }),
        )
        .await;
    assert!(!dup["error"].is_null(), "a duplicate board name must be rejected: {dup}");
}

/// `board_column_reorder` rewrites the order without moving a card, and
/// `board_column_delete` parks the card unmapped (no data loss).
#[tokio::test]
async fn column_reorder_and_delete_preserve_cards() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Flow" }),
        )
        .await;
    let board_id = only_board_id(&created);
    let mut col_ids = Vec::new();
    for name in ["A", "B", "C"] {
        let r = c
            .call(
                methods::HANGAR_BOARD_COLUMN_ADD,
                serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": name }),
            )
            .await;
        let cols = only_board(&r)["columns"].as_array().unwrap();
        col_ids.push(cols.last().unwrap()["id"].as_str().unwrap().to_string());
    }
    // Place issue-2 in column B (index 1).
    c.call(
        methods::HANGAR_BOARD_CARD_ADD,
        serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": "issue-2", "column_id": col_ids[1] }),
    )
    .await;

    // Reorder to C, A, B.
    let reordered = c
        .call(
            methods::HANGAR_BOARD_COLUMN_REORDER,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_ids": [col_ids[2], col_ids[0], col_ids[1]] }),
        )
        .await;
    assert!(reordered["error"].is_null(), "reorder must ack: {reordered}");
    let names: Vec<String> = only_board(&reordered)["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["C", "A", "B"], "columns follow the new order");
    // The card still sits in B — reorder never moved it.
    let b_col = only_board(&reordered)["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|col| col["id"] == serde_json::json!(col_ids[1]))
        .unwrap();
    assert_eq!(b_col["cards"].as_array().unwrap().len(), 1, "card stayed in B");

    // Delete column B — its card parks unmapped, not lost.
    let deleted = c
        .call(
            methods::HANGAR_BOARD_COLUMN_DELETE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_id": col_ids[1] }),
        )
        .await;
    assert!(deleted["error"].is_null(), "column_delete must ack: {deleted}");
    let board = only_board(&deleted);
    assert_eq!(board["columns"].as_array().unwrap().len(), 2, "two columns remain");
    let unmapped = board["unmapped"].as_array().unwrap();
    assert_eq!(unmapped.len(), 1, "the card parked unmapped, not lost: {deleted}");
    assert_eq!(unmapped[0]["issue_id"], "issue-2");
}

/// `board_card_create` mints an issue titled by the card + assigned to the agent
/// named for the profile (D16), places it, and `board_card_run` enqueues a task
/// routed to that agent's runtime (ccc / D6). A run with no resolvable assignee
/// still falls back to the workspace's agent, and a bad mode is rejected.
#[tokio::test]
async fn card_create_assigns_profile_then_run_enqueues() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Delivery" }),
        )
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }),
        )
        .await;
    let todo_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Create a card assigning the profile `claude-agent` — the seeded agent's name
    // (D16: the assignee slug is the profile slug = the agent name).
    let with_card = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "board_id": board_id,
                "column_id": todo_id,
                "title": "Ship the boards",
                "assignee_profile": "claude-agent",
                // F2/F4: a card carries a repo (required to launch) + a chosen agent.
                "repo_ref": "scratch",
                "agent": "codex",
            }),
        )
        .await;
    assert!(with_card["error"].is_null(), "card_create must ack: {with_card}");
    let card = &only_board(&with_card)["columns"].as_array().unwrap()[0]["cards"]
        .as_array()
        .unwrap()[0];
    assert_eq!(card["title"], "Ship the boards", "card carries the typed title");
    let issue_id = card["issue_id"].as_str().unwrap().to_string();

    // Run the card headless → routed to the assignee agent's runtime.
    let run = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }),
        )
        .await;
    assert!(run["error"].is_null(), "card_run must ack: {run}");
    assert_eq!(run["result"]["agent_id"], "agent-1", "routed to the assignee agent: {run}");
    assert_eq!(run["result"]["runtime_id"], "runtime-1", "keyed to the agent's runtime: {run}");
    assert_eq!(run["result"]["mode"], "headless", "the launch mode is echoed: {run}");
    let task_id = run["result"]["task_id"].as_str().unwrap().to_string();

    // A queued task row exists for the card's issue, routed to agent-1/runtime-1.
    let row: (String, String, Option<String>, String) = sqlx::query_as(
        "SELECT agent_id, runtime_id, issue_id, status FROM agent_task_queue WHERE id = ?",
    )
    .bind(&task_id)
    .fetch_one(store.pool())
    .await
    .expect("the run enqueued a task row");
    assert_eq!(row.0, "agent-1");
    assert_eq!(row.1, "runtime-1");
    assert_eq!(row.2.as_deref(), Some(issue_id.as_str()));
    assert_eq!(row.3, "queued", "the run task starts queued for the claim loop");

    // F2/F4: the card's repo + chosen agent flowed onto the dispatched task.
    let parity: (Option<String>, String) =
        sqlx::query_as("SELECT repo_ref, agent_kind FROM agent_task_queue WHERE id = ?")
            .bind(&task_id)
            .fetch_one(store.pool())
            .await
            .expect("the run task carries the card's repo + agent");
    assert_eq!(parity.0.as_deref(), Some("scratch"), "the card's repo flowed onto the task");
    assert_eq!(parity.1, "codex", "the card's chosen agent flowed onto the task");

    // A card whose profile never resolves still runs — the workspace agent is the
    // fallback so a card is never a dead end.
    let orphan = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "board_id": board_id,
                "column_id": todo_id,
                "title": "No such profile",
                "assignee_profile": "ghost-profile",
                "repo_ref": "scratch",
            }),
        )
        .await;
    let orphan_issue = {
        let cards = only_board(&orphan)["columns"].as_array().unwrap()[0]["cards"]
            .as_array()
            .unwrap();
        cards
            .iter()
            .find(|c| c["title"] == "No such profile")
            .unwrap()["issue_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let orphan_run = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": orphan_issue, "mode": "interactive" }),
        )
        .await;
    assert!(orphan_run["error"].is_null(), "an unassigned card still runs: {orphan_run}");
    assert_eq!(orphan_run["result"]["agent_id"], "agent-1", "fell back to the workspace agent");
    assert_eq!(orphan_run["result"]["mode"], "interactive", "interactive mode echoed");

    // A bad mode is a client error, never a silent default.
    let bad = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "yolo" }),
        )
        .await;
    assert!(!bad["error"].is_null(), "an unknown run mode must be rejected: {bad}");

    // MEMBERSHIP GUARD: running a workspace issue that is NOT a card on the board
    // is rejected (the run is a card affordance, not a bare issue dispatch).
    let non_card = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": "issue-1", "mode": "headless" }),
        )
        .await;
    assert!(
        !non_card["error"].is_null(),
        "an issue that is not a card on the board must not be runnable: {non_card}"
    );

    // ATOMIC CREATE: a card-create targeting a bad column rejects up front and
    // leaves NO orphan issue (nothing persists unless the card can be placed).
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(store.pool())
        .await
        .unwrap();
    let orphan_create = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "board_id": board_id,
                "column_id": "no-such-column",
                "title": "Orphan",
                "assignee_profile": null,
            }),
        )
        .await;
    assert!(
        !orphan_create["error"].is_null(),
        "a bad column must reject the create: {orphan_create}"
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(before, after, "a rejected card-create must not strand an orphan issue");
}

/// A shared helper: create a board + one Todo column, returning `(board_id,
/// column_id)`.
async fn board_with_todo(c: &mut Client, name: &str) -> (String, String) {
    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": name }),
        )
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }),
        )
        .await;
    let col_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    (board_id, col_id)
}

/// Create a card and return its issue id.
async fn create_card(c: &mut Client, board_id: &str, col_id: &str, body: serde_json::Value) -> String {
    let mut params = serde_json::json!({
        "workspace_id": WS_SLUG,
        "board_id": board_id,
        "column_id": col_id,
    });
    let map = params.as_object_mut().unwrap();
    for (k, v) in body.as_object().unwrap() {
        map.insert(k.clone(), v.clone());
    }
    let resp = c.call(methods::HANGAR_BOARD_CARD_CREATE, params).await;
    assert!(resp["error"].is_null(), "card_create must ack: {resp}");
    // The just-created card is the last one in the column.
    let cards = only_board(&resp)["columns"].as_array().unwrap()[0]["cards"]
        .as_array()
        .unwrap()
        .clone();
    let want = body["title"].as_str().unwrap();
    cards.iter().find(|c| c["title"] == want).unwrap()["issue_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// F2 (repo REQUIRED): a card created without a repo cannot be launched — the
/// run is refused (pointing at the scratch repo) rather than dispatching a
/// "random" task. Adding the repo afterwards makes it runnable.
#[tokio::test]
async fn card_run_requires_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let (board_id, col_id) = board_with_todo(&mut c, "Repo Gate").await;
    // A card with NO repo.
    let issue_id = create_card(
        &mut c,
        &board_id,
        &col_id,
        serde_json::json!({ "title": "No repo yet" }),
    )
    .await;

    let refused = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }),
        )
        .await;
    assert!(
        !refused["error"].is_null(),
        "a card with no repo must refuse to launch (F2): {refused}"
    );
    assert!(
        refused["error"]["message"].as_str().unwrap().contains("repo"),
        "the refusal must name the missing repo: {refused}"
    );

    // A run-time repo override makes the same card launchable.
    let ok = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless", "repo_ref": "scratch" }),
        )
        .await;
    assert!(ok["error"].is_null(), "a repo override makes the card runnable (F2): {ok}");
}

/// F8: copilot is picker-visible but its runner is not wired — a dispatch on it
/// is refused with a clear error rather than stranding the task.
#[tokio::test]
async fn card_run_rejects_copilot_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let (board_id, col_id) = board_with_todo(&mut c, "Copilot Gate").await;
    let issue_id = create_card(
        &mut c,
        &board_id,
        &col_id,
        serde_json::json!({ "title": "Copilot please", "repo_ref": "scratch", "agent": "copilot" }),
    )
    .await;

    let refused = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }),
        )
        .await;
    assert!(!refused["error"].is_null(), "copilot dispatch must be refused (F8): {refused}");
    let msg = refused["error"]["message"].as_str().unwrap();
    assert!(msg.contains("copilot") && msg.contains("F8"), "the refusal names copilot + F8: {refused}");
}

/// F4 cascade: a card with a repo but NO chosen agent resolves the run's agent
/// from the workspace default (when set) rather than the hard `claude` default.
#[tokio::test]
async fn card_run_resolves_agent_via_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Set the workspace-level default agent to codex (no last-used / board / global).
    sqlx::query("UPDATE workspace SET default_agent = 'codex' WHERE slug = ?")
        .bind(WS_SLUG)
        .execute(store.pool())
        .await
        .unwrap();

    let (board_id, col_id) = board_with_todo(&mut c, "Cascade").await;
    let issue_id = create_card(
        &mut c,
        &board_id,
        &col_id,
        serde_json::json!({ "title": "Cascade me", "repo_ref": "scratch" }),
    )
    .await;

    let run = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }),
        )
        .await;
    assert!(run["error"].is_null(), "card_run must ack: {run}");
    let task_id = run["result"]["task_id"].as_str().unwrap().to_string();
    let agent_kind: String = sqlx::query_scalar("SELECT agent_kind FROM agent_task_queue WHERE id = ?")
        .bind(&task_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(agent_kind, "codex", "cascade resolved the workspace default (F4)");

    // The run also recorded codex as the last-used agent (top of the cascade).
    let last_used: Option<String> =
        sqlx::query_scalar("SELECT value FROM daemon_config WHERE key = 'card_agent.last_used'")
            .fetch_optional(store.pool())
            .await
            .unwrap()
            .flatten();
    assert_eq!(last_used.as_deref(), Some("codex"), "last-used agent recorded (F4)");
}

/// F3: `hangar/repo_list` answers with a well-formed roster array (no error).
/// Content depends on the host's favorites/cache, so this asserts the wire
/// shape + method registration, not specific repos.
#[tokio::test]
async fn repo_list_answers_with_a_roster() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c.call(methods::HANGAR_REPO_LIST, serde_json::json!({})).await;
    assert!(resp["error"].is_null(), "repo_list must ack: {resp}");
    assert!(resp["result"]["repos"].is_array(), "repo_list returns a repos array: {resp}");
}

/// Board mutations are workspace-scoped: a sibling tenant cannot see the board,
/// and cannot mutate it (a foreign-workspace column-add is a not-found error).
#[tokio::test]
async fn boards_are_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    // A second, real tenant workspace.
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

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Sprint" }),
        )
        .await;
    let board_id = only_board_id(&created);

    // The "other" tenant sees no boards.
    let other_list = c
        .call(
            methods::HANGAR_BOARDS_LIST,
            serde_json::json!({ "workspace_id": "other" }),
        )
        .await;
    assert!(
        other_list["result"]["boards"].as_array().unwrap().is_empty(),
        "a sibling tenant must not see the board: {other_list}"
    );

    // The "other" tenant cannot add a column to the seeded board (not-found).
    let cross = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": "other", "board_id": board_id, "name": "X" }),
        )
        .await;
    assert!(
        !cross["error"].is_null(),
        "a cross-tenant board must not be mutable: {cross}"
    );
}

/// tcp T3 / F6: `board_card_reorder` rewrites the order of ONE column's cards, and
/// the new order survives a fresh `boards_list` (persisted to `board_card.ord`,
/// not merely echoed). A reorder set that is not exactly the column's cards is
/// rejected.
#[tokio::test]
async fn card_reorder_persists_within_column_and_rejects_bad_set() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, _store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Order" }),
        )
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }),
        )
        .await;
    let todo_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Place issue-1 then issue-2 → they APPEND in that order (ord 0, 1).
    for issue in ["issue-1", "issue-2"] {
        c.call(
            methods::HANGAR_BOARD_CARD_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue, "column_id": todo_id }),
        )
        .await;
    }
    let order = |resp: &serde_json::Value| -> Vec<String> {
        only_board(resp)["columns"].as_array().unwrap()[0]["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["issue_id"].as_str().unwrap().to_string())
            .collect()
    };
    let list = c.call(methods::HANGAR_BOARDS_LIST, serde_json::json!({ "workspace_id": WS_SLUG })).await;
    assert_eq!(order(&list), vec!["issue-1", "issue-2"], "cards append in insertion order");

    // Reorder to [issue-2, issue-1]; the reply reflects it.
    let reordered = c
        .call(
            methods::HANGAR_BOARD_CARD_REORDER,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_id": todo_id, "issue_ids": ["issue-2", "issue-1"] }),
        )
        .await;
    assert!(reordered["error"].is_null(), "reorder must ack: {reordered}");
    assert_eq!(order(&reordered), vec!["issue-2", "issue-1"], "reply reflects the new order");

    // A fresh boards_list proves the order PERSISTED (ord written to disk), not
    // just echoed by the mutation reply.
    let refetched = c.call(methods::HANGAR_BOARDS_LIST, serde_json::json!({ "workspace_id": WS_SLUG })).await;
    assert_eq!(order(&refetched), vec!["issue-2", "issue-1"], "the reorder persisted across a re-fetch");

    // A set that is not exactly the column's cards is rejected, nothing written.
    let bad = c
        .call(
            methods::HANGAR_BOARD_CARD_REORDER,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_id": todo_id, "issue_ids": ["issue-2"] }),
        )
        .await;
    assert!(!bad["error"].is_null(), "an incomplete reorder set must be rejected: {bad}");
    let after = c.call(methods::HANGAR_BOARDS_LIST, serde_json::json!({ "workspace_id": WS_SLUG })).await;
    assert_eq!(order(&after), vec!["issue-2", "issue-1"], "a rejected reorder leaves the order intact");
}

/// tcp T3 / F6: `board_card_remove` takes a card OFF a board but keeps the
/// underlying issue (re-addable), REFUSES a card with an active run
/// (delete-while-running = cancel-first), and is an idempotent no-op for a
/// non-card issue.
#[tokio::test]
async fn card_remove_keeps_issue_and_refuses_active_run() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_BOARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "Cleanup" }),
        )
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(
            methods::HANGAR_BOARD_COLUMN_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }),
        )
        .await;
    let todo_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A card that will get a live run: assign the seeded agent + a repo so a
    // headless run enqueues.
    let with_card = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "board_id": board_id,
                "column_id": todo_id,
                "title": "Throwaway",
                "assignee_profile": "claude-agent",
                "repo_ref": "scratch",
            }),
        )
        .await;
    let issue_id = only_board(&with_card)["columns"].as_array().unwrap()[0]["cards"]
        .as_array()
        .unwrap()[0]["issue_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Enqueue a run → the card now has an ACTIVE task.
    let run = c
        .call(
            methods::HANGAR_BOARD_CARD_RUN,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }),
        )
        .await;
    assert!(run["error"].is_null(), "run must ack: {run}");

    // A remove while the run is active is REFUSED (cancel-first).
    let refused = c
        .call(
            methods::HANGAR_BOARD_CARD_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }),
        )
        .await;
    assert!(!refused["error"].is_null(), "removing a card with an active run must be refused: {refused}");
    assert!(
        refused["error"]["message"].as_str().unwrap_or_default().contains("active run"),
        "the refusal names the active run: {refused}"
    );

    // Cancel the run, then the remove is allowed.
    let cancelled = c
        .call(
            methods::HANGAR_BOARD_CARD_CANCEL,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }),
        )
        .await;
    assert!(cancelled["error"].is_null(), "cancel must ack: {cancelled}");
    let removed = c
        .call(
            methods::HANGAR_BOARD_CARD_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }),
        )
        .await;
    assert!(removed["error"].is_null(), "remove after cancel must ack: {removed}");
    let cards = only_board(&removed)["columns"].as_array().unwrap()[0]["cards"].as_array().unwrap();
    assert!(cards.is_empty(), "the card is off the board: {removed}");

    // The underlying issue SURVIVES the card removal (removing a card is not
    // deleting the issue).
    let issue_alive: Option<i64> = sqlx::query_scalar("SELECT 1 FROM issue WHERE id = ?")
        .bind(&issue_id)
        .fetch_optional(store.pool())
        .await
        .expect("issue query");
    assert_eq!(issue_alive, Some(1), "the issue is kept after the card is removed");

    // Removing a NON-card issue (issue-1 was never added) is an idempotent no-op.
    let noop = c
        .call(
            methods::HANGAR_BOARD_CARD_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": "issue-1" }),
        )
        .await;
    assert!(noop["error"].is_null(), "removing a non-card is a clean no-op: {noop}");
}

/// tcp T3 / F6 (P10 §4.9): `board_card_timeline` returns the RAW provider
/// stream-json a card's newest run teed to disk, read from the deterministic
/// per-task logs dir. The plugin parses this text into the prettied timeline; the
/// e2e proves the daemon serves the right file (the parser's rendering is
/// unit-tested in the plugin crate).
#[tokio::test]
async fn card_timeline_serves_the_run_transcript_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    // The timeline RPC derives the per-task logs dir under $AINB_HANGAR_HOME; point
    // it at this test's tempdir so the seeded transcript resolves. Only this test
    // in the binary reads hangar_home, so the process-env set does not race.
    std::env::set_var("AINB_HANGAR_HOME", dir.path());

    let (socket_path, _store) = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(methods::HANGAR_BOARD_CREATE, serde_json::json!({ "workspace_id": WS_SLUG, "name": "Obs" }))
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(methods::HANGAR_BOARD_COLUMN_ADD, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }))
        .await;
    let todo_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"].as_str().unwrap().to_string();
    let with_card = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_id": todo_id, "title": "Watch me", "assignee_profile": "claude-agent", "repo_ref": "scratch" }),
        )
        .await;
    let issue_id = only_board(&with_card)["columns"].as_array().unwrap()[0]["cards"].as_array().unwrap()[0]["issue_id"].as_str().unwrap().to_string();

    // A card with NO run yet → an empty transcript (a read, never an error).
    let empty = c
        .call(methods::HANGAR_BOARD_CARD_TIMELINE, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }))
        .await;
    assert!(empty["error"].is_null(), "timeline of an unrun card is not an error: {empty}");
    assert_eq!(empty["result"]["jsonl"], "", "no run → empty transcript");

    // Enqueue a run, then seed a fixture transcript at that task's deterministic
    // logs path (`workspaces/default/{short_id(task_id)}/logs/claude.jsonl` — the
    // daemon's own slug derivation, so this test can never drift from it).
    let run = c
        .call(methods::HANGAR_BOARD_CARD_RUN, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }))
        .await;
    let task_id = run["result"]["task_id"].as_str().unwrap().to_string();
    let logs = dir
        .path()
        .join(".agents-in-a-box/hangar/workspaces/default")
        .join(ainb_hangar_daemon::execenv::short_id(&task_id))
        .join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let fixture = concat!(
        "{\"type\":\"assistant\",\"timestamp\":1000,\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{\"command\":\"cargo test --workspace\"}}]}}\n",
        "{\"type\":\"user\",\"timestamp\":1800,\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}]}}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"done\"}\n",
    );
    std::fs::write(logs.join("claude.jsonl"), fixture).unwrap();

    // The timeline RPC serves the transcript for the card's newest task.
    let tl = c
        .call(methods::HANGAR_BOARD_CARD_TIMELINE, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }))
        .await;
    assert!(tl["error"].is_null(), "timeline must ack: {tl}");
    assert_eq!(tl["result"]["task_id"], task_id, "the newest task's transcript");
    assert_eq!(tl["result"]["provider"], "claude", "read the claude log");
    let jsonl = tl["result"]["jsonl"].as_str().unwrap();
    assert!(
        jsonl.contains("cargo test --workspace") && jsonl.contains("tool_use"),
        "the run's tool call is in the served transcript: {jsonl}"
    );

    std::env::remove_var("AINB_HANGAR_HOME");
}

/// tcp T3 / F6 race guard: a card whose run is RUNNING refuses a second run — the
/// active-task guard closes the shadow-task hole the pending-only unique index
/// misses. Deterministic: the guard is a DB-conditional check, so a `running`
/// task is enough to prove the rejection without real concurrency.
#[tokio::test]
async fn rerun_while_running_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(methods::HANGAR_BOARD_CREATE, serde_json::json!({ "workspace_id": WS_SLUG, "name": "Race" }))
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(methods::HANGAR_BOARD_COLUMN_ADD, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }))
        .await;
    let todo_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"].as_str().unwrap().to_string();
    let with_card = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_id": todo_id, "title": "Run once", "assignee_profile": "claude-agent", "repo_ref": "scratch" }),
        )
        .await;
    let issue_id = only_board(&with_card)["columns"].as_array().unwrap()[0]["cards"].as_array().unwrap()[0]["issue_id"].as_str().unwrap().to_string();

    // First run enqueues a task; drive it to `running` so the guard covers the
    // running (not merely pending) case.
    let first = c
        .call(methods::HANGAR_BOARD_CARD_RUN, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }))
        .await;
    let task_id = first["result"]["task_id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE agent_task_queue SET status = 'running' WHERE id = ?")
        .bind(&task_id)
        .execute(store.pool())
        .await
        .unwrap();

    // A second run while the first is RUNNING is refused (no shadow task).
    let second = c
        .call(methods::HANGAR_BOARD_CARD_RUN, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }))
        .await;
    assert!(!second["error"].is_null(), "a rerun while running must be rejected: {second}");
    assert!(
        second["error"]["message"].as_str().unwrap_or_default().contains("already active"),
        "the rejection names the active run: {second}"
    );

    // Exactly ONE task exists for the card — no shadow row was enqueued.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
        .bind(&issue_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(n, 1, "the rejected rerun enqueued no shadow task");
}

/// tcp T3 / F6: a card cancelled BEFORE its run is ever claimed flips the queued
/// task straight to `cancelled` (the DB flip alone cancels a run that never
/// started — the run loop's kill signal simply finds no live process). Cancelling
/// again is an idempotent success, never a double-event.
#[tokio::test]
async fn cancel_before_start_cancels_a_queued_run() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(methods::HANGAR_BOARD_CREATE, serde_json::json!({ "workspace_id": WS_SLUG, "name": "Preempt" }))
        .await;
    let board_id = only_board_id(&created);
    let with_col = c
        .call(methods::HANGAR_BOARD_COLUMN_ADD, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "name": "Todo" }))
        .await;
    let todo_id = only_board(&with_col)["columns"].as_array().unwrap()[0]["id"].as_str().unwrap().to_string();
    let with_card = c
        .call(
            methods::HANGAR_BOARD_CARD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "column_id": todo_id, "title": "Never starts", "assignee_profile": "claude-agent", "repo_ref": "scratch" }),
        )
        .await;
    let issue_id = only_board(&with_card)["columns"].as_array().unwrap()[0]["cards"].as_array().unwrap()[0]["issue_id"].as_str().unwrap().to_string();

    // Enqueue a run — no claim loop is running here, so the task stays `queued`.
    let run = c
        .call(methods::HANGAR_BOARD_CARD_RUN, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id, "mode": "headless" }))
        .await;
    let task_id = run["result"]["task_id"].as_str().unwrap().to_string();
    let status: String = sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = ?")
        .bind(&task_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(status, "queued", "the run is queued-but-unclaimed");

    // Cancel before it ever starts → the DB flip cancels it.
    let cancelled = c
        .call(methods::HANGAR_BOARD_CARD_CANCEL, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }))
        .await;
    assert!(cancelled["error"].is_null(), "cancel must ack: {cancelled}");
    assert_eq!(cancelled["result"]["cancelled"], true, "a queued run cancels: {cancelled}");
    let status: String = sqlx::query_scalar("SELECT status FROM agent_task_queue WHERE id = ?")
        .bind(&task_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(status, "cancelled", "the queued task flipped straight to cancelled");

    // A second cancel is an idempotent no-op success (the card has no active task).
    let again = c
        .call(methods::HANGAR_BOARD_CARD_CANCEL, serde_json::json!({ "workspace_id": WS_SLUG, "board_id": board_id, "issue_id": issue_id }))
        .await;
    assert!(again["error"].is_null(), "a repeat cancel is not an error: {again}");
    assert_eq!(again["result"]["cancelled"], false, "nothing left to cancel: {again}");
}
