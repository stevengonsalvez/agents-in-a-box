//! tcp T3 (agents-in-a-box-aau.3) — the card-CANCEL lifecycle tripwire (F6).
//!
//! The T1 worktree tripwire proves a card RUN provisions a worktree that is torn
//! down when the run finishes NATURALLY. This one closes the F6 cancel gap: a
//! human `X` on a RUNNING card must KILL the in-flight run, finalize the card as
//! `cancelled` (a distinct terminal state, never `failed`), and tear the run's
//! worktree / tmux session down — mid-run, while the agent is still blocked.
//!
//! ```text
//!  Boards (B) ─▶ create card ─▶ Run ▾ ─▶ (headless | ↓ interactive)
//!         │                    claim loop provisions a worktree / tmux session
//!         │  ── agent BLOCKS on the release sentinel; run is LIVE ──
//!         ▼
//!  X: cancel ─▶ board_card_cancel ─▶ kill the run ─▶ cancelled
//!         │      headless: SIGKILL the process group + remove the worktree
//!         ▼      interactive: kill the tmux session by exact name
//!  card state = cancelled ; worktree gone ; tmux session reaped
//! ```
//!
//! The blocking fake-agent holds the run so the "cancel a LIVE run" assertion is
//! race-free — the tripwire never releases the sentinel; the cancel is what stops
//! the agent. SKIPs cleanly when tmux / the binaries / the staged plugin are
//! absent. Follows the `tmux-ui-tripwire` HARD RULES: exact-name kills only,
//! deadline-bounded polls, POSITIVE (run live) + NEGATIVE (torn down) proofs.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_PROFILE, TuiSession, board_card_by_title, budget_scale, can_run_tripwire,
    drive_card_create_to_profile, interactive_session_for_title,
    prepare_pipeline_board_run_interactive, prepare_pipeline_worktree_run, skip,
    task_short_id_by_title, tmux_session_live, worktree_dir,
};

/// Open the Boards screen, create a card via the full F1-F4 overlay, and leave it
/// focused ready to run — the shared prefix of both cancel scenarios.
///
/// `repo_down` is the `@`-dropdown offset the card picks: `1` selects the seeded
/// SCANNED repo (a real worktree, for the headless teardown proof), `0` selects
/// the always-first scratch repo (for the interactive fixture, which seeds no
/// scanned repo — the tmux session reap is what's under test there).
fn create_focused_card(sess: &TuiSession, title: &str, repo_down: u32, scale: u64) {
    let boards_deadline = Instant::now() + Duration::from_secs(30 * scale);
    sess.switch_tab_until("B", boards_deadline, |c| {
        c.contains("Board: Delivery") && c.contains("Todo") && c.contains("Done")
    })
    .unwrap_or_else(|| panic!("Boards screen never rendered:\n{}", sess.capture()));

    // Full F1-F4 overlay: title → @-pick the repo → agent → profile.
    let picker = drive_card_create_to_profile(sess, title, repo_down, scale);
    assert!(
        picker.contains(BOARD_RUN_PROFILE),
        "the picker must offer the seeded assignee profile:\n{picker}"
    );
    // Enter commits the create; the new card focuses on the board.
    sess.send_enter();
    sess.poll_capture(Instant::now() + Duration::from_secs(20 * scale), |c| {
        c.contains(title) && c.contains("Board: Delivery")
    })
    .unwrap_or_else(|| panic!("created card never rendered on the board:\n{}", sess.capture()));
}

/// Open the `Run ▾` menu over the focused card (re-sent — a lone Enter can drop).
fn open_run_menu(sess: &TuiSession, scale: u64) {
    let deadline = Instant::now() + Duration::from_secs(20 * scale);
    while Instant::now() < deadline {
        sess.send_enter();
        if sess
            .poll_capture(Instant::now() + Duration::from_millis(1500), |c| c.contains("Run ▾"))
            .is_some()
        {
            return;
        }
    }
    panic!("Run ▾ menu never opened:\n{}", sess.capture());
}

