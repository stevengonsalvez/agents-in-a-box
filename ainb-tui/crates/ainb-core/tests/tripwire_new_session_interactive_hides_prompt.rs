//! Tripwire: when the active preset has `mode = "interactive"`, the Configure
//! screen does NOT render the Prompt textarea. Per spec decision: preset
//! declares mode; prompt-textarea visibility is preset-driven.
//!
//! Phase 5 of `plans/new-session-redesign-spec.md`.


#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Seed a single `claude-opus-interactive` preset (appended to the shipped
/// defaults file) and a session-defaults that auto-loads it for ainb-tui —
/// drives the Configure screen into the Interactive variant.
fn seed_interactive_preset(home: &Path) {
    // Re-seed the shipped defaults then append our test preset to the same
    // single-file `presets.toml`. Matches the post-2026-05-27 layout: one
    // TOML file with multiple `[[preset]]` entries.
    seed_default_presets(home);
    let presets_file = home.join(".agents-in-a-box").join("presets.toml");
    let mut content = fs::read_to_string(&presets_file).unwrap();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(
        r#"
[[preset]]
name = "claude-opus-interactive"
description = "Claude Opus, interactive shell"
agent_provider = "claude"
agent_model = "opus"
mode = "interactive"
[preset.permissions]
skip_all = false
"#,
    );
    fs::write(&presets_file, content).unwrap();
    let defaults_yaml = r#"last_repo: ainb-tui
per_repo:
  ainb-tui:
    last_preset: claude-opus-interactive
    last_branch_override: null
    last_prompt: null
    last_used_at: "2026-05-01T00:00:00Z"
"#;
    fs::write(
        home.join(".agents-in-a-box").join("session-defaults.yaml"),
        defaults_yaml,
    )
    .unwrap();
}

#[test]
fn interactive_preset_hides_prompt_textarea() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());
    seed_interactive_preset(home_tmp.path());

    let session = format!("tripwire-cfg-interactive-{}", std::process::id());
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
    let on_cfg = poll_capture(&session, cfg_deadline, |c| {
        c.contains("Enter=Launch") && c.contains("claude-opus-interactive")
    });
    let final_cap = capture(&session);
    kill_session(&session);

    let Some(cap) = on_cfg else {
        panic!(
            "Configure never rendered with claude-opus-interactive. \
             Final:\n---\n{final_cap}\n---"
        );
    };
    // Positive: preset name visible.
    assert!(
        cap.contains("Preset:"),
        "Preset row missing from Interactive variant:\n{cap}"
    );
    // Negative: NO Prompt textarea.
    assert!(
        !cap.contains("Prompt:"),
        "Prompt textarea must be hidden for Interactive presets:\n{cap}"
    );
}
