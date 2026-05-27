//! Tripwire: switching to `Custom` preset and arrow-cycling the Yolo row
//! flips `permissions.skip_all`. Default preset ships ON; cycling Yolo via
//! arrow on the Custom slot flips to OFF + badge fires; flipping again
//! returns to ON.
//!
//! Part of the new-session redesign polish (`docs/specs/new-session-redesign-spec.md`).


#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn yolo_toggle_key_flips_on_off_and_marks_modified() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-yolo-toggle-{}", std::process::id());
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

    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let initial = poll_capture(&session, cfg_deadline, |c| {
        c.contains("Yolo:") && c.contains("ON") && c.contains("Enter=Launch")
    });
    if initial.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure did not render with Yolo: ON default; last:\n---\n{last}\n---");
    }
    let initial = initial.unwrap();
    assert!(
        !initial.contains("modified"),
        "modified badge visible BEFORE yolo toggle:\n{initial}"
    );

    // Right-arrow twice on the Preset row: claude-interactive-yolo → codex-interactive-yolo →
    // Custom, unlocking Yolo editing.
    send_key(&session, "Right");
    send_key(&session, "Right");
    let to_custom_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, to_custom_deadline, |c| {
        c.contains("Custom") && c.contains("modified")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Right-arrow did not cycle to Custom; last:\n---\n{last}\n---");
    }

    // Tab to Yolo row. Custom seeded from codex-interactive-yolo; 2026-05
    // refresh shows Model row for Codex too, so visible rows for Custom are
    // Preset → Agent → Model → Mode → Yolo → Branch. Four Tabs reach Yolo.
    send_key(&session, "Tab"); // Preset → Agent
    send_key(&session, "Tab"); // Agent → Model
    send_key(&session, "Tab"); // Model → Mode
    send_key(&session, "Tab"); // Mode → Yolo
    send_key(&session, "Right");
    let to_off_deadline = Instant::now() + Duration::from_secs(5);
    let to_off = poll_capture(&session, to_off_deadline, |c| {
        c.contains("Yolo:") && c.contains("OFF")
    });
    if to_off.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!(
            "Right-arrow on Yolo row (Custom) did not flip ON→OFF; last:\n---\n{last}\n---"
        );
    }

    // Flip back to ON.
    send_key(&session, "Right");
    let to_on_deadline = Instant::now() + Duration::from_secs(5);
    let to_on = poll_capture(&session, to_on_deadline, |c| {
        c.contains("Yolo:") && c.contains("ON")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        to_on.is_some(),
        "Right-arrow did not flip Yolo OFF→ON. Final:\n---\n{final_cap}\n---"
    );
}
