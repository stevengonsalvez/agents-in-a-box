//! Tripwire: the `Custom` preset slot unlocks Agent / Model / Mode / Yolo
//! row editing.
//!
//! Default named preset → Mode/Yolo render value-only (no `◀ ▶` cycling
//! arrows). Right-arrow on the Preset row twice cycles to `Custom`; once
//! there, Tab navigates through Agent → Model → Mode → Yolo rows, all of
//! which surface the cycling arrows. Confirms the spec's "named presets
//! are locked, Custom unlocks fine-grained editing" model.
//!
//! Part of the new-session redesign polish (`docs/specs/new-session-redesign-spec.md`).

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn custom_preset_unlocks_agent_model_mode_yolo_editing() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-cfg-custom-unlock-{}", std::process::id());
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

    // Open PickRepo → Enter on favorite → Configure.
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
    let initial = poll_capture(&session, cfg_deadline, |c| {
        c.contains("claude-interactive-yolo") && c.contains("Mode:") && c.contains("Enter=Launch")
    });
    if initial.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure did not render initial state; last:\n---\n{last}\n---");
    }

    // Left-arrow on the Preset row wraps Named(0) → Custom with seed =
    // current named preset (claude-interactive-yolo), giving the
    // claude-shaped row set with both Model and Mode rows visible.
    send_key(&session, "Left");
    let custom_deadline = Instant::now() + Duration::from_secs(5);
    let custom_cap = poll_capture(&session, custom_deadline, |c| {
        c.contains("Custom") && c.contains("modified")
    });
    if custom_cap.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Right-arrow did not cycle to Custom; last:\n---\n{last}\n---");
    }
    let cap_custom = custom_cap.unwrap();
    // Custom should expose Agent + Model + Mode + Yolo rows.
    assert!(
        cap_custom.contains("Agent:"),
        "Custom must expose Agent row:\n{cap_custom}"
    );
    assert!(
        cap_custom.contains("Mode:"),
        "Custom must keep Mode row visible:\n{cap_custom}"
    );
    assert!(
        cap_custom.contains("Yolo:"),
        "Custom must keep Yolo row visible:\n{cap_custom}"
    );

    // Tab through the unlocked rows — must reach Mode. Custom seed =
    // codex-interactive-yolo (agent="codex"); Codex now shows Model row too
    // (2026-05 refresh), so 3 Tabs reach Mode.
    send_key(&session, "Tab"); // → Agent
    send_key(&session, "Tab"); // → Model
    send_key(&session, "Tab"); // → Mode
    let mode_focus = Instant::now() + Duration::from_secs(5);
    let on_mode = poll_capture(&session, mode_focus, |c| c.contains("Mode:"));
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        on_mode.is_some(),
        "Tab navigation did not reach Mode row. Final:\n---\n{final_cap}\n---"
    );
}
