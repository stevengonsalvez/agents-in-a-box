//! Tripwire: after the Phase 6 hard-replace migration, pressing `n` from
//! HomeScreen must route into the redesigned PickRepo screen, and NONE of
//! the legacy 13-step wizard's UI strings may appear. We additionally
//! pound on every "old-flow trigger" key (`l`, `r`, `s`, `f` quick-select
//! keys plus `Tab` for the branch checkout mode toggle) to prove that the
//! routing is dead, not merely hidden.
//!
//! Phase 6 of `plans/new-session-redesign-spec.md`.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// Legacy-wizard UI strings — ANY occurrence in the post-Phase-6 new-session
/// flow proves a retired step's chrome leaked through.
const LEGACY_STRINGS: &[&str] = &[
    "Select Repository Source",
    "Select Repository",
    "Select Branch",
    "Select Agent",
    "Configure SSH",
    "Configure Permissions",
    "Select Mode",
    "Input Prompt",
    "Input Branch",
    "Input Repo Source",
    "Validating Repo",
    "Select Favorite",
];

#[test]
fn no_legacy_step_strings_appear_in_new_session_flow() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-no-legacy-{}", std::process::id());
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

    // 1. HomeScreen rendered.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre_home = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    if pre_home.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // 2. Open new-session — must land in PickRepo, not any legacy variant.
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(8);
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
    let Some(pick_cap) = on_pick else {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo never rendered after `n`; last:\n---\n{last}\n---");
    };

    // Positive markers — the redesigned chrome must be visible.
    assert!(
        pick_cap.contains("New Session"),
        "new chrome 'New Session' title missing:\n---\n{pick_cap}\n---"
    );
    assert!(
        pick_cap.contains("^R=Reset"),
        "new chrome '^R=Reset' help fragment missing:\n---\n{pick_cap}\n---"
    );

    // Negative assertion after open — no legacy chrome visible yet.
    for needle in LEGACY_STRINGS {
        assert!(
            !pick_cap.contains(needle),
            "legacy chrome '{needle}' visible right after `n`:\n---\n{pick_cap}\n---"
        );
    }

    // 3. Mash every old-flow trigger key. Each used to route into a now-
    // deleted variant; they must be inert in PickRepo (`l`/`r`/`s` are
    // smart-parse filter chars; `f` is the favorite-toggle prefix; `Tab`
    // used to flip BranchCheckoutMode in legacy SelectBranch).
    for key in ["l", "r", "s", "f", "Tab"] {
        send_key(&session, key);
        // Brief settle window — the picker handles each key locally, so
        // 500ms is generous.
        thread::sleep(Duration::from_millis(200));
        let cap = capture(&session);
        for needle in LEGACY_STRINGS {
            assert!(
                !cap.contains(needle),
                "key '{key}' surfaced legacy chrome '{needle}':\n---\n{cap}\n---"
            );
        }
    }

    // Forward + return paired: the picker's Esc semantics are "clear filter
    // first, return to home only when filter is empty" (the sticky-filter
    // UX from the spec). After mashing `l r s f`, the filter is non-empty,
    // so the first Esc clears it and the second returns home. Two presses
    // is intentional — papering over it with a wait would mask the
    // sticky-filter behaviour we explicitly want to preserve.
    send_key(&session, "Escape");
    thread::sleep(Duration::from_millis(300));
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
