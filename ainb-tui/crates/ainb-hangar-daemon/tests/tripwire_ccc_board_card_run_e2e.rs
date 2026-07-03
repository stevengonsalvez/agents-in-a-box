//! ccc (agents-in-a-box-lu5) — the interactive board-card RUN e2e tripwire.
//!
//! The P4 board auto-move tripwire (`tripwire_board_auto_move_e2e`) proves the
//! daemon's auto-move hook on a task it enqueues DIRECTLY via SQL — it never
//! drives the TUI, so the card interaction layer (create / assign / run from a
//! card) was untested and, as bead `lu5` found, entirely inert: the reducer
//! raised card intents the key router dropped. This tripwire closes that gap by
//! driving the REAL `ainb tui` binary through the whole J1–J3 card journey with
//! NO pre-seeded card:
//!
//! ```text
//!  open Boards (B) ─▶ c: type title ─▶ Enter: pick profile ─▶ Enter: create
//!         │
//!         ▼
//!  Enter: Run ▾ ─▶ Enter: headless ─▶ hangar/board_card_run enqueues a task
//!         │
//!         ▼
//!  daemon claim loop runs fake-claude ─▶ done ─▶ D8 auto-move
//!         │
//!         ▼
//!  card auto-moved Todo → Done + state=done (card-green)
//! ```
//!
//! The card is created INTERACTIVELY (typed title + picked assignee profile), so
//! a green regression here means the card layer went inert again. SKIPs cleanly
//! when tmux / the binaries / the staged plugin are absent.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_DONE_COL, BOARD_RUN_PROFILE, BOARD_RUN_TODO_COL, TuiSession, board_card_by_title,
    budget_scale, can_run_tripwire, prepare_pipeline_board_run, skip,
};

/// The distinctive card title the tripwire types — greppable on the pane and in
/// the db, and it can never alias a column header (`Todo` / `Done`) or chrome.
const CARD_TITLE: &str = "Cardruntripwire";

#[test]
fn creating_a_card_and_running_it_auto_moves_and_greens() {
    if !can_run_tripwire() {
        skip("ccc_board_card_run_e2e");
        return;
    }

    let pipe = prepare_pipeline_board_run();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();

    // Open the Boards screen; wait for the seeded "Delivery" board with its Todo +
    // Done columns (no cards yet).
    let boards_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let board = sess
        .switch_tab_until("B", boards_deadline, |c| {
            c.contains("Board: Delivery") && c.contains("Todo") && c.contains("Done")
        })
        .unwrap_or_else(|| panic!("Boards screen never rendered:\n{}", sess.capture()));
    // NEGATIVE: no card exists before the interactive create.
    assert!(
        !board.contains(CARD_TITLE),
        "the card must not exist before the tripwire creates it:\n{board}"
    );

    // `c` opens the title input. The first frames after a tab switch can race the
    // pane, dropping a lone keystroke, so re-send `c` until the input opens.
    let title_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let opened = press_until(&sess, "c", title_deadline, |c| c.contains("New card title"))
        .unwrap_or_else(|| panic!("card-title input never opened after `c`:\n{}", sess.capture()));
    assert!(
        opened.contains("New card title"),
        "the `c` key must open the card-title input:\n{opened}"
    );

    // Type the title (single literal run so tmux never coalesces / drops a char).
    sess.type_literal(CARD_TITLE);
    sess.poll_capture(Instant::now() + Duration::from_secs(10 * scale), |c| {
        c.contains(CARD_TITLE)
    })
    .unwrap_or_else(|| panic!("typed title never echoed:\n{}", sess.capture()));

    // Enter → assignee-profile pick; the seeded `claude-agent` profile is offered.
    sess.send_enter();
    let picker = sess
        .poll_capture(Instant::now() + Duration::from_secs(15 * scale), |c| {
            c.contains("Assignee profile") && c.contains(BOARD_RUN_PROFILE)
        })
        .unwrap_or_else(|| panic!("profile picker never offered the profile:\n{}", sess.capture()));
    assert!(
        picker.contains(BOARD_RUN_PROFILE),
        "the picker must offer the seeded assignee profile:\n{picker}"
    );

    // Enter commits the create — the card lands on the board in Todo.
    sess.send_enter();
    sess.poll_capture(Instant::now() + Duration::from_secs(20 * scale), |c| {
        c.contains(CARD_TITLE) && c.contains("Board: Delivery")
    })
    .unwrap_or_else(|| panic!("created card never rendered on the board:\n{}", sess.capture()));

    // The new card is focused (Todo, first card). Enter opens the `Run ▾` menu.
    let run_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut run_menu = None;
    while Instant::now() < run_deadline {
        sess.send_enter();
        if let Some(c) =
            sess.poll_capture(Instant::now() + Duration::from_millis(1500), |c| c.contains("Run ▾"))
        {
            run_menu = Some(c);
            break;
        }
    }
    let run_menu =
        run_menu.unwrap_or_else(|| panic!("Run ▾ menu never opened:\n{}", sess.capture()));
    assert!(
        run_menu.contains("Headless"),
        "the Run ▾ menu must offer the headless mode:\n{run_menu}"
    );

    // Enter launches the highlighted (headless) mode → hangar/board_card_run.
    sess.send_enter();

    // POSITIVE (store truth): the daemon claim loop runs the enqueued task to
    // `done`, and the D8 auto-move slides the interactively-created card from Todo
    // into the `done`-mapped Done column, reading state=done (the card-green
    // signal). Poll the db, bounded, to avoid a finalize/hook race.
    let move_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let mut landed = None;
    while Instant::now() < move_deadline {
        if let Some((col, state)) = board_card_by_title(pipe.home(), CARD_TITLE) {
            if col.as_deref() == Some(BOARD_RUN_DONE_COL) && state.as_deref() == Some("done") {
                landed = Some((col, state));
                break;
            }
            landed = Some((col, state));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Kill the tmux session by exact name before the assertions.
    drop(sess);

    let (col, state) = landed
        .unwrap_or_else(|| panic!("the created card never appeared in the db under `{CARD_TITLE}`"));
    assert_eq!(
        col.as_deref(),
        Some(BOARD_RUN_DONE_COL),
        "the run must auto-move the card into the Done column (was {col:?})"
    );
    assert_eq!(
        state.as_deref(),
        Some("done"),
        "the auto-moved card must read state=done (the card-green signal)"
    );

    // NEGATIVE: the card is not left behind in Todo (it truly MOVED).
    assert_ne!(
        col.as_deref(),
        Some(BOARD_RUN_TODO_COL),
        "the card must not remain in Todo after the run"
    );
}

/// Re-send single-char `key` every ~1.5s until `pred` holds on the pane or
/// `deadline` passes (the first frames after a tab switch can drop a lone
/// keystroke). Returns the matching capture, or `None` on timeout. Mirrors the
/// harness's own `switch_tab_until` retry shape for a verb key.
fn press_until(
    sess: &TuiSession,
    key: &str,
    deadline: Instant,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    loop {
        sess.send_key(key);
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), &pred) {
            return Some(c);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}
