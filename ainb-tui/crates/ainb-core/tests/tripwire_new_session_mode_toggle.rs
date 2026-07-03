//! Tripwire: switching to the `Custom` preset slot via Right-arrow on the
//! Preset row unlocks Mode editing. Asserts:
//!   1. Default (Interactive) named preset → no Prompt textarea, no badge.
//!   2. Right-arrow on Preset row twice cycles to `Custom`, badge appears.
//!   3. Tab to Mode row, Right-arrow flips Interactive → Boss, Prompt
//!      textarea appears.
//!   4. Left-arrow flips Boss → Interactive, Prompt textarea disappears.
//!
//! Part of the new-session redesign polish (`docs/specs/new-session-redesign-spec.md`).

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn mode_toggle_key_flips_boss_interactive_and_marks_modified() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-mode-toggle-{}", std::process::id());
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

    // Wait for the HomeScreen so we know the binary booted.
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

    // Enter on auto-highlighted favorite → Configure (default preset is
    // claude-interactive-yolo which now ships `mode = "interactive"`).
    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let initial = poll_capture(&session, cfg_deadline, |c| {
        c.contains("claude-interactive-yolo")
            && c.contains("Mode:")
            && c.contains("Interactive")
            && c.contains("Enter=Launch")
    });
    if initial.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never opened with Interactive default; last:\n---\n{last}\n---");
    }
    let initial = initial.unwrap();
    // Negative: no prompt textarea yet, no modified badge yet.
    assert!(
        !initial.contains("Prompt:"),
        "Prompt textarea visible on Interactive default:\n{initial}"
    );
    assert!(
        !initial.contains("modified"),
        "modified badge visible BEFORE toggle:\n{initial}"
    );

    // Left-arrow on the Preset row wraps Named(0) → Custom. Seed = current
    // named preset (claude-interactive-yolo), giving a claude-shaped row
    // set including Model and Mode rows. At Custom the badge appears and
    // Mode editing unlocks.
    send_key(&session, "Left");
    let to_custom_deadline = Instant::now() + Duration::from_secs(5);
    let to_custom = poll_capture(&session, to_custom_deadline, |c| {
        c.contains("Custom") && c.contains("modified")
    });
    if to_custom.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Left-arrow did not cycle to Custom; last:\n---\n{last}\n---");
    }

    // Custom seeded from claude-interactive-yolo (agent="claude"); rows
    // are Preset → Agent → Model → Mode → Yolo → Branch. Three Tabs reach Mode.
    send_key(&session, "Tab");
    send_key(&session, "Tab");
    send_key(&session, "Tab");
    send_key(&session, "Right");
    let to_boss_deadline = Instant::now() + Duration::from_secs(5);
    let to_boss = poll_capture(&session, to_boss_deadline, |c| {
        c.contains("Mode:") && c.contains("Boss") && c.contains("Prompt:")
    });
    if to_boss.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!(
            "Right-arrow on Mode row (Custom) did not flip Interactive→Boss; last:\n---\n{last}\n---"
        );
    }

    // Left-arrow flips Boss → Interactive; Prompt textarea disappears.
    send_key(&session, "Left");
    let back_deadline = Instant::now() + Duration::from_secs(5);
    let back = poll_capture(&session, back_deadline, |c| {
        c.contains("Mode:") && c.contains("Interactive") && !c.contains("Prompt:")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        back.is_some(),
        "Left-arrow did not flip Boss→Interactive. Final:\n---\n{final_cap}\n---"
    );
}
