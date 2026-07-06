//! tcp T4 (agents-in-a-box-aau.4) — the F7 SQUAD-CARD fan-out tripwire.
//!
//! A card can be assigned a whole SQUAD, not just a single agent. This tripwire
//! proves the acceptance end-to-end against the REAL daemon binary + claim loop:
//! a card assigned to a squad (leader + two members) is RUN via
//! `hangar/board_card_run`, which FANS OUT into a leader brief + one task per
//! member — and each of the THREE tasks provisions its OWN volatile worktree from
//! the card's repo, live at once.
//!
//! ```text
//!  board_card_run(squad card) ──▶ run_card ──▶ assign_fanout
//!         │                                        │
//!         ▼                                        ▼
//!   leader task (agent-1)   member task (agent-m1)   member task (agent-m2)
//!         │                        │                        │
//!         ▼                        ▼                        ▼
//!   worktrees/<lead>         worktrees/<m1>           worktrees/<m2>
//!    on ainb/<lead>           on ainb/<m1>             on ainb/<m2>   ── all live at once
//!         │  ── release the blocked agents ──▶ all three finalize + tear down ──
//! ```
//!
//! Drives the daemon directly (a framed socket RPC) rather than the TUI: the
//! card→overlay path is covered by the T1/T3 tripwires; the fan-out + per-member
//! worktree isolation is a pure run-handler + claim-loop behaviour. The three
//! member agents share `runtime-1` (distinct agents), so the single fixture daemon
//! claims all three concurrently under their per-agent caps. SKIPs cleanly when the
//! daemon binary / git are absent. Exact-name kills only (the `Pipeline` owns its
//! one daemon child); deadline-bounded polls; POSITIVE (three live worktrees +
//! member chips) + NEGATIVE (all torn down) proofs.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_BOARD, DaemonRpc, INTERACTIVE_RELEASE_SENTINEL, T4_SQUAD_CARD_ISSUE,
    WORKTREE_REPO_NAME, budget_scale, daemon_bin, git_available, git_branch_exists,
    prepare_pipeline_squad_card, skip, task_count_for_issue, task_short_id, task_status_by_id,
    worktree_branch, worktree_dir,
};

#[test]
fn squad_card_run_fans_out_to_own_worktrees() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_squad_card_fanout_e2e");
        return;
    }

    let pipe = prepare_pipeline_squad_card();
    let scale = budget_scale();
    let repo = pipe.home().join(WORKTREE_REPO_NAME);
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    // POSITIVE (fan-out): running the squad card enqueues a LEADER task + one task
    // per member — the result carries the leader `task_id` + the member ids.
    let run = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN,
        serde_json::json!({
            "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
            "board_id": BOARD_RUN_BOARD,
            "issue_id": T4_SQUAD_CARD_ISSUE,
            "mode": "headless",
        }),
    );
    assert!(
        run["error"].is_null(),
        "squad card run must ack, got: {run}"
    );
    let leader_task = run["result"]["task_id"].as_str().unwrap_or("").to_string();
    assert!(
        !leader_task.is_empty(),
        "run result must carry a leader task id: {run}"
    );
    let member_tasks: Vec<String> = run["result"]["member_task_ids"]
        .as_array()
        .unwrap_or_else(|| panic!("run result must carry member_task_ids: {run}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        member_tasks.len(),
        2,
        "the fan-out must brief the leader + two member tasks: {run}"
    );

    // The three tasks resolve to THREE DISTINCT worktree dirs + branches (the
    // a54 per-task-slug uniqueness — no member collides with another).
    let mut all_task_ids = vec![leader_task.clone()];
    all_task_ids.extend(member_tasks.clone());
    let slugs: Vec<String> = all_task_ids.iter().map(|t| task_short_id(t).to_string()).collect();
    let dirs: Vec<_> = slugs.iter().map(|s| worktree_dir(pipe.home(), s)).collect();
    let distinct: HashSet<&std::path::PathBuf> = dirs.iter().collect();
    assert_eq!(
        distinct.len(),
        3,
        "the leader + two members must map to three distinct worktree dirs"
    );

    // POSITIVE (own worktrees): all THREE worktrees exist AT ONCE (every agent
    // blocked on the release sentinel), each on its own ainb/<slug> branch.
    let live_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_live = poll_until(live_deadline, || {
        dirs.iter()
            .zip(&slugs)
            .all(|(dir, slug)| is_worktree_live(dir, &repo, &worktree_branch(slug)))
    });
    assert!(
        all_live,
        "all three fanned runs must provision distinct live worktrees ({dirs:?})"
    );

    // POSITIVE (member chips): the board card renders one chip per member task.
    let list = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARDS_LIST,
        serde_json::json!({ "workspace_id": ainb_hangar_daemon::seed::WS_SLUG }),
    );
    let member_states = find_card_member_states(&list, T4_SQUAD_CARD_ISSUE)
        .unwrap_or_else(|| panic!("the squad card must render member chips: {list}"));
    assert!(
        member_states.len() >= 2,
        "a squad card must show a chip per member task (got {}): {list}",
        member_states.len()
    );

    // Release the blocked agents → all three finalize → all three CLEAN worktrees
    // torn down (F5 teardown holds for every fanned run).
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go")
        .expect("write release sentinel");

    let teardown_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_gone = poll_until(teardown_deadline, || dirs.iter().all(|d| !d.exists()));

    // Kill the daemon by its exact child handle before the final assert.
    drop(rpc);
    drop(pipe);

    assert!(
        all_gone,
        "every fanned worktree must be torn down after its run ({dirs:?})"
    );
}

