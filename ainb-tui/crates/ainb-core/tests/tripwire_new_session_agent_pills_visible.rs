//! Tripwire: the Configure screen renders the Agent row as inline option
//! pills (Claude · Codex · Gemini · Copilot · Shell · SSH) with the current
//! one highlighted and Gemini greyed-out (`[soon]`),
//! not as a single `◀ Claude ▶` cycle display.
//!
//! Discoverability fix from the 2026-05-27 redesign — users couldn't tell
//! which other agents were available without cycling through them blindly.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn agent_row_renders_inline_pills_in_custom() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home_path = home_tmp.path().to_path_buf();
    seed_isolated_home(&home_path);
    seed_new_session_fixtures(&home_path);

    let session = format!("tripwire-cfg-pills-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
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

    // Open new-session picker, then Configure.
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(8);
    if poll_capture(&session, pick_deadline, |c| c.contains("Enter=Select")).is_none() {
        send_key(&session, "n");
        let retry = Instant::now() + Duration::from_secs(8);
        if poll_capture(&session, retry, |c| c.contains("Enter=Select")).is_none() {
            let last = capture(&session);
            kill_session(&session);
            panic!("PickRepo never opened; last:\n---\n{last}\n---");
        }
    }

    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    if poll_capture(&session, cfg_deadline, |c| {
        c.contains("Enter=Launch") && c.contains("Preset:")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never rendered; last:\n---\n{last}\n---");
    }

    // Pills should already be visible on the Preset row: at least the
    // shipped preset names + `Custom` separated by `·`.
    let preset_pills = capture(&session);
    let has_preset_dot_sep = preset_pills.contains("\u{00b7}") || preset_pills.contains("·");
    assert!(
        has_preset_dot_sep,
        "Preset row should render dot-separated pills (·); capture:\n---\n{preset_pills}\n---"
    );

    // Right-arrow until we land on Custom — that unlocks the Agent row so
    // we can verify the pill rendering for it too. The preset ring length
    // for the shipped fixtures is 4 named + Custom = 5; pressing Right four
    // times from the first named entry lands on Custom.
    for _ in 0..4 {
        send_key(&session, "Right");
        std::thread::sleep(Duration::from_millis(120));
    }

    let custom_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, custom_deadline, |c| c.contains("Custom")).is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("Did not cycle to Custom preset; last:\n---\n{last}\n---");
    }

    // Tab to the Agent row.
    send_key(&session, "Tab");
    std::thread::sleep(Duration::from_millis(200));

    let agent_deadline = Instant::now() + Duration::from_secs(5);
    let final_cap = poll_capture(&session, agent_deadline, |c| {
        c.contains("Agent:") && c.contains("Claude") && c.contains("Codex")
    });

    kill_session(&session);

    let cap = final_cap.unwrap_or_else(|| String::from("(no capture)"));
    // Look for the Agent row rendering all four pills inline.
    let agent_line = cap.lines().find(|l| l.contains("Agent:")).unwrap_or("");
    assert!(
        agent_line.contains("Claude"),
        "Agent row missing Claude pill: {agent_line}"
    );
    assert!(
        agent_line.contains("Codex"),
        "Agent row missing Codex pill: {agent_line}"
    );
    assert!(
        agent_line.contains("Shell"),
        "Agent row missing Shell pill: {agent_line}"
    );
    assert!(
        agent_line.contains("SSH"),
        "Agent row missing SSH pill: {agent_line}"
    );
    assert!(
        agent_line.contains("Copilot"),
        "Agent row missing Copilot pill: {agent_line}"
    );
    assert!(
        agent_line.contains("Gemini"),
        "Agent row missing Gemini pill (greyed-out): {agent_line}"
    );
    assert!(
        agent_line.contains("[soon]"),
        "Gemini pill should carry the greyed-out [soon] tag: {agent_line}"
    );
    // Negative: should NOT be using the lone cycle-arrow display where the
    // OTHER pills are absent. Older render had `◀ Claude ▶` only.
    let has_inline_separator = agent_line.contains("\u{00b7}") || agent_line.contains("·");
    assert!(
        has_inline_separator,
        "Agent row should render dot-separated pills, got: {agent_line}\nFull capture:\n---\n{cap}\n---"
    );
}