/// Cancel the focused card via `X` → the confirm overlay → Enter, polling the
/// store until its latest task reaches `cancelled`, bounded. `X` opens the
/// confirm (a no-op if already open); Enter confirms. Cancel is idempotent (a
/// double-cancel is a no-op), so re-driving absorbs a dropped keystroke without
/// over-cancelling.
fn cancel_until_terminal(sess: &TuiSession, home: &std::path::Path, title: &str, scale: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(40 * scale);
    while Instant::now() < deadline {
        // `X` opens the confirm overlay; Enter fires the cancel.
        sess.type_literal("X");
        std::thread::sleep(Duration::from_millis(150));
        sess.send_enter();
        if let Some((_, state)) = board_card_by_title(home, title) {
            if state.as_deref() == Some("cancelled") {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Cancelling a live HEADLESS run finalizes the card `cancelled` and tears its
/// provisioned worktree down (F6: kill the process group + remove the worktree).
#[test]
fn cancelling_a_headless_run_cancels_the_card_and_tears_down_its_worktree() {
    if !can_run_tripwire() {
        skip("tcp_card_cancel_headless_e2e");
        return;
    }

    let title = "Cancelheadlesstripwire";
    let pipe = prepare_pipeline_worktree_run();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();

    // repo_down = 1: pick the seeded SCANNED repo so the run provisions a real
    // worktree the cancel must tear down.
    create_focused_card(&sess, title, 1, scale);
    open_run_menu(&sess, scale);
    sess.send_enter(); // headless launch → hangar/board_card_run

    // Read the dispatched task's short-id → the worktree dir the run provisions.
    let id_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut slug = None;
    while Instant::now() < id_deadline {
        if let Some(s) = task_short_id_by_title(pipe.home(), title) {
            slug = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let slug = slug.unwrap_or_else(|| panic!("no task was dispatched for `{title}`"));
    let worktree = worktree_dir(pipe.home(), &slug);

    // POSITIVE: while the blocked agent holds the run, the worktree is LIVE — so
    // the cancel below stops a genuinely-running task with a real checkout.
    let live_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut saw_worktree = false;
    while Instant::now() < live_deadline {
        if worktree.join(".git").exists() {
            saw_worktree = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        saw_worktree,
        "the run must provision a live worktree at {} before the cancel\npane:\n{}",
        worktree.display(),
        sess.capture()
    );

    // Cancel the running card with `X` (never release the sentinel — the cancel
    // is what stops the agent).
    let cancelled = cancel_until_terminal(&sess, pipe.home(), title, scale);

    // NEGATIVE (F6 teardown): the worktree is removed once the cancel finalizes.
    let teardown_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut torn_down = false;
    while Instant::now() < teardown_deadline {
        if !worktree.exists() {
            torn_down = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let pane = sess.capture();
    drop(sess); // kill the TUI tmux session by exact name before the assertions.

    assert!(cancelled, "the running card must finalize to `cancelled`\npane:\n{pane}");
    assert!(
        torn_down,
        "the cancelled run's worktree at {} must be torn down",
        worktree.display()
    );
    // Store cross-check: the card is terminal-cancelled, not `failed` (a cancel is
    // a distinct terminal state, never the failure path).
    let (_, state) = board_card_by_title(pipe.home(), title)
        .unwrap_or_else(|| panic!("the card vanished from the db"));
    assert_eq!(
        state.as_deref(),
        Some("cancelled"),
        "the killed run must be cancelled, not {state:?}"
    );
}

/// Cancelling a live INTERACTIVE run reaps its `tmux_hangar-<task_id>` session by
/// exact name and finalizes the card `cancelled` (F6: the interactive kill path).
#[test]
fn cancelling_an_interactive_run_reaps_its_tmux_session() {
    if !can_run_tripwire() {
        skip("tcp_card_cancel_interactive_e2e");
        return;
    }

    let title = "Cancelinteractivetripwire";
    let pipe = prepare_pipeline_board_run_interactive();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();

    // repo_down = 0: the interactive fixture seeds no scanned repo — scratch is
    // fine (the tmux session reap, not a worktree, is under test here).
    create_focused_card(&sess, title, 0, scale);
    open_run_menu(&sess, scale);
    // ↓ selects Interactive (Headless is the default), Enter launches it.
    sess.send_key("Down");
    sess.send_enter();

    // POSITIVE (store truth): the interactive runner records the exact
    // `tmux_hangar-<task_id>` session name on the card's task row.
    let name_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut session_name = None;
    while Instant::now() < name_deadline {
        if let Some(name) = interactive_session_for_title(pipe.home(), title) {
            session_name = Some(name);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let session_name = session_name
        .unwrap_or_else(|| panic!("the interactive run never recorded a session name"));

    // POSITIVE (system truth): a REAL tmux session is live under that name.
    let live_deadline = Instant::now() + Duration::from_secs(15 * scale);
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
        "the interactive runner must spawn a REAL tmux session `{session_name}` before the cancel"
    );

    // Cancel the running card with `X` — the daemon kills the tmux session by its
    // exact name (never a wildcard / kill-server).
    let cancelled = cancel_until_terminal(&sess, pipe.home(), title, scale);

    // NEGATIVE (F6 reap): the interactive session is gone once the cancel lands.
    let reap_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut reaped = false;
    while Instant::now() < reap_deadline {
        if !tmux_session_live(&session_name) {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let pane = sess.capture();
    drop(sess); // kill the TUI tmux session by exact name before the assertions.

    assert!(cancelled, "the running card must finalize to `cancelled`\npane:\n{pane}");
    assert!(
        reaped,
        "the cancelled interactive run's tmux session `{session_name}` must be reaped"
    );
    let (_, state) = board_card_by_title(pipe.home(), title)
        .unwrap_or_else(|| panic!("the card vanished from the db"));
    assert_eq!(
        state.as_deref(),
        Some("cancelled"),
        "the killed interactive run must be cancelled, not {state:?}"
    );
}
