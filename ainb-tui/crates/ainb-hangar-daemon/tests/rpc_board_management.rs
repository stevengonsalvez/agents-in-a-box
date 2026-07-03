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
