//! Tripwire: cycling the preset selection (Right-arrow on the Preset row)
//! toggles a `• modified` badge. Cycling back to the autoloaded preset
//! clears it. Sibling: cycling to `Custom` always shows the badge.
//!
//! Phase 5 of `plans/new-session-redesign-spec.md`.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn modified_badge_toggles_with_preset_cycle() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-modified-{}", std::process::id());
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

    // Pre-state: Configure rendered, NO `modified` badge.
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let pre_cap = poll_capture(&session, cfg_deadline, |c| {
        c.contains("claude-interactive-yolo") && c.contains("Enter=Launch")
    });
    if pre_cap.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never opened; last:\n---\n{last}\n---");
    }
    let pre = pre_cap.unwrap();
    assert!(
        !pre.contains("modified"),
        "badge visible BEFORE Tab cycle:\n{pre}"
    );

    // Right-arrow on the Preset row → cycles to the other named preset →
    // badge appears.
    send_key(&session, "Right");
    let tab_deadline = Instant::now() + Duration::from_secs(5);
    let tab_cap = poll_capture(&session, tab_deadline, |c| {
        c.contains("modified") && c.contains("Enter=Launch")
    });
    if tab_cap.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("modified badge did not appear after Right-arrow; last:\n---\n{last}\n---");
    }

    // Left-arrow back → badge clears.
    send_key(&session, "Left");
    let back_deadline = Instant::now() + Duration::from_secs(5);
    let back_cap = poll_capture(&session, back_deadline, |c| {
        c.contains("claude-interactive-yolo")
            && c.contains("Enter=Launch")
            && !c.contains("modified")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        back_cap.is_some(),
        "modified badge did not clear after Left-arrow. Final:\n---\n{final_cap}\n---"
    );
}

/// Sibling: cycling to the `Custom` preset slot fires the `• modified`
/// badge even without further edits, and flipping Mode → Boss while on
/// Custom keeps the badge lit.
#[test]
fn modified_badge_toggles_with_mode_toggle_alone() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-mod-toggle-{}", std::process::id());
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
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let pre_cap = poll_capture(&session, cfg_deadline, |c| {
        c.contains("claude-interactive-yolo") && c.contains("Enter=Launch")
    });
    if pre_cap.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never opened; last:\n---\n{last}\n---");
    }
    let pre = pre_cap.unwrap();
    assert!(
        !pre.contains("modified"),
        "badge visible BEFORE mode toggle:\n{pre}"
    );

    // Right-arrow twice on Preset row to reach Custom; Tab to Mode row;
    // Right-arrow to flip Interactive → Boss. Badge must remain lit.
    // Custom seeds from codex-interactive-yolo (agent="codex"); 2026-05 refresh
    // shows Model row for Codex too, so 3 Tabs reach Mode.
    send_key(&session, "Right");
    send_key(&session, "Right");
    send_key(&session, "Tab"); // Preset → Agent
    send_key(&session, "Tab"); // Agent → Model
    send_key(&session, "Tab"); // Model → Mode
    send_key(&session, "Right"); // Interactive → Boss
    let toggled_deadline = Instant::now() + Duration::from_secs(5);
    let toggled = poll_capture(&session, toggled_deadline, |c| {
        c.contains("modified") && c.contains("Boss")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        toggled.is_some(),
        "Custom + Mode→Boss did not produce `modified` + Boss state. Final:\n---\n{final_cap}\n---"
    );
}