/// tcp T4 / FANOUT-SEMANTICS — cancelling a SQUAD card cancels EVERY member.
///
/// The fan-out creates N concurrent active tasks on one issue, but the old cancel
/// path resolved a single task (LIMIT 1) and cancelled only the newest sibling — the
/// leader + the other members kept burning tokens and later re-moved the "cancelled"
/// card. This proves the fix end-to-end against the REAL daemon: with all three
/// fanned runs LIVE, one `board_card_cancel` drives ALL THREE task rows to
/// `cancelled` and tears down ALL THREE worktrees — WITHOUT releasing the sentinel
/// (the cancel is what stops the agents).
#[test]
fn cancelling_a_squad_card_cancels_every_member() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_squad_card_cancel_e2e");
        return;
    }

    let pipe = prepare_pipeline_squad_card();
    let scale = budget_scale();
    let repo = pipe.home().join(WORKTREE_REPO_NAME);
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    // Fan the squad card out (headless) and collect all three task ids.
    let run = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN,
        serde_json::json!({
            "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
            "board_id": BOARD_RUN_BOARD,
            "issue_id": T4_SQUAD_CARD_ISSUE,
            "mode": "headless",
        }),
    );
    assert!(
        run["error"].is_null(),
        "squad card run must ack, got: {run}"
    );
    let mut all_task_ids = vec![run["result"]["task_id"].as_str().unwrap_or("").to_string()];
    all_task_ids.extend(
        run["result"]["member_task_ids"]
            .as_array()
            .unwrap_or_else(|| panic!("run must carry member_task_ids: {run}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string()),
    );
    assert_eq!(all_task_ids.len(), 3, "leader + two members: {run}");

    // Wait until all three runs are LIVE (their worktrees exist) — so the cancel
    // stops three genuinely-running siblings, not a half-started fan-out.
    let slugs: Vec<String> = all_task_ids.iter().map(|t| task_short_id(t).to_string()).collect();
    let dirs: Vec<_> = slugs.iter().map(|s| worktree_dir(pipe.home(), s)).collect();
    let live_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_live = poll_until(live_deadline, || {
        dirs.iter()
            .zip(&slugs)
            .all(|(dir, slug)| is_worktree_live(dir, &repo, &worktree_branch(slug)))
    });
    assert!(
        all_live,
        "all three fanned runs must be live before the cancel ({dirs:?})"
    );

    // ONE cancel of the card — never releasing the sentinel.
    let cancel = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARD_CARD_CANCEL,
        serde_json::json!({
            "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
            "board_id": BOARD_RUN_BOARD,
            "issue_id": T4_SQUAD_CARD_ISSUE,
        }),
    );
    assert!(cancel["error"].is_null(), "squad cancel must ack: {cancel}");
    assert_eq!(
        cancel["result"]["cancelled"], true,
        "the squad card must report cancelled: {cancel}"
    );

    // EVERY member task must reach `cancelled` — not just the newest sibling.
    let cancel_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_cancelled = poll_until(cancel_deadline, || {
        all_task_ids
            .iter()
            .all(|id| task_status_by_id(pipe.home(), id).as_deref() == Some("cancelled"))
    });

    // And every worktree must be torn down (each cancelled run reclaims its own).
    let teardown_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_gone = poll_until(teardown_deadline, || dirs.iter().all(|d| !d.exists()));

    // Snapshot the per-task states for a precise failure message before teardown.
    let states: Vec<Option<String>> =
        all_task_ids.iter().map(|id| task_status_by_id(pipe.home(), id)).collect();

    drop(rpc);
    drop(pipe);

    assert!(
        all_cancelled,
        "every fanned member task must be cancelled, got {states:?}"
    );
    assert!(
        all_gone,
        "every fanned worktree must be torn down after the cancel ({dirs:?})"
    );
}

