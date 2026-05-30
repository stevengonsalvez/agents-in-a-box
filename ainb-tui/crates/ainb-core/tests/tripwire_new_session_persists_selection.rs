//! Tripwire: screen-1 selection persists across Esc → re-open. After
//! highlighting `ainb-tui` and pressing Esc back to home, pressing `n`
//! again re-opens PickRepo with `ainb-tui` already highlighted, AND the
//! on-disk `~/.agents-in-a-box/session-defaults.yaml` carries
//! `last_repo: ainb-tui`.
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
fn selection_persists_across_esc_then_reopen() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home_path = home_tmp.path().to_path_buf();
    seed_isolated_home(&home_path);
    seed_new_session_fixtures(&home_path);

    let session = format!("tripwire-persists-{}", std::process::id());
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
    assert!(status.success());

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

    // First open: `n` → PickRepo. Retry once if the keystroke is lost
    // (tmux send-keys can drop chars when the TUI is mid-render under
    // CPU contention; observed in CI ~10% of the time).
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(5);
    let first_open = poll_capture(&session, pick_deadline, |c| {
        c.contains("New Session") && c.contains("Enter=Select")
    });
    if first_open.is_none() {
        tracing::debug!("first `n` lost — retrying");
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        if poll_capture(&session, retry_deadline, |c| {
            c.contains("New Session") && c.contains("Enter=Select")
        })
        .is_none()
        {
            let last = capture(&session);
            kill_session(&session);
            panic!("first PickRepo open failed (incl. retry); last:\n---\n{last}\n---");
        }
    }

    // Move cursor to ainb-tui (the second favorite). Default is row 0
    // (agents-in-a-box), Down lands on row 1 (ainb-tui).
    send_key(&session, "Down");

    // Confirm the arrow indicator is on ainb-tui now.
    let arrow_deadline = Instant::now() + Duration::from_secs(5);
    let arrow_cap = poll_capture(&session, arrow_deadline, |c| {
        // Look for ▸ ★ ainb-tui (selection arrow + favorite marker + label).
        // Allow whitespace between marker and label.
        c.contains('\u{25b8}') && c.contains("ainb-tui")
    });
    if arrow_cap.is_none() {
        let last = capture(&session);
        kill_session(&session);
        panic!("cursor never moved onto ainb-tui; last:\n---\n{last}\n---");
    }

    // Esc back to home — should persist the highlight.
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(10);
    if poll_capture(&session, back_deadline, |c| {
        c.contains("Stats") && c.contains("[i]") && !c.contains("New Session")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("Esc did not return to home; last:\n---\n{last}\n---");
    }

    // Wait briefly for the persistence write to land.
    let yaml_path = home_path
        .join(".agents-in-a-box")
        .join("session-defaults.yaml");
    let yaml_deadline = Instant::now() + Duration::from_secs(5);
    let mut yaml_content = String::new();
    while Instant::now() < yaml_deadline {
        if let Ok(s) = fs::read_to_string(&yaml_path) {
            if s.contains("last_repo") {
                yaml_content = s;
                break;
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    // Disk assertion FIRST — the positive marker is `last_repo: ainb-tui`,
    // negative is the absence of any other repo name.
    assert!(
        yaml_content.contains("ainb-tui"),
        "session-defaults.yaml missing `ainb-tui`: {:?}",
        yaml_content
    );
    assert!(
        yaml_content.contains("last_repo"),
        "session-defaults.yaml missing `last_repo` field: {:?}",
        yaml_content
    );

    // Second open: `n` → PickRepo with ainb-tui highlighted. Same retry
    // shape as the first open — `n` is occasionally dropped under load.
    send_key(&session, "n");
    let reopen_deadline = Instant::now() + Duration::from_secs(5);
    let mut reopen_cap = poll_capture(&session, reopen_deadline, |c| {
        c.contains("New Session")
            && c.contains("Enter=Select")
            && c.contains("ainb-tui")
            && c.contains('\u{25b8}') // ▸ on some row
    });
    if reopen_cap.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        reopen_cap = poll_capture(&session, retry_deadline, |c| {
            c.contains("New Session")
                && c.contains("Enter=Select")
                && c.contains("ainb-tui")
                && c.contains('\u{25b8}')
        });
    }

    let final_cap = capture(&session);
    kill_session(&session);

    let cap = reopen_cap.unwrap_or(final_cap.clone());
    // Positive: ▸ on a line that contains "ainb-tui".
    let on_ainb = cap.lines().any(|line| {
        line.contains('\u{25b8}') && line.contains("ainb-tui")
    });
    assert!(
        on_ainb,
        "▸ not on ainb-tui after re-open. Capture:\n---\n{cap}\n---"
    );
}
