//! tcp T1 (agents-in-a-box-aau.1) — the F5 per-run worktree LIFECYCLE tripwire.
//!
//! The card-run e2e tripwires prove a card runs to `done` in the SCRATCH repo.
//! This one closes the F5 gap the T1 bead is about: a card created on a REAL git
//! repo must run in a volatile git WORKTREE on branch `ainb/<slug>`, isolated from
//! the origin working tree, and that worktree must be torn down when the run
//! finishes clean. It drives the REAL `ainb tui` through the FULL F1-F4 parity
//! overlay, picking the seeded repo from the `@` autocomplete dropdown:
//!
//! ```text
//!  Boards (B) ─▶ c: title ─▶ @: pick testrepo ─▶ agent ─▶ profile ─▶ create
//!         │
//!         ▼
//!  Run ▾ ─▶ Enter: headless ─▶ claim loop provisions a WORKTREE
//!         │      ~/.agents-in-a-box/worktrees/<slug> on branch ainb/<slug>
//!         │  ── agent BLOCKS; tripwire asserts the worktree + branch are live ──
//!         ▼
//!  release ─▶ agent exits ─▶ done ─▶ teardown removes the CLEAN worktree
//! ```
//!
//! The blocking fake-claude holds the run so the live-worktree assertion is
//! race-free (the sandbox is disabled for the fixture so the headless agent can
//! read the `$HOME` release sentinel — the worktree LIFECYCLE, not the sandbox,
//! is under test). SKIPs cleanly when tmux / the binaries / the staged plugin are
//! absent. Follows the `tmux-ui-tripwire` HARD RULES: exact-name kills only,
//! deadline-bounded polls, POSITIVE (worktree live) + NEGATIVE (torn down) proofs.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_DONE_COL, BOARD_RUN_PROFILE, INTERACTIVE_RELEASE_SENTINEL, TuiSession,
    WORKTREE_REPO_NAME, board_card_by_title, budget_scale, can_run_tripwire,
    drive_card_create_to_profile, git_branch_exists, prepare_pipeline_worktree_run, skip,
    task_short_id_by_title, worktree_branch, worktree_dir,
};

/// The distinctive card title the tripwire types — greppable, never aliases a
/// column header (`Todo` / `Done`) or chrome.
const CARD_TITLE: &str = "Worktreelifecycletripwire";

#[test]
fn running_a_card_on_a_repo_provisions_a_worktree_then_tears_it_down() {
    if !can_run_tripwire() {
        skip("tcp_worktree_lifecycle_e2e");
        return;
    }

    let pipe = prepare_pipeline_worktree_run();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();
    let repo = pipe.home().join(WORKTREE_REPO_NAME);

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

    // Drive the FULL F1-F4 overlay: title → @-pick the SCANNED repo (repo_down = 1,
    // the entry after the always-first scratch) → agent → the profile picker.
    let picker = drive_card_create_to_profile(&sess, CARD_TITLE, 1, scale);
    assert!(
        picker.contains(BOARD_RUN_PROFILE),
        "the picker must offer the seeded assignee profile:\n{picker}"
    );

    // Enter commits the create; Enter again opens Run ▾; Enter launches headless.
    sess.send_enter();
    sess.poll_capture(Instant::now() + Duration::from_secs(20 * scale), |c| {
        c.contains(CARD_TITLE) && c.contains("Board: Delivery")
    })
    .unwrap_or_else(|| panic!("created card never rendered on the board:\n{}", sess.capture()));

    let run_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut opened = false;
    while Instant::now() < run_deadline {
        sess.send_enter();
        if sess
            .poll_capture(Instant::now() + Duration::from_millis(1500), |c| c.contains("Run ▾"))
            .is_some()
        {
            opened = true;
            break;
        }
    }
    assert!(opened, "Run ▾ menu never opened:\n{}", sess.capture());
    sess.send_enter(); // headless launch → hangar/board_card_run

    // The task is dispatched — read its short-id (the worktree dir name + branch
    // suffix). Poll the db until the claim loop has minted the task.
    let id_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut slug = None;
    while Instant::now() < id_deadline {
        if let Some(s) = task_short_id_by_title(pipe.home(), CARD_TITLE) {
            slug = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let slug = slug.unwrap_or_else(|| panic!("no task was dispatched for the card `{CARD_TITLE}`"));
    let worktree = worktree_dir(pipe.home(), &slug);
    let branch = worktree_branch(&slug);

    // POSITIVE (F5): while the blocked agent holds the run, the volatile worktree
    // exists on branch `ainb/<slug>` in the origin repo. (If the `@` dropdown had
    // NOT offered the scanned repo, the run would have provisioned scratch instead
    // and this would never appear — so it also proves the repo was picked.)
    let live_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut saw_worktree = false;
    while Instant::now() < live_deadline {
        if worktree.join(".git").exists() && git_branch_exists(&repo, &branch) {
            saw_worktree = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        saw_worktree,
        "the run must provision a worktree at {} on branch {branch}\npane:\n{}",
        worktree.display(),
        sess.capture()
    );

    // Release the blocked agent → it exits 0 → the run finalizes → the CLEAN
    // worktree is torn down (keep-if-dirty: nothing was written, so it is removed).
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go")
        .expect("write release sentinel");

    // POSITIVE (F5 teardown): the worktree checkout is removed after the run.
    let teardown_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut torn_down = false;
    while Instant::now() < teardown_deadline {
        if !worktree.exists() {
            torn_down = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Store truth: the run reached done + auto-moved (card green).
    let done_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut done = None;
    while Instant::now() < done_deadline {
        if let Some((col, state)) = board_card_by_title(pipe.home(), CARD_TITLE) {
            done = Some((col, state));
            if done.as_ref().is_some_and(|(_, s)| s.as_deref() == Some("done")) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Kill the TUI tmux session by exact name before the assertions.
    drop(sess);

    assert!(
        torn_down,
        "the clean worktree at {} must be torn down after the run",
        worktree.display()
    );
    let (col, state) = done.unwrap_or_else(|| panic!("the card never appeared in the db"));
    assert_eq!(
        state.as_deref(),
        Some("done"),
        "the worktree run must finalize to done (was {state:?})"
    );
    assert_eq!(
        col.as_deref(),
        Some(BOARD_RUN_DONE_COL),
        "the finished card must auto-move into the Done column (was {col:?})"
    );
}
