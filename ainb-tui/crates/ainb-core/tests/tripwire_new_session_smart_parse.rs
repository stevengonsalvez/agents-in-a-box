//! Tripwire: smart-parse on Enter advances past PickRepo.
//!
//! Type `foo/bar` (no matching row), press Enter. The picker invokes
//! `RepoSource::parse_with` → `GithubShorthand` → `PickRepoOutcome::StartClone`
//! (or `AdvanceTo` for local hits). Phase 4 stubs the clone wire-up out
//! (Phase 5 owns the async clone + spinner), so the visible signal is that
//! the PickRepo title disappears AND the captured pane no longer shows
//! `^R=Reset` (the bottom help bar that's only present on PickRepo).
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
fn smart_parse_advances_past_pick_repo() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_new_session_fixtures(home_tmp.path());

    let session = format!("tripwire-smart-parse-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success());

    let cmd = launch_cmd_gh_authed(home_tmp.path(), &ainb);
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

    // Open PickRepo (retry once if `n` is dropped under load).
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
        panic!("PickRepo never opened after `n`; last:\n---\n{last}\n---");
    }

    // Type filter text — `foo/bar` smart-parses as GitHubShorthand → clone.
    send_text(&session, "foo/bar");

    // Confirm filter text typed (positive marker: the filter prompt shows it).
    let typed_deadline = Instant::now() + Duration::from_secs(5);
    let with_filter = poll_capture(&session, typed_deadline, |c| {
        c.contains("foo/bar") && c.contains("New Session")
    });
    if with_filter.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("filter `foo/bar` never appeared in PickRepo; last:\n---\n{last}\n---");
    }

    // Press Enter — smart-parse fires. Phase 4 routes to a cancel stub
    // (Phase 5 owns the spinner), so we observe a transition AWAY from
    // PickRepo: the title and help bar both vanish.
    send_key(&session, "Enter");

    let post_deadline = Instant::now() + Duration::from_secs(10);
    let post = poll_capture(&session, post_deadline, |c| {
        !c.contains("Enter=Select") && !c.contains("^R=Reset") && !c.contains("^F=Favorite")
    });

    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        post.is_some(),
        "smart-parse Enter did NOT advance past PickRepo within 10s. \
         Final:\n---\n{final_cap}\n---"
    );
    // Negative: PickRepo title gone confirms we left.
    assert!(
        !final_cap.contains("Enter=Select"),
        "PickRepo help bar still visible after Enter:\n---\n{final_cap}\n---"
    );
}
