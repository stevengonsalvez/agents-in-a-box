//! ccc (agents-in-a-box-jgb) — the INTERACTIVE board-card RUN e2e tripwire.
//!
//! The lu5 tripwire (`tripwire_ccc_board_card_run_e2e`) drives a card through the
//! HEADLESS `Run ▾` path: `board_card_run` enqueues a task the claim loop runs as
//! a captured `claude -p` subprocess. This tripwire closes the D6 interactive gap
//! the `jgb` bead is about: when the user picks INTERACTIVE, the runner must spawn
//! the agent as a REAL, attachable tmux session per task (not the headless pipe
//! path), record the exact session name on the row, finalize the task when the
//! session exits, and auto-move the card — with NO pre-seeded card:
//!
//! ```text
//!  open Boards (B) ─▶ c: type title ─▶ Enter: pick profile ─▶ Enter: create
//!         │
//!         ▼
//!  Enter: Run ▾ ─▶ ↓ Interactive ─▶ Enter ─▶ hangar/board_card_run mode=interactive
//!         │
//!         ▼
//!  runner spawns REAL tmux session `tmux_hangar-<task_id>` (recorded on row)
//!         │  ── tripwire asserts the session is LIVE, then releases the agent ──
//!         ▼
//!  agent exits ─▶ session reaped ─▶ task done ─▶ D8 auto-move ─▶ card green
//! ```
//!
//! The proof the interactive path (not the headless one) ran is store truth: only
//! an interactive dispatch records a `tmux_hangar-<task_id>` session name on the
//! task row, and `tmux has-session` confirms the daemon actually created it. The
//! agent stub BLOCKS on a `$HOME/interactive-go` sentinel so the "session is real"
//! assertion is race-free — the tripwire observes the live session, then touches
//! the sentinel to let the agent finish. SKIPs cleanly when tmux / the binaries /
//! the staged plugin are absent. Follows the `tmux-ui-tripwire` HARD RULES:
//! exact-name kills only, deadline-bounded polls, POSITIVE + NEGATIVE assertions.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_DONE_COL, BOARD_RUN_PROFILE, BOARD_RUN_TODO_COL, INTERACTIVE_RELEASE_SENTINEL,
    TuiSession, board_card_by_title, budget_scale, can_run_tripwire, interactive_session_for_title,
    prepare_pipeline_board_run_interactive, skip, tmux_session_live,
};

/// The distinctive card title the tripwire types — greppable on the pane and in
/// the db, and it can never alias a column header (`Todo` / `Done`) or chrome.
const CARD_TITLE: &str = "Interactivecardtripwire";

/// The exact prefix the interactive runner mints session names under
/// (`tmux_hangar-<task_id>`); its presence on the row is the proof the interactive
/// path (not the headless one) dispatched this card.
const SESSION_PREFIX: &str = "tmux_hangar-";

#[test]
fn running_a_card_interactively_spawns_a_real_tmux_session_then_reaps_and_greens() {
    if !can_run_tripwire() {
        skip("ccc_board_card_run_interactive_e2e");
        return;
    }

    let pipe = prepare_pipeline_board_run_interactive();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();

    // Open the Boards screen; wait for the seeded "Delivery" board (no cards yet).
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

    // `c` opens the title input (re-sent until it opens — the first frames after a
    // tab switch can drop a lone keystroke).
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
    // The menu offers BOTH modes; the tripwire drives the Interactive one.
    assert!(
        run_menu.contains("Interactive"),
        "the Run ▾ menu must offer the interactive mode:\n{run_menu}"
    );

    // ↓ moves the highlight from Headless (default) to Interactive; Enter launches
    // it → hangar/board_card_run mode=interactive. (If the ↓ were dropped and a
    // headless run launched, the row's session_name would stay NULL and the poll
    // below would time out — a clear failure, not a silent pass.)
    sess.send_key("Down");
    sess.send_enter();

    // POSITIVE (store truth): the interactive runner records a
    // `tmux_hangar-<task_id>` session name on the card's task row. Poll the db,
    // bounded, until it appears.
    let name_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut session_name = None;
    while Instant::now() < name_deadline {
        if let Some(name) = interactive_session_for_title(pipe.home(), CARD_TITLE) {
            session_name = Some(name);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let session_name = session_name.unwrap_or_else(|| {
        panic!("the interactive run never recorded a session name on the card's task row")
    });
    assert!(
        session_name.starts_with(SESSION_PREFIX),
        "the recorded session name must be the interactive `tmux_hangar-*` name, got `{session_name}`"
    );

    // POSITIVE (system truth): a REAL tmux session exists under the recorded name.
    // The agent stub is blocked on the release sentinel, so the session is live
    // here; poll briefly to absorb the record→spawn ordering, then assert.
    let live_deadline = Instant::now() + Duration::from_secs(10 * scale);
    let mut saw_live = false;
    while Instant::now() < live_deadline {
        if tmux_session_live(&session_name) {
            saw_live = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        saw_live,
        "the interactive runner must spawn a REAL tmux session named `{session_name}`"
    );

    // Release the blocked agent so it exits 0 → the daemon reaps the session and
    // finalizes the task.
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go")
        .expect("write interactive release sentinel");

    // POSITIVE: the run finalizes to `done` and the D8 auto-move slides the
    // interactively-created card from Todo into the `done`-mapped Done column
    // (state=done, the card-green signal).
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

    // POSITIVE: the tmux session was REAPED once the agent exited (no orphan pane).
    let reap_deadline = Instant::now() + Duration::from_secs(15 * scale);
    let mut reaped = false;
    while Instant::now() < reap_deadline {
        if !tmux_session_live(&session_name) {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Kill the TUI tmux session by exact name before the assertions.
    drop(sess);

    let (col, state) = landed
        .unwrap_or_else(|| panic!("the created card never appeared in the db under `{CARD_TITLE}`"));
    assert_eq!(
        col.as_deref(),
        Some(BOARD_RUN_DONE_COL),
        "the interactive run must auto-move the card into the Done column (was {col:?})"
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
    assert!(
        reaped,
        "the interactive tmux session `{session_name}` must be reaped after the agent exits"
    );
}

/// Re-send single-char `key` every ~1.5s until `pred` holds on the pane or
/// `deadline` passes (the first frames after a tab switch can drop a lone
/// keystroke). Returns the matching capture, or `None` on timeout.
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
