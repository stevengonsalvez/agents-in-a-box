//! Tripwire: branch worktree line is auto-derived as `agents/<8-hex-rand>`,
//! independent of any prompt text typed into the Boss-mode textarea.
//!
//! Stevie 2026-05-27: the slug-from-prompt behavior was ripped out — branch
//! names are now random and STABLE for the lifetime of one Configure session,
//! so the displayed name doesn't jitter on every prompt keystroke.
//!
//! Asserts:
//!   1. Branch line shows `agents/<8-hex>` immediately on Configure open.
//!   2. After selecting the `opusplan` Boss preset and typing a multi-word
//!      prompt, the branch suffix is UNCHANGED (no slug-from-prompt regression).
//!
//! Phase 5 of `plans/new-session-redesign-spec.md` (revised).

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use regex::Regex;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn branch_line_is_random_hex_and_stable_across_prompt_edits() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-branch-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success());

    let cmd = launch_cmd_gh_authed(home_tmp.path(), &ainb);
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux launch");

    let home_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }

    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(5);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| c.contains("Enter=Select"));
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        on_pick = poll_capture(&session, retry_deadline, |c| c.contains("Enter=Select"));
    }
    if on_pick.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo never opened; last:\n---\n{last}\n---");
    }

    send_key(&session, "Enter");

    // Step 1: branch line shows `agents/<8-hex>` immediately on open.
    let hex_re = Regex::new(r"agents/[0-9a-f]{8}\b").expect("regex");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let initial_cap = poll_capture(&session, cfg_deadline, |c| {
        c.contains("Branch:") && hex_re.is_match(c)
    });
    let initial_cap = match initial_cap {
        Some(c) => c,
        None => {
            let last = capture(&session);
            kill_session(&session);
            panic!("Branch line never showed agents/<8-hex>; last:\n---\n{last}\n---");
        }
    };
    let initial_branch = hex_re
        .find(&initial_cap)
        .expect("regex matched in poll predicate")
        .as_str()
        .to_string();

    // Step 2: select the `opusplan` Boss preset + type a prompt. Branch must
    // stay EQUAL to the initial random suffix (no slug-from-prompt regression).
    //
    // The Boss/Mode toggle was removed (commit fabdd92a) — Mode now hard-locks
    // to Interactive on the Custom path. A Boss-mode configuration (which
    // reveals the Prompt textarea) is still reachable by *selecting* the shipped
    // `opusplan` preset, whose `mode = "boss"`. Presets are sorted by name, so
    // from the claude-interactive-yolo default (Named(0)) two Right presses
    // land on `opusplan` (Named(2)). Focus stays on the Preset row.
    send_key(&session, "Right"); // Named(0) → Named(1) codex-interactive-yolo
    send_key(&session, "Right"); // Named(1) → Named(2) opusplan (Boss preset)
    let boss_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, boss_deadline, |c| {
        c.contains("[opusplan]") && c.contains("Prompt:")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Selecting opusplan did not reveal Prompt textarea; last:\n---\n{last}\n---");
    }
    // Prompt is always the second-to-last row (Launch is last). Two Shift+Tabs
    // from the Preset row wrap backwards onto it, independent of how many middle
    // rows (Mode/Yolo/Headroom/RTK) the preset exposes.
    send_key(&session, "BTab"); // Preset → Launch (wrap to last)
    send_key(&session, "BTab"); // Launch → Prompt (second-to-last)
    send_text(&session, "fix the bug now");

    // Settle, then capture and assert.
    std::thread::sleep(Duration::from_millis(800));
    let final_cap = capture(&session);
    kill_session(&session);

    // The branch line must STILL contain the original 8-hex suffix.
    assert!(
        final_cap.contains(&initial_branch),
        "branch suffix changed after prompt edits — expected `{initial_branch}` \
         still present.\nFinal capture:\n---\n{final_cap}\n---"
    );

    // Negative: must NOT contain a slug derived from the prompt words.
    assert!(
        !final_cap.contains("agents/fix-the-bug"),
        "prompt-slug branch (`agents/fix-the-bug`) leaked into the branch line — \
         the prompt-derives-branch behavior must stay removed.\nFinal capture:\n---\n{final_cap}\n---"
    );
}
