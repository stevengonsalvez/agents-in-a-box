//! tcp T4 (agents-in-a-box-aau.4): the F7 SQUAD-CARD single-owner tripwire.
//!
//! A card can be assigned a whole SQUAD, not just a single agent. This tripwire
//! proves the acceptance end-to-end against the REAL daemon binary + claim loop:
//! a card assigned to a squad (leader + two members) is RUN via
//! `hangar/board_card_run`, which dispatches the work to exactly ONE owner,
//! never one task per member.
//!
//! This test previously asserted the OPPOSITE: that a squad-card run FANNED OUT
//! into a leader task plus one task per member, with all three live at once in
//! three separate worktrees on three separate `ainb/<slug>` branches. That was
//! the defect being tested, not the feature: three agents racing the SAME issue
//! meant three worktrees, three branches, and three independently-committed
//! answers to one card, with nothing reconciling them. `SquadAssignService::
//! assign_fanout` now enqueues the card into the first role-gated board column
//! (or, when the workspace has no such pipeline, briefs the squad LEADER
//! directly), and exactly one eligible agent takes it. `SquadFanout.members` is
//! now ALWAYS EMPTY, except for the explicit `--redundant N` opt-in
//! (`assign_redundant`), a different function entirely. This tripwire is
//! inverted to match: it proves ONE owner, ONE worktree, ONE branch, per card.
//!
//! ```text
//!  board_card_run(squad card) ──▶ run_card ──▶ assign_fanout
//!         │                                        │
//!         ▼                                        ▼
//!                                    ONE owner task (the leader, or
//!                                    the single agent a pipeline pulled)
//!                                             │
//!                                             ▼
//!                                      worktrees/<owner>
//!                                       on ainb/<owner>
//!                                             │
//!             ── release the blocked agent ──▶ finalizes, tears down ──
//! ```
//!
//! Drives the daemon directly (a framed socket RPC) rather than the TUI: the
//! card-to-overlay path is covered by the T1/T3 tripwires; the single-owner
//! dispatch plus worktree lifecycle is a pure run-handler / claim-loop
//! behaviour. SKIPs cleanly when the daemon binary or git are absent. Exact-name
//! kills only (the `Pipeline` owns its one daemon child); deadline-bounded
//! polls; POSITIVE (one live worktree, one owner chip) and NEGATIVE (torn down,
//! no member tasks) proofs.

use std::path::Path;
use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_BOARD, DaemonRpc, INTERACTIVE_RELEASE_SENTINEL, T4_SQUAD_CARD_ISSUE,
    T4_SQUAD_INSTRUCTIONS, T4_SQUAD_M1_ROLE, T4_SQUAD_M1_SKILL, WORKTREE_REPO_NAME, budget_scale,
    daemon_bin, git_available, git_branch_exists, materialised_context_prompt,
    prepare_pipeline_squad_card, skip, task_count_for_issue, task_short_id, task_status_by_id,
    worktree_branch, worktree_dir,
};

