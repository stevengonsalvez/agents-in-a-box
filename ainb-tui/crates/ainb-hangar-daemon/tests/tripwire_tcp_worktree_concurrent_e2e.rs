//! tcp T1 (agents-in-a-box-aau.1) — the F5 CONCURRENT same-repo worktree tripwire.
//!
//! The lifecycle tripwire (`tripwire_tcp_worktree_lifecycle_e2e`) proves ONE card
//! run provisions + tears down a worktree. This one proves the a54 "N tasks on one
//! repo never collide" guarantee (spec F5): TWO tasks bound to the SAME repo, run
//! concurrently by the claim loop, each provision their OWN distinct volatile
//! worktree on their OWN `ainb/<slug>` branch — no collision, no shared checkout.
//!
//! ```text
//!  enqueue task wt-a (repo=R) ─┐
//!  enqueue task wt-b (repo=R) ─┴▶ claim loop (cap 2) runs BOTH concurrently
//!                    │
//!                    ▼
//!   worktrees/wt-a  on ainb/wt-a   ┐  two DISTINCT dirs + branches, live at once
//!   worktrees/wt-b  on ainb/wt-b   ┘  (the blocking agents hold both runs)
//!                    │  ── release both ──▶ both finalize ──▶ both torn down ──
//! ```
//!
//! This drives the daemon directly (two enqueued tasks) rather than the TUI twice:
//! the card→task create path is covered by the lifecycle tripwire, and the
//! concurrency guarantee is a pure claim-loop + provisioning behaviour. The
//! blocking fake-claude holds both runs so the "two live worktrees" assertion is
//! race-free. SKIPs cleanly when git / the daemon binary are absent. Follows the
//! `tmux-ui-tripwire` HARD RULES: exact-name kills only (the daemon owns none
//! here), deadline-bounded polls, POSITIVE (two live worktrees) + NEGATIVE (no
//! collision / both torn down) proofs.

use std::path::Path;
use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    INTERACTIVE_RELEASE_SENTINEL, WORKTREE_REPO_NAME, budget_scale, daemon_bin,
    enqueue_task_with_repo, git_available, git_branch_exists, prepare_pipeline_worktree_concurrent,
    skip, worktree_branch, worktree_dir,
};

#[test]
fn two_cards_on_the_same_repo_run_in_two_distinct_worktrees() {
    // This tripwire needs only the daemon + git (no TUI), so it gates on those
    // rather than the full tmux/plugin pipeline.
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_worktree_concurrent_e2e");
        return;
    }

    let pipe = prepare_pipeline_worktree_concurrent();
    let scale = budget_scale();
    let repo = pipe.home().join(WORKTREE_REPO_NAME);
    let repo_ref = repo.to_str().expect("utf-8 repo path");

    // Enqueue TWO tasks on the SAME repo (no issue, so the per-issue pending index
    // never blocks the second). The claim loop (cap 2) runs both concurrently.
    let id_a = enqueue_task_with_repo(pipe.home(), "a", repo_ref);
    let id_b = enqueue_task_with_repo(pipe.home(), "b", repo_ref);
    let slug_a = common::task_short_id(&id_a).to_string();
    let slug_b = common::task_short_id(&id_b).to_string();
    let wt_a = worktree_dir(pipe.home(), &slug_a);
    let wt_b = worktree_dir(pipe.home(), &slug_b);

    // NEGATIVE (no collision): the two tasks resolve to DISTINCT worktree dirs +
    // branches before either runs — the a54 uniqueness comes from the per-task slug.
    assert_ne!(
        wt_a, wt_b,
        "the two same-repo tasks must map to distinct worktree dirs"
    );
    assert_ne!(
        worktree_branch(&slug_a),
        worktree_branch(&slug_b),
        "the two tasks must branch onto distinct ainb/<slug> refs"
    );

    // POSITIVE (F5 concurrency): BOTH worktrees exist AT ONCE (both agents blocked),
    // each on its own ainb/<slug> branch in the shared origin repo.
    let live_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let both_live = poll_until(live_deadline, || {
        is_worktree_live(&wt_a, &repo, &worktree_branch(&slug_a))
            && is_worktree_live(&wt_b, &repo, &worktree_branch(&slug_b))
    });
    assert!(
        both_live,
        "both same-repo runs must provision distinct live worktrees ({} + {})",
        wt_a.display(),
        wt_b.display()
    );

    // Release both blocked agents → both finalize → both CLEAN worktrees torn down.
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go")
        .expect("write release sentinel");

    // NEGATIVE (F5 teardown): neither worktree dir survives the finished runs.
    let teardown_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let both_gone = poll_until(teardown_deadline, || !wt_a.exists() && !wt_b.exists());
    assert!(
        both_gone,
        "both clean worktrees must be torn down after the runs ({} + {})",
        wt_a.display(),
        wt_b.display()
    );
}

/// Whether the worktree checkout at `dir` exists AND `branch` is registered in the
/// origin `repo` — the two-part proof a run provisioned a real worktree.
fn is_worktree_live(dir: &Path, repo: &Path, branch: &str) -> bool {
    dir.join(".git").exists() && git_branch_exists(repo, branch)
}

/// Poll `pred` every 150ms until it holds or `deadline` passes. Returns whether it
/// held (no bare sleep before a check — deadline-bounded, per the skill rules).
fn poll_until(deadline: Instant, pred: impl Fn() -> bool) -> bool {
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
