//! P4 / D8 e2e tripwire: a real `ainb-hangar-daemon` binary auto-moves a board
//! card when the card's task run reaches `done` — the full custom-board chain,
//! no mocks.
//!
//! ```text
//!  seed world + issue + BOARD(Todo, Done↦done auto-move) + card(issue→Todo)
//!         │
//!         ▼
//!  INSERT queued task(issue) ─▶ daemon claim ─▶ running ─▶ done (fake claude)
//!                                                              │
//!                                             board auto-move hook (D8)
//!                                                              ▼
//!                          card auto-moved Todo → Done  +  boards_list state=done
//! ```
//!
//! This is the acceptance leg from the phase spec — "create board w/ custom
//! columns → add card → run (fake exec) → succeeded → card green + auto-moved" —
//! driven end-to-end through the genuine release daemon binary configured via env
//! vars, exactly like [`tripwire_task_happy_path_claude_provider`]. It proves the
//! dispatch hook fires on a REAL FSM transition (not just the repo unit test): the
//! daemon runs the fake-claude task to `done`, the auto-move hook slides the card
//! to the `done`-mapped auto-move column, and the `boards_list` snapshot the TUI
//! renders reports the card in `Done` with `state = "done"` (the card-green
//! signal). SKIP cleanly when tmux is unavailable.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

mod tripwire_support;

use std::time::Duration;

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::repo::board::BoardRepo;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use tripwire_support::{DaemonSession, daemon_bin, fake_claude_happy, seed_world, wait_for_db};

#[tokio::test]
async fn board_card_auto_moves_and_greens_on_run_success() {
    if !tripwire_support::tmux_available() {
        eprintln!("tmux not available; skipping board auto-move e2e tripwire");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir home");
    let db_path = home.path().join("hangar.db");
    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;
    let ws = WorkspaceId::from_str(ids.workspace_id.clone()).expect("ws id");

    // An issue the card represents (`card = issue`, D8).
    let issue_id = "issue-board-1";
    IssueRepo::insert(
        &pool,
        &NewIssue {
            id: issue_id.into(),
            workspace_id: ids.workspace_id.clone(),
            title: "Ship the boards".into(),
            description: None,
            state: "open".into(),
            assignee: None,
            creator: ainb_hangar_core::actor::ActorRef::new(
                ainb_hangar_core::actor::ActorKind::Member,
                "user-trip",
            )
            .unwrap(),
            created_at: tripwire_support::now_ms(),
            priority: 0,
            due_date: None,
            labels: Vec::new(),
        },
    )
    .await
    .expect("insert issue");

    // Create a board with a manual `Todo` column and a `done`-mapped auto-move
    // `Done` column, then place the issue card in `Todo`.
    BoardRepo::create(&pool, &ws, "board-1", "Delivery", tripwire_support::now_ms())
        .await
        .expect("create board");
    BoardRepo::column_add(&pool, &ws, "board-1", "col-todo", "Todo", None, false)
        .await
        .expect("add Todo column");
    BoardRepo::column_add(&pool, &ws, "board-1", "col-done", "Done", Some("done"), true)
        .await
        .expect("add auto-move Done column");
    BoardRepo::card_add(&pool, &ws, "board-1", issue_id, Some("col-todo"), tripwire_support::now_ms())
        .await
        .expect("place card in Todo");

    // Precondition: the card sits in Todo before the run.
    let before = BoardRepo::list(&pool, &ws).await.expect("list before")[0].clone();
    let card_before = before.cards.iter().find(|c| c.issue_id == issue_id).expect("card present");
    assert_eq!(
        card_before.column_id.as_deref(),
        Some("col-todo"),
        "the card starts in Todo, not the Done column"
    );

    // Spawn the real daemon binary WITH the claim loop enabled (a fake claude that
    // succeeds), so the enqueued task actually runs to `done`.
    let fake_claude = fake_claude_happy(home.path(), "board-run-1");
    let session = DaemonSession::spawn(
        &daemon_bin(),
        home.path(),
        &[
            ("AINB_HANGAR_HOME", home.path().to_str().unwrap()),
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake_claude.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // Enqueue a queued task FOR THE CARD'S ISSUE. When it reaches `done`, the
    // daemon's board auto-move hook slides the card into the Done column.
    let task_id = "task-board-1";
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(issue_id)
    .bind(tripwire_support::now_ms())
    .execute(&pool)
    .await
    .expect("enqueue task");

    // Wait for the task to walk the FSM to `done`.
    let _ = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;

    // The auto-move hook fires AFTER the terminal finalize; poll the board until
    // the card lands in the Done column (bounded, to avoid a finalize/hook race).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut card_col = None;
    while std::time::Instant::now() < deadline {
        let boards = BoardRepo::list(&pool, &ws).await.expect("list after");
        if let Some(c) = boards[0].cards.iter().find(|c| c.issue_id == issue_id) {
            card_col = c.column_id.clone();
            if card_col.as_deref() == Some("col-done") {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(session); // kill the tmux session by exact name before assertions

    // POSITIVE: the card auto-moved from Todo to the `done`-mapped Done column.
    assert_eq!(
        card_col.as_deref(),
        Some("col-done"),
        "the card must auto-move to the Done column when its task reaches done"
    );

    // POSITIVE (card-green): the boards_list snapshot the TUI renders reports the
    // card in Done with state=done — the exact signal that turns the card green.
    let wire = ainb_hangar_daemon::rpc::snapshots::boards_list(&pool, &ids.workspace_id)
        .await
        .expect("boards_list snapshot");
    let done_col = wire[0]
        .columns
        .iter()
        .find(|c| c.id == "col-done")
        .expect("Done column present");
    let card = done_col
        .cards
        .iter()
        .find(|c| c.issue_id == issue_id)
        .expect("the card renders in the Done column");
    assert_eq!(
        card.state.as_deref(),
        Some("done"),
        "the rendered card reports state=done (card-green signal)"
    );

    // NEGATIVE: the Todo column no longer holds the card (it truly MOVED, not
    // duplicated).
    let todo_col = wire[0].columns.iter().find(|c| c.id == "col-todo").unwrap();
    assert!(
        !todo_col.cards.iter().any(|c| c.issue_id == issue_id),
        "the card must not remain in Todo after the auto-move"
    );
}

/// Open a `SQLite` WAL pool at `db_path` (creating the file if absent).
async fn open_pool(db_path: &std::path::Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