/// tcp T4 / FANOUT-SEMANTICS: running a squad card dispatches to exactly ONE
/// owner, never one task per member.
///
/// Inverted from the removed broadcast defect: this test used to prove three
/// tasks (leader + two members) landed in three distinct worktrees, all live at
/// once. Under pull that shape is gone (`assign_fanout` writes ONE task and
/// `SquadFanout.members` is always empty), so the proof is now that a squad
/// dispatch behaves exactly like a single-agent one: one task, one worktree, one
/// branch, one board chip.
#[test]
fn squad_card_run_dispatches_to_exactly_one_owner() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_squad_card_single_owner_e2e");
        return;
    }

    let pipe = prepare_pipeline_squad_card();
    let scale = budget_scale();
    let repo = pipe.home().join(WORKTREE_REPO_NAME);
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

    // POSITIVE (single owner): running the squad card enqueues EXACTLY ONE task.
    // `member_task_ids` is `#[serde(skip_serializing_if = "Vec::is_empty")]` on
    // the wire, so under pull it is ABSENT from the reply entirely, never `[]`.
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
    let owner_task = run["result"]["task_id"].as_str().unwrap_or("").to_string();
    assert!(
        !owner_task.is_empty(),
        "run result must carry the single owner's task id: {run}"
    );
    assert!(
        run["result"]["member_task_ids"].as_array().is_none_or(std::vec::Vec::is_empty),
        "a squad dispatch under pull must carry no member tasks: {run}"
    );
    assert_eq!(
        task_count_for_issue(pipe.home(), T4_SQUAD_CARD_ISSUE),
        1,
        "a squad card run must enqueue exactly ONE task, never one per member"
    );

    // POSITIVE (one worktree, one branch): the owner's task resolves to exactly
    // ONE live worktree dir on its own `ainb/<slug>` branch, and the shared
    // worktrees root holds no other entry (never three live at once).
    let slug = task_short_id(&owner_task);
    let dir = worktree_dir(pipe.home(), &slug);
    let branch = worktree_branch(&slug);
    let live_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let is_live = poll_until(live_deadline, || is_worktree_live(&dir, &repo, &branch));
    assert!(
        is_live,
        "the single owner's run must provision its worktree ({dir:?})"
    );
    let worktrees_root = pipe.home().join(".agents-in-a-box").join("worktrees");
    let live_count = std::fs::read_dir(&worktrees_root).map_or(0, std::iter::Iterator::count);
    assert_eq!(
        live_count, 1,
        "exactly one worktree dir must exist under {worktrees_root:?}, never three"
    );

    // POSITIVE (one owner chip): the board card renders exactly one member chip
    // (the pulled owner), never one chip per squad member.
    let list = rpc.call(
        ainb_hangar_proto::methods::HANGAR_BOARDS_LIST,
        serde_json::json!({ "workspace_id": ainb_hangar_daemon::seed::WS_SLUG }),
    );
    let member_states = find_card_member_states(&list, T4_SQUAD_CARD_ISSUE)
        .unwrap_or_else(|| panic!("the squad card must render its owner chip: {list}"));
    assert_eq!(
        member_states.len(),
        1,
        "a squad card must show exactly one chip (the pulled owner), not one per squad member: {list}"
    );

    // NEGATIVE (release, finalize, teardown): releasing the blocked agent lets
    // the single run finalize, tearing its worktree down cleanly.
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go")
        .expect("write release sentinel");

    let teardown_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let gone = poll_until(teardown_deadline, || !dir.exists());

    // Kill the daemon by its exact child handle before the final assert.
    drop(rpc);
    drop(pipe);

    assert!(
        gone,
        "the owner's worktree must be torn down after its run ({dir:?})"
    );
}

