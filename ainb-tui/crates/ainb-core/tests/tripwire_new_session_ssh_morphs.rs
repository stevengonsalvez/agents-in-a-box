//! Tripwire: typing `ssh://user@host` on PickRepo + Enter advances to
//! Configure rendered in the SSH variant — Host/User/Port fields visible,
//! Branch/Prompt absent.
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
fn ssh_url_morphs_configure_to_ssh_variant() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-ssh-{}", std::process::id());
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
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("Enter=Select")
    });
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

    // Type SSH session URL (no repo segment → SshSession).
    send_text(&session, "ssh://deploy@prod-1.internal");
    let typed_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, typed_deadline, |c| c.contains("prod-1.internal"))
        .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("filter never typed; last:\n---\n{last}\n---");
    }

    send_key(&session, "Enter");

    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let on_cfg = poll_capture(&session, cfg_deadline, |c| {
        c.contains("Host:") && c.contains("User:") && c.contains("Port:")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    let Some(cap) = on_cfg else {
        panic!(
            "SSH variant did not render. Final:\n---\n{final_cap}\n---"
        );
    };
    // Positive markers.
    assert!(cap.contains("Host:"), "SSH variant missing Host:\n{cap}");
    assert!(cap.contains("User:"), "SSH variant missing User:\n{cap}");
    assert!(cap.contains("Port:"), "SSH variant missing Port:\n{cap}");
    // Negative: Branch/Prompt must NOT render in SSH variant.
    assert!(
        !cap.contains("Branch:"),
        "SSH variant must NOT show Branch:\n{cap}"
    );
    assert!(
        !cap.contains("Prompt:"),
        "SSH variant must NOT show Prompt:\n{cap}"
    );
}
