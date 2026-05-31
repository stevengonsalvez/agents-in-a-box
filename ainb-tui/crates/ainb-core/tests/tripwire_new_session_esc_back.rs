//! Tripwire: two-level back navigation. Esc on Configure returns to PickRepo
//! (with `ainb-tui` still highlighted); a second Esc returns to home.
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
fn esc_from_configure_returns_to_pick_repo_then_to_home() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-esc-back-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
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

    // Enter → Configure.
    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let on_cfg = poll_capture(&session, cfg_deadline, |c| {
        c.contains("Enter=Launch") && !c.contains("Enter=Select")
    });
    if on_cfg.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never rendered after Enter; last:\n---\n{last}\n---");
    }

    // 1st Esc → back to PickRepo with ainb-tui still highlighted (▸ marker on
    // an `ainb-tui` row).
    send_key(&session, "Escape");
    let back1_deadline = Instant::now() + Duration::from_secs(10);
    let back1 = poll_capture(&session, back1_deadline, |c| {
        c.contains("Enter=Select")
            && c.contains("ainb-tui")
            && c.contains('\u{25b8}')
            && !c.contains("Enter=Launch")
    });
    if back1.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Esc did not return to PickRepo; last:\n---\n{last}\n---");
    }

    // 2nd Esc → back to home.
    send_key(&session, "Escape");
    let back2_deadline = Instant::now() + Duration::from_secs(10);
    let back2 = poll_capture(&session, back2_deadline, |c| {
        c.contains("Stats") && c.contains("[i]") && !c.contains("Enter=Select")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        back2.is_some(),
        "Second Esc did not return to home. Final:\n---\n{final_cap}\n---"
    );
    assert!(
        !final_cap.contains("Enter=Launch"),
        "Configure chrome leaking to home:\n{final_cap}"
    );
}
