//! Tripwire: when the active preset's `agent_model = "default"` (the new
//! `claude-interactive-yolo` / `codex-interactive-yolo` default), launching
//! must NOT include `--model` in the spawned CLI command. The Configure
//! screen's Model row should display "system default" (muted-grey italic)
//! for both Claude and Codex defaults.
//!
//! This proves the 2026-05 model-flag-omission semantics for the
//! `SystemDefault` variants of `ClaudeModel` and `CodexModel`. The full CLI
//! emission path is also covered by unit tests in `cli/run.rs` (which
//! exercise `build_agent_command` directly) — this PTY-level tripwire just
//! confirms the wired-up Configure screen presents the "system default"
//! label end-to-end.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn configure_shows_system_default_for_default_model_preset() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-default-model-{}", std::process::id());
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

    // 1. HomeScreen
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

    // 2. Open PickRepo.
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

    // 3. Enter on auto-highlighted favorite → Configure (default preset is
    //    claude-interactive-yolo with `agent_model = "default"`).
    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let initial = poll_capture(&session, cfg_deadline, |c| {
        c.contains("claude-interactive-yolo") && c.contains("Enter=Launch")
    });
    if initial.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never opened on default preset; last:\n---\n{last}\n---");
    }
    let initial = initial.unwrap();
    // Default preset (`agent_model = "default"`) — Named selection hides the
    // Agent / Model rows. The preset's description line at the top of the
    // Configure screen should mention the system-default model. We assert the
    // description carries the "default" token rather than a stale "opus"/"sonnet"
    // — that proves the new TOML default landed.
    assert!(
        initial.contains("default"),
        "Named preset description must mention 'default' (system default model). Got:\n{initial}"
    );

    // 4. Switch to Custom to expose the Model row, then assert it reads
    //    "system default" verbatim (matches `ClaudeModel::SystemDefault::display_label()`).
    // Left-arrow wraps Named(0) → Custom with seed = claude-interactive-yolo
    // (current_preset). Custom seed agent=claude → Model row reads
    // "system default" since agent_model="default".
    send_key(&session, "Left");
    let custom_deadline = Instant::now() + Duration::from_secs(5);
    let custom_cap = poll_capture(&session, custom_deadline, |c| {
        c.contains("Custom") && c.contains("Model:") && c.contains("system default")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        custom_cap.is_some(),
        "Custom preset (seeded from codex-interactive-yolo, agent_model=\"default\") \
         must show Model row reading \"system default\". Final:\n---\n{final_cap}\n---"
    );
}
