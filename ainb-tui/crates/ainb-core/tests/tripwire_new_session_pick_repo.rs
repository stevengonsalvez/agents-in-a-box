//! Tripwire: pressing `n` on HomeScreen opens the new PickRepo screen
//! with the "New Session" title, the ★ favorites marker, and a help bar
//! advertising `^R=Reset` and `^F=Favorite`. Pressing Esc returns to home.
//!
//! Forward + return paired in a single test (the canonical shape per
//! `.claude/skills/tmux-ui-tripwire/SKILL.md` — forward-only tripwires let
//! Esc swallows ship undetected, e.g. PR #128).
//!
//! Phase 4 of `plans/new-session-redesign-spec.md`.


#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn n_opens_pick_repo_then_esc_returns_home() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-pick-repo-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
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
    assert!(status.success(), "tmux new-session failed");

    // Plugins off — the new-session screen is host-owned (Phase 4 of the
    // redesign explicitly keeps the screen in `legacy::NewSessionComponent`
    // dispatching to `pick_repo::render`).
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_tmp.path().display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux launch");

    // 1. HomeScreen rendered (sidebar + Stats entry).
    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre_home = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    if pre_home.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // Pre-press negative assertion: we are NOT on PickRepo yet.
    let pre_cap = capture(&session);
    assert!(
        !pre_cap.contains("New Session"),
        "PickRepo title visible before pressing `n` — state leaked\n{pre_cap}"
    );

    // 2. Press `n` — PickRepo opens. Retry once if the keystroke is lost
    // (tmux send-keys drops chars under CPU contention ~5% of the time;
    // a single retry erases the flake without papering over real bugs).
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(5);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("New Session")
            && c.contains("Enter=Select")
            && !c.contains("Loading placeholder")
    });
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        on_pick = poll_capture(&session, retry_deadline, |c| {
            c.contains("New Session")
                && c.contains("Enter=Select")
                && !c.contains("Loading placeholder")
        });
    }
    let Some(pick_cap) = on_pick else {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo never rendered after `n`; last:\n---\n{last}\n---");
    };

    // Positive markers: the favorite row's ★ + help-bar advertisements.
    assert!(
        pick_cap.contains('\u{2605}'),
        "favorites row marker (★) missing from PickRepo:\n---\n{pick_cap}\n---"
    );
    assert!(
        pick_cap.contains("^R=Reset"),
        "help bar missing ^R=Reset:\n---\n{pick_cap}\n---"
    );
    assert!(
        pick_cap.contains("^F=Favorite"),
        "help bar missing ^F=Favorite:\n---\n{pick_cap}\n---"
    );
    // Negative assertion: the old "Select repo source" wizard chrome must
    // NOT be visible — confirms we routed to the new screen not the legacy.
    assert!(
        !pick_cap.contains("Select repo source") && !pick_cap.contains("Stats"),
        "legacy chrome bleeding through PickRepo:\n---\n{pick_cap}\n---"
    );

    // 3. Forward+return: Esc returns to home.
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(10);
    let back_home = poll_capture(&session, back_deadline, |c| {
        c.contains("Stats") && c.contains("[i]") && !c.contains("New Session")
    });

    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        back_home.is_some(),
        "Esc on PickRepo did not return to home within 10s. \
         Final capture:\n---\n{final_cap}\n---"
    );
}
