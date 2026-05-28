//! Tripwire: `^R` on PickRepo clears the filter, resets the highlight
//! back to row 0, and removes `last_repo` from `session-defaults.yaml`.
//! Per-repo history under `per_repo:` is preserved (only `last_repo` is
//! cleared — see `SessionDefaults::reset_last_repo`).
//!
//! Phase 4 of `plans/new-session-redesign-spec.md`.


#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn ctrl_r_clears_last_repo_and_resets_highlight() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home_path = home_tmp.path().to_path_buf();
    seed_isolated_home(&home_path);
    seed_new_session_fixtures(&home_path);
    // Seed a pre-existing session-defaults.yaml that points last_repo at
    // ainb-tui plus a per_repo entry — ^R must clear the former but keep
    // the latter. (Persistence is dispatcher-owned post-finding-#3, so we
    // assert YAML state AFTER navigating away via Esc.)
    let root = home_path.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    let seed_yaml = r#"last_repo: ainb-tui
per_repo:
  ainb-tui:
    last_preset: claude-interactive-yolo
    last_branch_override: null
    last_prompt: null
    last_used_at: "2026-05-01T00:00:00Z"
"#;
    fs::write(root.join("session-defaults.yaml"), seed_yaml).unwrap();

    let session = format!("tripwire-reset-{}", std::process::id());
    let ainb = ainb_bin();

    Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "180",
            "-y",
            "50",
        ])
        .status()
        .expect("tmux new-session");

    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_path.display(),
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

    // Open PickRepo. Should restore `ainb-tui` highlight from seeded yaml.
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

    // Type some filter text — `^R` should clear it.
    send_text(&session, "ainb");
    let typed_deadline = Instant::now() + Duration::from_secs(5);
    if poll_capture(&session, typed_deadline, |c| {
        c.contains("> ainb") || c.contains(">ainb")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("filter `ainb` never appeared; last:\n---\n{last}\n---");
    }

    // Press ^R.
    send_key(&session, "C-r");

    // After ^R: filter cleared (positive), highlight on first row, and
    // disk `last_repo:` removed. `^R` is a deliberate-user-action and
    // persists synchronously (see `pick_repo::handle_key` comment) so we
    // assert on YAML state without navigating away.
    let after_deadline = Instant::now() + Duration::from_secs(5);
    let after_cap = poll_capture(&session, after_deadline, |c| {
        // Help bar still present (we're still on PickRepo), filter empty.
        c.contains("New Session")
            && c.contains("^R=Reset")
            // The filter prompt line is `> ` with nothing typed.
            && !c.contains("> ainb")
            && !c.contains(">ainb")
    });

    // YAML check: poll for last_repo removal.
    let yaml_path = home_path
        .join(".agents-in-a-box")
        .join("session-defaults.yaml");
    let yaml_deadline = Instant::now() + Duration::from_secs(5);
    let mut yaml_after = String::new();
    while Instant::now() < yaml_deadline {
        if let Ok(s) = fs::read_to_string(&yaml_path) {
            // After reset: `last_repo` should be `null` or absent.
            if !s.contains("last_repo: ainb-tui") {
                yaml_after = s;
                break;
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        after_cap.is_some(),
        "filter not cleared after ^R; final:\n---\n{final_cap}\n---"
    );
    // Positive: per_repo block survives (history must NOT be wiped).
    assert!(
        yaml_after.contains("per_repo"),
        "per_repo history wiped by ^R — must be preserved per spec.\nYAML: {yaml_after:?}"
    );
    // Negative: `last_repo: ainb-tui` gone.
    assert!(
        !yaml_after.contains("last_repo: ainb-tui"),
        "^R failed to clear last_repo. YAML: {yaml_after:?}"
    );
}
