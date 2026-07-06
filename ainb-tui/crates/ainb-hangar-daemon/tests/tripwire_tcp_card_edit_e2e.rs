//! tcp T3 (agents-in-a-box-aau.3) — the card-EDIT lifecycle tripwire (F6).
//!
//! Editing a card (`e`) reuses the create overlay, PREFILLED from the card, and
//! commits at the agent stage as a `hangar/issue_update` (not a create): it
//! rewrites the issue TITLE and re-persists the card's repo + agent on the issue.
//! This tripwire proves the edit is durable AND steers the NEXT run: it creates a
//! card on `claude`, edits its title + flips the agent to `codex`, then launches a
//! run and asserts the enqueued TASK routed to `codex` (the durable "the run uses
//! the NEW agent" proof, read off the task row).
//!
//! ```text
//!  create card (claude) ─▶ e ─▶ title+EDIT ─▶ keep repo ─▶ agent→codex ─▶ save
//!         │                                                    │
//!         ▼ (issue.title rewritten, issue.agent_kind = codex)  ▼
//!  Enter ─▶ Run ▾ ─▶ headless ─▶ task enqueued with agent_kind = codex
//! ```
//!
//! The run's agent_kind is stamped in the enqueue transaction, so the assertion
//! holds regardless of whether the codex provider binary exists in the harness
//! (the proof is the routing decision, not the run's completion). SKIPs cleanly
//! when tmux / the binaries / the staged plugin are absent. Follows the
//! `tmux-ui-tripwire` HARD RULES: exact-name kills only, deadline-bounded polls.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    BOARD_RUN_PROFILE, TuiSession, budget_scale, can_run_tripwire, card_title_agent_by_title,
    drive_card_create_to_profile, prepare_pipeline_worktree_pr, skip, task_agent_kind_by_title,
};

/// The distinctive card title — greppable, never aliasing a column header.
const CARD_TITLE: &str = "Editcardlifecycle";
/// The suffix the edit appends to the title (proves the title rewrite landed).
const EDIT_MARKER: &str = "EDITED";

