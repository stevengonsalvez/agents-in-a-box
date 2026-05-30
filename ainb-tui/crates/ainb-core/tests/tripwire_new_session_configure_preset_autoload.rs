//! Tripwire: when entering Configure for a repo whose
//! `session-defaults.yaml.per_repo[repo].last_preset` is set, that preset is
//! auto-selected in the preset dropdown.
//!
//! Forward-only is OK here — Esc-back is exercised by `tripwire_new_session_esc_back`.
//! Phase 5 of `plans/new-session-redesign-spec.md`.


#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn configure_autoloads_last_preset_from_session_defaults() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-autoload-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args([
            "new-session", "-d", "-s", &session, "-x", "180", "-y", "50",
        ])
        .status()
        .expect("tmux new-session");
    assert!(status.success());

    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_tmp.path().display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux launch");

    // Wait for home.
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

    // Open PickRepo.
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(5);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("New Session") && c.contains("Enter=Select")
    });
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        on_pick = poll_capture(&session, retry_deadline, |c| {
            c.contains("New Session") && c.contains("Enter=Select")
        });
    }
    if on_pick.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo never opened; last:\n---\n{last}\n---");
    }

    // Enter on the highlighted (autoloaded) favorite ainb-tui → Configure.
    send_key(&session, "Enter");

    // Configure has its own title `ainb-tui → new session` and shows the
    // `claude-interactive-yolo` preset name. Negative: PickRepo help bar is gone.
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let on_cfg = poll_capture(&session, cfg_deadline, |c| {
        c.contains("new session")
            && c.contains("claude-interactive-yolo")
            && c.contains("Enter=Launch")
            && !c.contains("Enter=Select")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    let Some(cap) = on_cfg else {
        panic!(
            "Configure did not autoload claude-interactive-yolo within 10s. \
             Final capture:\n---\n{final_cap}\n---"
        );
    };
    // Positive: autoloaded preset name in the pane.
    assert!(
        cap.contains("claude-interactive-yolo"),
        "Configure missing `claude-interactive-yolo`:\n{cap}"
    );
    // Negative: PickRepo chrome must be gone.
    assert!(
        !cap.contains("^R=Reset"),
        "PickRepo chrome leaking into Configure:\n{cap}"
    );
}