/// tcp T4: cancelling a SQUAD card cancels its run and reclaims its worktree,
/// WITHOUT releasing the sentinel (the cancel is what stops the agent).
///
/// # Scope narrowed with the broadcast
///
/// This used to build THREE live sibling runs out of the squad fan-out, to prove
/// the cancel path no longer resolved a single task (`LIMIT 1`) and left the other
/// siblings burning tokens. A squad card now carries ONE run, so that setup is no
/// longer reachable here.
///
/// The cancel-every-sibling property is NOT dropped: it is re-homed onto the shape
/// that can still produce several concurrent runs on one card, in
/// `rpc_issue_cancel_active.rs`
/// (`cancel_active_cancels_every_sibling_not_just_the_newest`). What stays here is
/// the genuinely end-to-end half: a real daemon, a real worktree, one cancel, and
/// the worktree reclaimed.
#[test]
fn cancelling_a_squad_card_cancels_its_run() {
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
    let all_task_ids = [run["result"]["task_id"].as_str().unwrap_or("").to_string()];
    assert!(
        run["result"]["member_task_ids"].as_array().is_none_or(std::vec::Vec::is_empty),
        "a squad card must not fan out to one task per member: {run}"
    );
    assert_eq!(all_task_ids.len(), 1, "exactly one owner: {run}");

    // Wait until the run is LIVE (its worktree exists), so the cancel stops a
    // genuinely-running agent rather than a half-started dispatch.
    let slugs: Vec<String> = all_task_ids.iter().map(|t| task_short_id(t)).collect();
    let dirs: Vec<_> = slugs.iter().map(|s| worktree_dir(pipe.home(), s)).collect();
    let live_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_live = poll_until(live_deadline, || {
        dirs.iter()
            .zip(&slugs)
            .all(|(dir, slug)| is_worktree_live(dir, &repo, &worktree_branch(slug)))
    });
    assert!(
        all_live,
        "the run must be live before the cancel ({dirs:?})"
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

    // The task must reach `cancelled`.
    let cancel_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_cancelled = poll_until(cancel_deadline, || {
        all_task_ids
            .iter()
            .all(|id| task_status_by_id(pipe.home(), id).as_deref() == Some("cancelled"))
    });

    // And its worktree must be torn down (a cancelled run reclaims its own).
    let teardown_deadline = Instant::now() + Duration::from_secs(45 * scale);
    let all_gone = poll_until(teardown_deadline, || dirs.iter().all(|d| !d.exists()));

    // Snapshot the per-task states for a precise failure message before teardown.
    let states: Vec<Option<String>> =
        all_task_ids.iter().map(|id| task_status_by_id(pipe.home(), id)).collect();

    drop(rpc);
    drop(pipe);

    assert!(
        all_cancelled,
        "the card's run must be cancelled, got {states:?}"
    );
    assert!(
        all_gone,
        "the run's worktree must be torn down after the cancel ({dirs:?})"
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

/// tcp T4 / parity #25 + `7-rest`: the LEADER of a squad-card run actually
/// RECEIVES the briefing: protocol + roster (with each member's role and the
/// skills it will materialise) + the squad's instructions.
///
/// The strongest available proof: the REAL daemon binary runs the real dispatch
/// and materialises a real `CLAUDE.md`, which this reads off disk. The read
/// happens INSIDE the window where the blocking fake agent still holds the run
/// (the task tree is reclaimed after finalize, so reading later would race
/// teardown). Under pull the dispatch writes no member tasks
/// (`SquadFanout.members` is always empty), so this no longer collects member
/// task ids, and the old "a member run is never briefed" negative is deleted
/// rather than kept as a loop over a list that is always empty.
#[test]
fn squad_card_leader_run_materialises_the_briefing_with_roles_and_skills() {
    if daemon_bin().is_none() || !git_available() {
        skip("tcp_squad_card_leader_briefing_e2e");
        return;
    }

    let pipe = prepare_pipeline_squad_card();
    let scale = budget_scale();
    let mut rpc = DaemonRpc::connect_and_auth(pipe.home());

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
    assert!(
        run["result"]["member_task_ids"].as_array().is_none_or(std::vec::Vec::is_empty),
        "a squad dispatch under pull must carry no member tasks: {run}"
    );

    // Poll for the leader's materialised prompt while the run is still held.
    let deadline = Instant::now() + Duration::from_secs(45 * scale);
    let mut leader_prompt = String::new();
    loop {
        if let Some(text) = materialised_context_prompt(pipe.home(), &leader_task) {
            if text.contains("## Squad Roster") {
                leader_prompt = text;
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    // Release + tear down BEFORE asserting so a failure never leaks the daemon.
    std::fs::write(pipe.home().join(INTERACTIVE_RELEASE_SENTINEL), "go")
        .expect("write release sentinel");
    drop(rpc);
    drop(pipe);

    assert!(
        !leader_prompt.is_empty(),
        "the real daemon must materialise a CLAUDE.md carrying the squad roster \
         for the leader task {leader_task}"
    );
    assert!(
        leader_prompt.contains("## Squad Operating Protocol"),
        "the injected prompt must carry the operating protocol:\n{leader_prompt}"
    );
    // The roled + skilled member's WHOLE row — role THEN skills, never a bare
    // substring, so a half-rendered row cannot pass.
    assert!(
        leader_prompt.contains(&format!(
            "- member-one — agent — agent-m1 — role: {T4_SQUAD_M1_ROLE} — \
             skills: {T4_SQUAD_M1_SKILL}\n"
        )),
        "the injected roster row must carry the member's role and skills:\n{leader_prompt}"
    );
    // The bare member pins the blank-omit: identity only, no empty fragments.
    assert!(
        leader_prompt.contains("- member-two — agent — agent-m2\n"),
        "a member with neither role nor skills must render identity only:\n{leader_prompt}"
    );
    assert!(
        leader_prompt.contains("## Squad Instructions"),
        "the injected prompt must carry the instructions section:\n{leader_prompt}"
    );
    assert!(
        leader_prompt.contains(T4_SQUAD_INSTRUCTIONS),
        "the instructions must be injected VERBATIM:\n{leader_prompt}"
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
