//! tcp T3 (agents-in-a-box-aau.3) — the card-RERUN lifecycle tripwire (F6).
//!
//! Rerun rides the existing `Enter` → `Run ▾` affordance: pressing it on a
//! FINISHED card must enqueue a FRESH task (a new `agent_task_queue` row, a new
//! `ainb/<slug>` worktree), respecting the per-(issue, agent) claim guard — a
//! second run only lands because the first is terminal. This tripwire proves the
//! second run is genuinely fresh (a DISTINCT task short-id + its own branch) and
//! greens the card again, all through the real `ainb tui`.
//!
//! ```text
//!  create card ─▶ Run ▾ ─▶ headless ─▶ run #1 commits ─▶ done (slug1, ainb/<slug1>)
//!         │
//!         ▼   (card terminal → claim guard lets a fresh run enqueue)
//!  Enter ─▶ Run ▾ ─▶ headless ─▶ run #2 (slug2 ≠ slug1, ainb/<slug2>) ─▶ done
//! ```
//!
//! The committing fake-agent exits on its own (non-blocking), so each run
//! finalizes to `done` and its clean worktree tears down while the durable branch
//! survives — the DISTINCT `ainb/<slug2>` branch is the proof the rerun
//! provisioned a fresh worktree, not a re-run of the first. SKIPs cleanly when
//! tmux / the binaries / the staged plugin are absent. Follows the
//! `tmux-ui-tripwire` HARD RULES: exact-name kills only, deadline-bounded polls.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_PROFILE, TuiSession, WORKTREE_REPO_NAME, board_card_by_title, budget_scale,
    can_run_tripwire, drive_card_create_to_profile, git_branch_exists,
    prepare_pipeline_worktree_pr, skip, task_short_id_by_title, worktree_branch,
};

/// The distinctive card title — greppable, never aliasing a column header.
const CARD_TITLE: &str = "Rerunlifecycletripwire";

/// Open the `Run ▾` menu over the focused card and launch headless (`Enter`
/// re-sent — a lone key can drop after a screen change).
fn launch_headless(sess: &TuiSession, scale: u64) {
    let deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut opened = false;
    while Instant::now() < deadline {
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
}

/// Poll the store until the card's latest task reaches `done`, returning its
/// short-id (the worktree slug), or `None` on timeout.
fn wait_for_done(home: &std::path::Path, title: &str, scale: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(45 * scale);
    while Instant::now() < deadline {
        if let Some((_, state)) = board_card_by_title(home, title) {
            if state.as_deref() == Some("done") {
                return task_short_id_by_title(home, title);
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

#[test]
fn rerunning_a_finished_card_enqueues_a_fresh_run_and_greens_it_again() {
    if !can_run_tripwire() {
        skip("tcp_card_rerun_e2e");
        return;
    }

    let pipe = prepare_pipeline_worktree_pr();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();
    let repo = pipe.home().join(WORKTREE_REPO_NAME);

    // Open Boards; create the card via the full F1-F4 overlay on the seeded repo.
    let boards_deadline = Instant::now() + Duration::from_secs(30 * scale);
    sess.switch_tab_until("B", boards_deadline, |c| {
        c.contains("Board: Delivery") && c.contains("Todo") && c.contains("Done")
    })
    .unwrap_or_else(|| panic!("Boards screen never rendered:\n{}", sess.capture()));
    let picker = drive_card_create_to_profile(&sess, CARD_TITLE, 1, scale);
    assert!(
        picker.contains(BOARD_RUN_PROFILE),
        "the picker must offer the seeded assignee profile:\n{picker}"
    );
    sess.send_enter();
    sess.poll_capture(Instant::now() + Duration::from_secs(20 * scale), |c| {
        c.contains(CARD_TITLE) && c.contains("Board: Delivery")
    })
    .unwrap_or_else(|| panic!("created card never rendered:\n{}", sess.capture()));

    // RUN #1: launch, wait for done, capture the first run's slug + assert its
    // durable branch exists (the committing run left commits).
    launch_headless(&sess, scale);
    let slug1 = wait_for_done(pipe.home(), CARD_TITLE, scale)
        .unwrap_or_else(|| panic!("run #1 never reached done:\n{}", sess.capture()));
    let branch1 = worktree_branch(&slug1);
    assert!(
        git_branch_exists(&repo, &branch1),
        "run #1 must leave a durable branch {branch1}"
    );

    // The finished card auto-moved into the Done column (the last column), but the
    // focus cursor stayed on the now-empty Todo column. Walk focus right — the
    // board clamps at the last column — so the Done card is focused before the
    // rerun (else Run ▾ has no card to open over).
    for _ in 0..5 {
        sess.send_key("Right");
        std::thread::sleep(Duration::from_millis(100));
    }

    // RERUN: Enter on the now-FINISHED card opens the same Run ▾ picker and
    // launches a fresh headless run. The claim guard permits it precisely because
    // run #1 is terminal (no pending task holds the per-(issue, agent) slot).
    launch_headless(&sess, scale);

    // POSITIVE (fresh run): a DISTINCT task short-id appears — the rerun enqueued
    // a new row, not a replay of run #1.
    let fresh_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut slug2 = None;
    while Instant::now() < fresh_deadline {
        if let Some(s) = task_short_id_by_title(pipe.home(), CARD_TITLE) {
            if s != slug1 {
                slug2 = Some(s);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let slug2 = slug2.unwrap_or_else(|| {
        panic!(
            "the rerun never enqueued a fresh task (still `{slug1}`):\n{}",
            sess.capture()
        )
    });

    // POSITIVE: the fresh run reaches done too, and left its OWN distinct durable
    // branch — the proof it provisioned a fresh `ainb/<slug2>` worktree.
    let done2 = wait_for_done(pipe.home(), CARD_TITLE, scale);
    let branch2 = worktree_branch(&slug2);

    let pane = sess.capture();
    drop(sess); // kill the TUI tmux session by exact name before the assertions.

    assert_ne!(
        slug1, slug2,
        "the rerun must mint a fresh task/worktree slug"
    );
    assert_eq!(
        done2.as_deref(),
        Some(slug2.as_str()),
        "the rerun must finalize to done as the fresh task\npane:\n{pane}"
    );
    assert!(
        git_branch_exists(&repo, &branch2),
        "the rerun must provision a fresh worktree leaving branch {branch2}"
    );
    let (_, state) = board_card_by_title(pipe.home(), CARD_TITLE)
        .unwrap_or_else(|| panic!("the card vanished from the db"));
    assert_eq!(
        state.as_deref(),
        Some("done"),
        "the reran card must be green again"
    );
}
