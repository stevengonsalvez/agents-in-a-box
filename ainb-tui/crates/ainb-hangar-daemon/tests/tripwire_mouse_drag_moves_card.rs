//! 63l.7 — real-tmux MOUSE tripwire: a left-button DRAG across columns on the
//! live Issues board visibly MOVES a card to a new status column.
//!
//! This is the user-visible proof of the mouse-driven board redesign end to end:
//! not a synthetic `MouseEvent` fed to the FSM (covered by `mouse_fsm_test.rs`),
//! nor a mock-daemon RPC assertion (covered by `issue_board_drag_rpc_over_socket`),
//! but a REAL SGR mouse sequence (`ESC[<0;col;rowM` press, `ESC[<32;…M` drag,
//! `ESC[<0;…m` release) driven against the actual `ainb tui` process over tmux,
//! whose `capture-pane` then shows the card left its origin column.
//!
//! ```text
//!  seed hangar.db (3 Todo issues) ──▶ ainb-hangar-daemon (RPC only)
//!         ▲ hangar/issue_update{state}              │ IssueUpdated push
//!  ainb tui (tmux) ──`g`──▶ Issues board ──SGR drag──▶ card re-buckets column
//!                                            │
//!                  capture-pane: `Todo (3)` → `Todo (2)` (card left Todo)
//! ```
//!
//! ## Traps respected (`reference_tmux_tripwire_flake_patterns`)
//!
//!   * **settle-on-observed-change**: the post-drag assertion gates on a DIFF from
//!     the pre-drag frame (`Todo (3)` → `Todo (2)`), never a fixed sleep.
//!   * **coordinate resolution from the capture**: the press/drop cells are
//!     resolved by locating the card text + the destination column header in the
//!     captured pane, so the test never hardcodes fragile board geometry that
//!     would drift with the chrome.
//!   * **exact-name tmux kill only** (via `TuiSession`'s `Drop`).
//!
//! Heavy (launches the full TUI + daemon + real drag), so it is pruned out of the
//! `HANGAR_TRIPWIRE_SMOKE` macOS leg by `run_all_tripwires.sh`; the Linux leg runs
//! it full. SKIPs (never fails) when tmux / the binaries / the staged plugin are
//! absent.

#![allow(clippy::duration_suboptimal_units)] // `from_secs` reads fine as a budget.

#[path = "tripwire_p4_common.rs"]
mod common;

use std::time::{Duration, Instant};

use common::{TuiSession, budget_scale, can_run_tripwire, prepare_pipeline, skip};

/// The 1-based `(col, row)` of the first cell of `needle` in the captured pane,
/// or `None` when it is not on screen. tmux `capture-pane` is row-major; the
/// returned coordinates are SGR-mouse 1-based (top-left cell = `1;1`).
fn cell_of(capture: &str, needle: &str) -> Option<(u16, u16)> {
    for (y, line) in capture.lines().enumerate() {
        if let Some(byte_idx) = line.find(needle) {
            // The board labels/ids are ASCII, so the byte index is the char column.
            let col = u16::try_from(byte_idx + 1).ok()?;
            let row = u16::try_from(y + 1).ok()?;
            return Some((col, row));
        }
    }
    None
}