/// Re-send Enter until `pred` holds on the pane or `deadline` passes (a lone
/// Enter can drop after a screen change). Returns the matching capture, or `None`
/// on timeout. Stops the instant `pred` holds, so a committed overlay never eats a
/// stray Enter that would open the next affordance.
fn enter_until(
    sess: &TuiSession,
    deadline: Instant,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    loop {
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(200), &pred) {
            return Some(c);
        }
        sess.send_enter();
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), &pred) {
            return Some(c);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

/// Open the `Run ▾` menu over the focused card and launch headless (`Enter`
/// re-sent — a lone key can drop after a screen change).
fn launch_headless(sess: &TuiSession, scale: u64) {
    enter_until(
        sess,
        Instant::now() + Duration::from_secs(20 * scale),
        |c| c.contains("Run ▾"),
    )
    .unwrap_or_else(|| panic!("Run ▾ menu never opened:\n{}", sess.capture()));
    sess.send_enter(); // headless launch → hangar/board_card_run
}

#[test]
fn editing_a_card_rewrites_its_title_and_reroutes_the_next_run_to_the_new_agent() {
    if !can_run_tripwire() {
        skip("tcp_card_edit_e2e");
        return;
    }

    let pipe = prepare_pipeline_worktree_pr();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();

    // Open Boards; create the card (claude default) on the seeded scanned repo.
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
    // Commit the create: Enter on the profile picker → board_card_create. Re-send
    // until the picker CLOSES (a lone Enter can drop; and the card title also
    // appears in the picker prompt, so the pane text alone is an unreliable
    // "committed" signal — the picker-gone transition is the reliable one).
    enter_until(
        &sess,
        Instant::now() + Duration::from_secs(20 * scale),
        |c| !c.contains("Assignee profile"),
    )
    .unwrap_or_else(|| {
        panic!(
            "create never committed off the profile picker:\n{}",
            sess.capture()
        )
    });
    // Confirm the card actually landed in the db (the durable commit signal).
    let create_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let mut created_agent = None;
    while Instant::now() < create_deadline {
        if let Some((_, agent)) = card_title_agent_by_title(pipe.home(), CARD_TITLE) {
            created_agent = Some(agent);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let created_agent = created_agent
        .unwrap_or_else(|| panic!("created card missing from the db:\n{}", sess.capture()));
    assert_eq!(
        created_agent.as_deref(),
        Some("claude"),
        "the card is created on the cascade-default agent"
    );

    // EDIT: `e` opens the create overlay PREFILLED with the card's title.
    common::press_until(
        &sess,
        "e",
        Instant::now() + Duration::from_secs(20 * scale),
        |c| c.contains("Edit card title"),
    )
    .unwrap_or_else(|| panic!("edit title stage never opened:\n{}", sess.capture()));
    // Append the marker to the prefilled title (the title INPUT is not width-
    // clipped, so the marker echoes here even though the board card may truncate).
    sess.type_literal(EDIT_MARKER);
    sess.poll_capture(Instant::now() + Duration::from_secs(10 * scale), |c| {
        c.contains(EDIT_MARKER)
    })
    .unwrap_or_else(|| panic!("edited title never echoed:\n{}", sess.capture()));

    // Enter → repo stage; Enter again KEEPS the card's current repo (edit prefill)
    // and advances to the agent stage. Each transition re-sends on a dropped key.
    enter_until(
        &sess,
        Instant::now() + Duration::from_secs(15 * scale),
        |c| c.contains("Repo for"),
    )
    .unwrap_or_else(|| panic!("edit repo stage never opened:\n{}", sess.capture()));
    enter_until(
        &sess,
        Instant::now() + Duration::from_secs(15 * scale),
        |c| c.contains("Edit agent for") && c.contains("Enter save"),
    )
    .unwrap_or_else(|| panic!("edit agent stage never opened:\n{}", sess.capture()));

    // Nudge the agent claude → codex, confirming the highlight before saving (only
    // nudge while still on claude, so a re-sent Down never overshoots to copilot).
    let agent_deadline = Instant::now() + Duration::from_secs(15 * scale);
    loop {
        let c = sess.capture();
        if c.contains("▶ codex") {
            break;
        }
        assert!(
            Instant::now() < agent_deadline,
            "agent never moved to codex:\n{c}"
        );
        if c.contains("▶ claude") {
            sess.send_key("Down");
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // Enter SAVES (commits issue_update, no profile pick); re-send until the agent
    // overlay closes.
    enter_until(
        &sess,
        Instant::now() + Duration::from_secs(20 * scale),
        |c| !c.contains("Edit agent for"),
    )
    .unwrap_or_else(|| {
        panic!(
            "edit never saved (agent overlay still open):\n{}",
            sess.capture()
        )
    });

    // The edit persisted on the durable card: the title carries the marker and the
    // agent flipped to codex.
    let edited_title = format!("{CARD_TITLE}{EDIT_MARKER}");
    let edit_deadline = Instant::now() + Duration::from_secs(15 * scale);
    let mut edited = None;
    while Instant::now() < edit_deadline {
        if let Some((title, agent)) = card_title_agent_by_title(pipe.home(), &edited_title) {
            if agent.as_deref() == Some("codex") {
                edited = Some(title);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let title = edited.unwrap_or_else(|| {
        panic!(
            "the edit never persisted the new title + codex agent:\n{}",
            sess.capture()
        )
    });
    assert!(
        title.contains(EDIT_MARKER),
        "the title rewrite persisted: {title}"
    );

    // RUN: launch the edited card. The enqueued task must route to the NEW agent —
    // the durable "the run uses the edited agent" proof, read off the task row.
    launch_headless(&sess, scale);
    let run_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut task_agent = None;
    while Instant::now() < run_deadline {
        if let Some(agent) = task_agent_kind_by_title(pipe.home(), &edited_title) {
            task_agent = Some(agent);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let pane = sess.capture();
    drop(sess); // kill the TUI tmux session by exact name before the assertion.

    assert_eq!(
        task_agent.as_deref(),
        Some("codex"),
        "the run must route to the edited agent (codex), read off the task row\npane:\n{pane}"
    );
}
