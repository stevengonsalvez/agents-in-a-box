//! Tripwire: when the user Escs out of Configure mid-prompt, the partial
//! prompt is persisted to `session-defaults.yaml.per_repo[repo].last_prompt`
//! and restored on re-entry into Configure.
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
fn half_typed_prompt_persists_across_esc_back() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home_path = home_tmp.path().to_path_buf();
    seed_isolated_home(&home_path);
    seed_new_session_fixtures(&home_path);

    let session = format!("tripwire-cfg-half-prompt-{}", std::process::id());
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
    // Default preset is Interactive → no prompt textarea. Switch to Custom
    // and flip Mode to Boss so the textarea exists.
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    if poll_capture(&session, cfg_deadline, |c| {
        c.contains("Mode:") && c.contains("Enter=Launch")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never rendered; last:\n---\n{last}\n---");
    }
    // Cycle to Custom by Left-arrow (wraps Named(0) → Custom). Seed for
    // Custom on a wrap-cycle is the current named preset
    // (claude-interactive-yolo), so the Custom rows match the codex/claude
    // shape with a Model row.
    send_key(&session, "Left");
    send_key(&session, "Tab"); // Preset → Agent
    send_key(&session, "Tab"); // Agent → Model (claude has Model row)
    send_key(&session, "Tab"); // Model → Mode
    send_key(&session, "Right"); // Interactive → Boss
    let boss_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, boss_deadline, |c| {
        c.contains("Prompt:") && c.contains("Boss")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Custom→Boss did not reveal Prompt textarea; last:\n---\n{last}\n---");
    }
    send_key(&session, "Tab"); // Mode → Yolo
    send_key(&session, "Tab"); // Yolo → Branch
    send_key(&session, "Tab"); // Branch → Prompt

    // Type partial prompt.
    send_text(&session, "fix the s");
    let typed_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, typed_deadline, |c| c.contains("fix the s")).is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("prompt text never typed; last:\n---\n{last}\n---");
    }

    // Esc → PickRepo. Persistence writes to YAML.
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(10);
    if poll_capture(&session, back_deadline, |c| {
        c.contains("Enter=Select") && !c.contains("Enter=Launch")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Esc did not return to PickRepo; last:\n---\n{last}\n---");
    }

    // YAML carries last_prompt.
    let yaml_path = home_path.join(".agents-in-a-box").join("session-defaults.yaml");
    let yaml_deadline = Instant::now() + Duration::from_secs(5);
    let mut yaml_content = String::new();
    while Instant::now() < yaml_deadline {
        if let Ok(s) = fs::read_to_string(&yaml_path) {
            if s.contains("last_prompt") && s.contains("fix the s") {
                yaml_content = s;
                break;
            }
        }
        thread::sleep(Duration::from_millis(150));
    }
    assert!(
        yaml_content.contains("fix the s"),
        "YAML missing partial prompt: {yaml_content:?}"
    );

    // Re-enter Configure — partial prompt should be restored. Default preset
    // is Interactive so the textarea is hidden; toggle to Boss to expose it.
    send_key(&session, "Enter");
    let reenter_deadline = Instant::now() + Duration::from_secs(10);
    if poll_capture(&session, reenter_deadline, |c| {
        c.contains("Mode:") && c.contains("Enter=Launch")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure did not re-render on re-entry; last:\n---\n{last}\n---");
    }
    // Cycle to Custom + flip Mode → Boss so the Prompt textarea re-appears.
    // Left-arrow on Named(0) wraps to Custom with seed = current named
    // preset (claude-interactive-yolo), giving us a claude-shaped row set.
    send_key(&session, "Left");
    send_key(&session, "Tab"); // Preset → Agent
    send_key(&session, "Tab"); // Agent → Model
    send_key(&session, "Tab"); // Model → Mode
    send_key(&session, "Right"); // Interactive → Boss
    let reveal_deadline = Instant::now() + Duration::from_secs(5);
    let restored = poll_capture(&session, reveal_deadline, |c| {
        c.contains("Enter=Launch") && c.contains("fix the s")
    });
    let final_cap = capture(&session);
    let final_yaml = fs::read_to_string(&yaml_path).unwrap_or_else(|_| "<missing>".into());
    kill_session(&session);

    assert!(
        restored.is_some(),
        "Configure did not restore partial prompt on re-entry.\n\
         YAML:\n---\n{final_yaml}\n---\n\
         Final:\n---\n{final_cap}\n---"
    );
}