#[test]
fn mouse_drag_moves_card_to_new_column() {
    if !can_run_tripwire() {
        skip("mouse drag moves card tripwire");
        return;
    }
    let scale = budget_scale();

    let pipe = prepare_pipeline();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");

    // Launch the TUI and land on a NON-EMPTY issue list. The shared pipeline waits
    // only for the daemon SOCKET to appear, not for its first issue snapshot to be
    // servable, so on a busy box the plugin's first `issues_list` RPC can race the
    // daemon's DB open and the board renders with all-zero column counts (no rows).
    // That is a launch/snapshot race orthogonal to the mouse drag this tripwire
    // proves, so a fresh session is relaunched (up to 3 attempts) until the seeded
    // `Todo (3)` board is on screen — then the drag runs against a real card.
    let (sess, landing) = {
        let mut attempt: Option<(TuiSession, String)> = None;
        for _ in 0..3 {
            let sess = TuiSession::spawn(&bin, pipe.home());
            if let Some(landing) = sess.open_hangar_and_wait_ready() {
                if landing.contains("Todo (3)") {
                    attempt = Some((sess, landing));
                    break;
                }
            }
            // Empty / never-rendered board: drop this session (exact-name kill) and
            // retry with a fresh launch.
            drop(sess);
        }
        attempt.unwrap_or_else(|| panic!("seeded Todo (3) issue board never rendered for the drag"))
    };

    // PRE-DRAG: the three seeded issues sit in Todo — the board shows `Todo (3)`
    // and the `Refactor API` card. The In Progress column is empty (`In Progress
    // (0)`). This is the frame the post-drag assertion diffs against.
    assert!(
        landing.contains("Todo (3)"),
        "expected the three seeded issues in Todo before the drag:\n{landing}"
    );
    assert!(
        landing.contains("In Progress (0)"),
        "In Progress must start empty before the drag:\n{landing}"
    );

    // Resolve the drop target: the `In Progress` column header gives the x of that
    // column; drop a few rows below it, inside the column body / drop zone. The
    // header position is stable for the whole drag (the destination column never
    // moves), so it is resolved once from the landing frame.
    let (prog_col, prog_row) =
        cell_of(&landing, "In Progress").expect("In Progress column header on screen");
    // Drop well inside the In Progress column body: a couple cells right of the
    // header glyph (still within the column) and several rows below the header.
    let drop_col = prog_col + 4;
    let drop_row = prog_row + 5;

    // SETTLE-ON-CHANGE with a RE-ISSUED gesture: drive the full SGR drag, then poll
    // for the board to re-bucket — `Todo` drops to 2 and In Progress gains the card
    // (`In Progress (1)`). A single one-shot gesture can race the first board frame
    // (the hit-map is rebuilt per render, so a press a tick before the board
    // settled lands on no card) or be coalesced by tmux, so — like the key-nav
    // `switch_tab_until` helper — the gesture is re-issued every ~1.5s until the
    // observed count diff appears, bounded by the deadline. The card cell is
    // re-resolved from a fresh capture each attempt so a card that already moved is
    // never pressed at a stale slot. The plugin moves the card optimistically on
    // release AND the daemon's IssueUpdated push reconciles it, so the count change
    // is the visible proof the drag took effect (never a fixed sleep).
    let moved_pred = |c: &str| c.contains("Todo (2)") && c.contains("In Progress (1)");
    let deadline = Instant::now() + Duration::from_secs(45 * scale);
    let mut after: Option<String> = None;
    while Instant::now() < deadline {
        // Re-resolve the card cell from the live pane (it may have moved already).
        let now = sess.capture();
        if moved_pred(&now) {
            after = Some(now);
            break;
        }
        if let Some((card_col, card_row)) = cell_of(&now, "Refactor API") {
            // Press on the card title row (a cell inside the card's hit rect), drag
            // into the In Progress column body, release there.
            sess.mouse_press(card_col + 1, card_row);
            sess.mouse_drag(drop_col, drop_row);
            sess.mouse_release(drop_col, drop_row);
        }
        // Give the gesture a window to settle into a re-bucketed frame before the
        // next re-issue (deadline-bounded poll, not a bare sleep).
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
            moved_pred(c)
        }) {
            after = Some(c);
            break;
        }
    }

    let after = after.unwrap_or_else(|| {
        panic!(
            "the dragged card never moved columns (still:\n{}\n)",
            sess.capture()
        )
    });

    // POSITIVE: the card landed in In Progress (count went 0 → 1) and left Todo
    // (count went 3 → 2).
    assert!(
        after.contains("In Progress (1)"),
        "the dragged card must land in In Progress:\n{after}"
    );
    assert!(
        after.contains("Todo (2)"),
        "the dragged card must leave the Todo column:\n{after}"
    );
    // NEGATIVE: the pre-drag counts are gone — proof of a real transition, not a
    // pane that already matched.
    assert!(
        !after.contains("Todo (3)"),
        "Todo must no longer show all three issues after the drag:\n{after}"
    );

    drop(sess);
    drop(pipe);
}
