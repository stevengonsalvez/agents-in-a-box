//! Tripwire: `^S` opens the save-preset modal. Typing a new name + Enter
//! appends a `[[preset]]` entry to `~/.agents-in-a-box/presets.toml` AND the
//! dropdown selects it.
//!
//! Phase 5 of `plans/new-session-redesign-spec.md`.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn ctrl_s_saves_new_preset_and_selects_it() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home_path = home_tmp.path().to_path_buf();
    seed_isolated_home(&home_path);
    seed_new_session_fixtures(&home_path);

    let session = format!("tripwire-cfg-save-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success());

    let cmd = launch_cmd_gh_authed(&home_path, &ainb);
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
    if poll_capture(&session, cfg_deadline, |c| {
        c.contains("Enter=Launch") && c.contains("claude-interactive-yolo")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never rendered; last:\n---\n{last}\n---");
    }

    // Right-arrow on Preset row → cycles to the other named preset →
    // modified badge fires.
    send_key(&session, "Right");
    let mod_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, mod_deadline, |c| c.contains("modified")).is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Right-arrow did not produce modified badge; last:\n---\n{last}\n---");
    }

    // ^S opens save-preset modal.
    send_key(&session, "C-s");
    let modal_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, modal_deadline, |c| c.contains("Save preset as")).is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("^S did not open save-preset modal; last:\n---\n{last}\n---");
    }

    // Type the new name + Enter.
    send_text(&session, "claude-haiku-yolo");
    send_key(&session, "Enter");

    // The modal closes; new preset becomes the selected entry.
    let saved_deadline = Instant::now() + Duration::from_secs(10);
    let saved = poll_capture(&session, saved_deadline, |c| {
        c.contains("claude-haiku-yolo")
            && !c.contains("Save preset as")
            && c.contains("Enter=Launch")
    });

    // Disk check — the new preset must appear as a `[[preset]]` entry in
    // the single-file `~/.agents-in-a-box/presets.toml`.
    let presets_path = home_path.join(".agents-in-a-box").join("presets.toml");
    let disk_deadline = Instant::now() + Duration::from_secs(5);
    let mut on_disk = false;
    while Instant::now() < disk_deadline {
        if presets_path.exists() {
            let content = std::fs::read_to_string(&presets_path).unwrap_or_default();
            if content.contains("\"claude-haiku-yolo\"") {
                on_disk = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        on_disk,
        "preset not found in {} as [[preset]] name=\"claude-haiku-yolo\"",
        presets_path.display()
    );
    assert!(
        saved.is_some(),
        "Configure did not surface claude-haiku-yolo as selected. \
         Final:\n---\n{final_cap}\n---"
    );
}
