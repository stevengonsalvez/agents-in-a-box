//! Tripwire: the hard-refresh confirm journey end-to-end.
//!
//! `R` on the analytics screen must raise the ⚠ confirm overlay —
//! NOT fire a rebuild; `n` must dismiss it with the prior data still
//! on screen; `R` → `y` must dismiss the overlay, publish the hard
//! refresh, and the dashboard must come back with data after the
//! rebuild republishes. Asserting the full key→JSON-RPC→plugin→render
//! pipe, same shape as `tripwire_burndown_keys.rs`:
//!
//!   tmux send-keys → host forwards key → Burndown::handle_key →
//!     confirm_hard state → render overlay → (y) hard refresh_request
//!     → session-reader wipes caches + rebuilds → usage_data republish.
//!
//! Runs against the deterministic `tripwire_keys` fixture (isolated
//! HOME, pinned AINB_NOW). Skips gracefully when `tmux` or the staged
//! `dist/plugins` binaries are unavailable — mirroring the gates on
//! `tripwire_burndown_keys.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Walk up from the ainb binary looking for the `dist/plugins/`
/// staging dir produced by `scripts/build-plugins.sh`. Returns `None`
/// if absent so the test can skip rather than fail in fresh checkouts.
fn plugins_staged() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("burndown").join("burndown").exists()
            && candidate.join("session-reader").join("session-reader").exists()
        {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tripwire_keys")
}

fn fixture_now() -> String {
    fs::read_to_string(fixture_root().join("FIXTURE_NOW.txt"))
        .expect("FIXTURE_NOW.txt present")
        .trim()
        .to_string()
}

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir dst");
    for entry in fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("dir entry");
        let ty = entry.file_type().expect("file_type");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to);
        } else if ty.is_file() {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}

fn seed_fixture_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    let fixture = fixture_root();
    let claude_src = fixture.join("claude").join("projects");
    if claude_src.is_dir() {
        copy_dir_all(&claude_src, &home.join(".claude").join("projects"));
    }
    let codex_src = fixture.join("codex").join("sessions");
    if codex_src.is_dir() {
        copy_dir_all(&codex_src, &home.join(".codex").join("sessions"));
    }
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll_capture<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

/// The overlay's distinctive copy — present iff the confirm is up.
const OVERLAY_MARKER: &str = "Re-parses ALL session history";

fn has_data(cap: &str) -> bool {
    cap.contains("Usage Analytics")
        && cap.contains('$')
        && cap.chars().any(|ch| ch.is_ascii_digit())
}

#[test]
fn hard_refresh_requires_confirm_and_survives_cancel() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!(
            "SKIP: dist/plugins/{{burndown,session-reader}} not staged — \
             run `scripts/build-plugins.sh` first"
        );
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_fixture_home(home_tmp.path());

    let session = format!("tripwire-hardrefresh-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} AINB_PLUGIN_ROOT={} AINB_NOW={} exec {} tui",
        home_tmp.path().display(),
        plugin_root.display(),
        fixture_now(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    let home_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // Enter analytics, wait for real data.
    send_key(&session, "i");
    let data_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, data_deadline, |c| {
        has_data(c) && !c.contains("Waiting for session-reader plugin")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("burndown never rendered data; last:\n---\n{last}\n---");
    }

    // R must raise the confirm overlay — and must NOT have fired a
    // rebuild (the prior data is still behind the modal).
    send_key(&session, "R");
    let overlay = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains(OVERLAY_MARKER)
    });
    if overlay.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("R did not raise the hard-refresh confirm overlay; last:\n---\n{last}\n---");
    }

    // n cancels: overlay gone, data intact, nothing rebuilt.
    send_key(&session, "n");
    let cancelled = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        !c.contains(OVERLAY_MARKER) && has_data(c)
    });
    if cancelled.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("cancel did not dismiss overlay with data intact; last:\n---\n{last}\n---");
    }

    // R → y confirms: overlay drops, the hard rebuild runs, and the
    // dashboard must come back with data once the rebuild republishes.
    // (The fixture tree is tiny so the rebuild completes in seconds.)
    send_key(&session, "R");
    if poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains(OVERLAY_MARKER)
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("second R did not raise the overlay; last:\n---\n{last}\n---");
    }
    send_key(&session, "y");
    let rebuilt = poll_capture(&session, Instant::now() + Duration::from_secs(45), |c| {
        !c.contains(OVERLAY_MARKER) && has_data(c)
    });
    if rebuilt.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("hard refresh did not republish data; last:\n---\n{last}\n---");
    }

    kill_session(&session);
}
