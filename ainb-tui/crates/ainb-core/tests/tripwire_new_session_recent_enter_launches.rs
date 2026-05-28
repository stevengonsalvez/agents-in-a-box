//! Tripwire (finding #1): pressing Enter on a Recent row in the unified
//! picker advances the user to the Configure screen — it is NOT a silent
//! no-op.
//!
//! Pre-fix the picker stamped every recent with `RepoSource::Filter(alias)`
//! which dispatched to `PickRepoOutcome::Stay`; observable bug = `n` →
//! arrow-down → Enter sat on PickRepo with no feedback. Fix persists
//! `source_type` + `source` on every successful launch so the next open
//! reconstructs a real `RepoSource`.
//!
//! Phase 6 follow-up of `plans/new-session-redesign-spec.md`.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn seed_recent_pointing_at_local_repo(home: &Path, repo: &Path) {
    let root = home.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    // Empty favorites so the recent row is the only non-empty entry.
    let favorites_yaml = r#"version: 1
favorites: []
settings:
  auto_promote_threshold: 5
"#;
    fs::write(root.join("favorites.yaml"), favorites_yaml).unwrap();
    let defaults_yaml = format!(
        r#"last_repo: seeded-repo
per_repo:
  seeded-repo:
    last_preset: claude-interactive-yolo
    last_branch_override: null
    last_prompt: null
    last_used_at: "2026-05-01T00:00:00Z"
    source_type: local_path
    source: {}
"#,
        repo.display()
    );
    fs::write(root.join("session-defaults.yaml"), defaults_yaml).unwrap();
}

#[test]
fn enter_on_recent_advances_to_configure_not_stay() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    if !git_available() {
        eprintln!("SKIP: git not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let repo = seed_local_git_repo(home_tmp.path());
    seed_recent_pointing_at_local_repo(home_tmp.path(), &repo);

    let session = format!("tripwire-recent-enter-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

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
    if poll_capture(&session, home_deadline, |c| c.contains("Stats") && c.contains("[i]")).is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }

    // 2. Press `n` -> PickRepo with a Recent row (⌚ marker). Retry once
    // if the keystroke is lost (tmux send-keys drops chars ~5% of the time
    // under CPU contention; matches the pick_repo tripwire pattern).
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(8);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("Enter=Select") && c.contains('\u{231a}')
    });
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(10);
        on_pick = poll_capture(&session, retry_deadline, |c| {
            c.contains("Enter=Select") && c.contains('\u{231a}')
        });
    }
    if on_pick.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo with ⌚ recent never opened; last:\n---\n{last}\n---");
    }

    // 3. Enter on the auto-highlighted recent (last_repo points at it).
    // Pre-fix: this was a no-op and PickRepo stayed visible.
    // Post-fix: Configure opens with a Branch line.
    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    let on_cfg = poll_capture(&session, cfg_deadline, |c| {
        c.contains("Branch:") && c.contains("agents/")
    });
    let last = capture(&session);
    kill_session(&session);
    assert!(
        on_cfg.is_some(),
        "Enter on Recent did NOT advance to Configure (finding #1 regressed). \
         Last:\n---\n{last}\n---"
    );
}