/// tcp T4 / FANOUT-SEMANTICS — a squad card REJECTS `interactive` mode loudly.
///
/// A squad runs as a HEADLESS batch (the leader coordinates the members), so
/// `interactive` has no coherent meaning across the fan-out. The old handler silently
/// discarded the requested mode yet echoed it back in the reply — a lie. This proves
/// the fix: running a squad card with `mode: interactive` is refused with a clear
/// error, and nothing is dispatched.
#[test]
fn running_a_squad_card_interactive_is_rejected() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_squad_card_interactive_reject_e2e");
        return;
    }

    let pipe = prepare_pipeline_squad_card();
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    let run = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARD_CARD_RUN,
        serde_json::json!({
            "workspace_id": ainb_hangar_daemon::seed::WS_SLUG,
            "board_id": BOARD_RUN_BOARD,
            "issue_id": T4_SQUAD_CARD_ISSUE,
            "mode": "interactive",
        }),
    );
    let err = run["error"]["message"].as_str().unwrap_or("").to_string();
    // NEGATIVE: the refused run fanned nothing out.
    let dispatched = task_count_for_issue(pipe.home(), T4_SQUAD_CARD_ISSUE);

    drop(rpc);
    drop(pipe);

    assert!(
        !run["error"].is_null(),
        "an interactive squad run must be refused: {run}"
    );
    assert!(
        err.contains("interactive") && err.contains("squad"),
        "the refusal must name interactive + squad ({err:?})"
    );
    assert_eq!(
        dispatched, 0,
        "a refused interactive squad run must not dispatch any task"
    );
}

/// Whether the worktree at `dir` exists AND `branch` is registered in `repo`.
fn is_worktree_live(dir: &Path, repo: &Path, branch: &str) -> bool {
    dir.join(".git").exists() && git_branch_exists(repo, branch)
}

/// The `member_states` array of the card `issue_id` in a `boards_list` result,
/// searched across every column + the unmapped pool.
fn find_card_member_states(
    list: &serde_json::Value,
    issue_id: &str,
) -> Option<Vec<serde_json::Value>> {
    let boards = list["result"]["boards"].as_array()?;
    for b in boards {
        let mut buckets: Vec<&serde_json::Value> = Vec::new();
        if let Some(cols) = b["columns"].as_array() {
            for c in cols {
                if let Some(cards) = c["cards"].as_array() {
                    buckets.extend(cards);
                }
            }
        }
        if let Some(un) = b["unmapped"].as_array() {
            buckets.extend(un);
        }
        for card in buckets {
            if card["issue_id"] == issue_id {
                return Some(card["member_states"].as_array().cloned().unwrap_or_default());
            }
        }
    }
    None
}

/// Poll `pred` every 150ms until it holds or `deadline` passes.
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
