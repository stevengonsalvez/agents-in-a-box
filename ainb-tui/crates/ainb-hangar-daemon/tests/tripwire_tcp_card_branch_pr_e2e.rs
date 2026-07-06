//! tcp T2 (agents-in-a-box-aau.2) — the card BRANCH + PR surfacing tripwire.
//!
//! The T1 worktree tripwires prove a card runs in a volatile worktree that is
//! torn down clean. This one closes the T2 surfacing gap: a card run whose agent
//! COMMITS in its worktree must leave a durable `ainb/<slug>` branch (surviving
//! `git worktree remove`) that the card SHOWS, and a PR the run "opened" must
//! render a PR chip + CI status on the card — all through the real `ainb tui`.
//!
//! ```text
//!  Boards (B) ─▶ create card on testrepo ─▶ Run ▾ ─▶ headless
//!         │
//!         ▼
//!  claim loop provisions a WORKTREE ─▶ fake agent COMMITS + prints a PR url
//!         │      (branch ainb/<slug> now ahead of base)
//!         ▼
//!  finalize records the branch + captures result.pr_url ─▶ done
//!         │      TaskFinished ─▶ plugin re-pulls tasks_list
//!         ▼      (tasks_list fetches PR status via the STUB gh under HANGAR_GH_PATH)
//!  the card shows  ·ainb/<slug>·  and  ·PR ✓·   (branch + PR chip + CI status)
//! ```
//!
//! The PR association is seeded through the REAL plumbing, never real GitHub: the
//! agent prints a canonical `gh pr create` url line (captured by the P9.1 scan)
//! and the daemon's `gh` is a stub (`HANGAR_GH_PATH`) answering a passing /
//! mergeable / open PR. SKIPs cleanly when tmux / the binaries / git / the staged
//! plugin are absent. Follows the `tmux-ui-tripwire` HARD RULES: exact-name kills
//! only, deadline-bounded polls, POSITIVE (branch + PR on the card) + NEGATIVE
//! (neither present before the run) proofs.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_DONE_COL, BOARD_RUN_PROFILE, TuiSession, WORKTREE_REPO_NAME, board_card_by_title,
    budget_scale, can_run_tripwire, drive_card_create_to_profile, dump_daemon_logs,
    git_branch_exists, prepare_pipeline_worktree_pr, skip, task_branch_by_title,
    task_short_id_by_title, worktree_branch, worktree_dir,
};

/// The distinctive card title the tripwire types — greppable, never aliases a
/// column header (`Todo` / `Done`) or chrome.
const CARD_TITLE: &str = "Cardbranchprtripwire";

#[test]
fn a_committed_card_run_surfaces_its_branch_and_pr_on_the_card() {
    if !can_run_tripwire() {
        skip("tcp_card_branch_pr_e2e");
        return;
    }

    let pipe = prepare_pipeline_worktree_pr();
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
    // NEGATIVE: neither the card nor any branch / PR chip exists before the run.
    assert!(
        !board.contains(CARD_TITLE) && !board.contains("ainb/") && !board.contains("PR ✓"),
        "no card / branch / PR chip must exist before the tripwire creates one:\n{board}"
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
    .unwrap_or_else(|| {
        panic!(
            "created card never rendered on the board:\n{}",
            sess.capture()
        )
    });

    let run_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut opened = false;
    while Instant::now() < run_deadline {
        sess.send_enter();
        if sess
            .poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
                c.contains("Run ▾")
            })
            .is_some()
        {
            opened = true;
            break;
        }
    }
    assert!(opened, "Run ▾ menu never opened:\n{}", sess.capture());
    sess.send_enter(); // headless launch → hangar/board_card_run

    // Read the dispatched task's short-id → the expected `ainb/<slug>` branch.
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
    let branch = worktree_branch(&slug);
    let worktree = worktree_dir(pipe.home(), &slug);

    // Store truth: the committed run finalises to `done`, records its branch on the
    // task row, and the branch really exists in the origin repo (survives teardown).
    let done_deadline = Instant::now() + Duration::from_secs(40 * scale);
    let mut card_state = None;
    let mut recorded_branch = None;
    while Instant::now() < done_deadline {
        if let Some((_, state)) = board_card_by_title(pipe.home(), CARD_TITLE) {
            card_state = state.clone();
            recorded_branch = task_branch_by_title(pipe.home(), CARD_TITLE);
            if state.as_deref() == Some("done") && recorded_branch.is_some() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        card_state.as_deref(),
        Some("done"),
        "the committed run must finalize to done (was {card_state:?})"
    );
    assert_eq!(
        recorded_branch.as_deref(),
        Some(branch.as_str()),
        "finalize must record the run's ainb/<slug> branch on the task row"
    );
    assert!(
        git_branch_exists(&repo, &branch),
        "the branch must survive teardown in the origin repo (the durable artifact)"
    );
    // The clean worktree is torn down, but the branch remains — that is the point.
    assert!(
        !worktree.exists(),
        "the clean worktree is torn down after the run"
    );

    // POSITIVE (tcp T2): the Kanban board's TASK card SHOWS the durable branch AND
    // a PR chip carrying the stub gh's passing CI. The TaskFinished re-pull already
    // folded branch + PR + status onto the card cache, so switching to the board
    // (`K`) renders them on the finished task's tile in the `done` column.
    //
    // The run branch is `ainb/<task-id>` keyed on the FULL task ULID (tcp vpm made
    // it collision-safe), which is wider than the compact Kanban tile — so the card
    // SOFT-WRAPS the branch mid-string and the full value is never contiguous in the
    // tmux capture (the adjacent column also interleaves the wrapped halves). The
    // EXACT branch is already pinned above (recorded on the task row + present in
    // git), so the tile check proves the card SURFACES the run's artifacts: the
    // `PR ✓` chip plus the branch's leading `ainb/<ulid-head>`, a deterministic
    // prefix guaranteed to fit the first tile line before the wrap.
    let branch_head = &branch[..branch.len().min(15)];
    let surface_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let surfaced = sess.switch_tab_until("K", surface_deadline, |c| {
        c.contains("done (") && c.contains(branch_head) && c.contains("PR ✓")
    });

    // Kill the TUI tmux session by exact name before the final assertions.
    let pane = sess.capture();
    drop(sess);

    assert!(
        surfaced.is_some(),
        "the Kanban task card must surface its branch `{branch}` (matched on the \
         pre-wrap head `{branch_head}`) AND a passing `PR ✓` chip\npane:\n{pane}"
    );
    // NEGATIVE cross-check: the card is in Done (the run finished), so the branch +
    // PR read on a finished card, not a phantom render.
    let (col, _) = board_card_by_title(pipe.home(), CARD_TITLE)
        .unwrap_or_else(|| panic!("the card vanished from the db"));
    let daemon_logs = dump_daemon_logs(pipe.home());
    assert_eq!(
        col.as_deref(),
        Some(BOARD_RUN_DONE_COL),
        "the finished card must be in the Done column (was {col:?})\n\
         daemon logs:\n{daemon_logs}"
    );
}
