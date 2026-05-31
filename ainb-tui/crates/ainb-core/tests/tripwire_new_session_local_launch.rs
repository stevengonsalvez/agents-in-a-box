//! Tripwire: pressing Enter on Configure with a LocalPath favorite actually
//! launches the session — it transitions into the "Creating Session" in-flight
//! UI (or further) and NEVER shows the Phase 6 "not yet wired" stub message.
//!
//! Phase 6.5 of `plans/new-session-redesign-spec.md`: this proves the
//! create-session dispatch is wired end-to-end for the LocalPath path. The
//! HttpsUrl / SshUrl / GithubShorthand and SshSession paths are intentionally
//! NOT tripwired here — they require network access (clone) or a real SSH
//! endpoint and are flaky in CI. Those are manual-test items; the dispatch
//! logic is covered by the `ssh_session_url_tests` unit module in `state.rs`.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn launching_local_repo_hits_create_session_dispatch_not_stub() {
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
    seed_favorites_pointing_at(home_tmp.path(), &repo);

    let session = format!("tripwire-local-launch-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // AINB_DISABLE_PLUGINS=1: keep the host-owned new-session screen path,
    // same as the other Phase 4-6 tripwires.
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

    // 2. Press `n` → PickRepo, retry once if the keystroke is dropped.
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

    // 3. Enter on the auto-highlighted favorite → Configure.
    send_key(&session, "Enter");
    let cfg_deadline = Instant::now() + Duration::from_secs(10);
    if poll_capture(&session, cfg_deadline, |c| {
        c.contains("Branch:") && c.contains("agents/")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Configure never opened; last:\n---\n{last}\n---");
    }

    // 4. Stevie 2026-05-27 added an explicit `[ Launch ]` row that is the
    //    canonical commit affordance (Enter elsewhere is no longer Launch).
    //    Navigate down past the visible rows — for a default Interactive
    //    preset on a local repo: Preset → Mode → Yolo → Branch → Launch (4
    //    ↓ presses). Then Enter to fire.
    for _ in 0..4 {
        send_key(&session, "Down");
    }
    // Confirm the Launch row is focused — the `▸ [ Launch ]` indicator
    // should be visible. This guards against future row-order changes.
    let launch_focus_deadline = Instant::now() + Duration::from_secs(3);
    if poll_capture(&session, launch_focus_deadline, |c| {
        c.contains("[ Launch ]") || c.contains("Launch")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Launch row never became reachable; last:\n---\n{last}\n---");
    }
    send_key(&session, "Enter");

    let launch_deadline = Instant::now() + Duration::from_secs(15);
    let final_state = poll_capture(&session, launch_deadline, |c| {
        c.contains("Creating Session")
            || c.contains("Creating Git worktree")
            || (c.contains("Stats") && !c.contains("New Session") && !c.contains("Branch:"))
    });

    let final_cap = capture(&session);
    kill_session(&session);

    // Hard negative: the Phase 6 stub message must NEVER appear. Even on
    // session-creation failure, the user should see a real diagnostic, not
    // the "not yet wired" placeholder that this phase deleted.
    assert!(
        !final_cap.contains("not yet wired"),
        "Phase 6 stub message resurfaced — launch dispatch regressed!\n---\n{final_cap}\n---"
    );

    // Positive: we DID transition past Configure. Either Creating banner,
    // a returned home view, or any post-Configure notification is fine —
    // but staying frozen on the Configure screen with no progress is a fail.
    assert!(
        final_state.is_some(),
        "Enter on Configure produced no observable state change in 15s. \
         Launch path may not be wired. Final capture:\n---\n{final_cap}\n---"
    );
}
