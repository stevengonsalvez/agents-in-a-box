//! Tripwire (overlay-panels): Esc on the burndown wire plugin pops ONE
//! internal level per press, and only closes the panel once it's at its
//! root view.
//!
//! This is the wire-plugin half of the "Esc pops one level" contract
//! (the built-in half — skills/inbox — is covered by their own
//! tripwires). The mechanism is the hard part: `plugin/handle_key` is a
//! fire-and-forget notification, so the plugin signals "I'm at root,
//! close me" by publishing `ui.close_request`, which the host polls and
//! acts on. A regression that made Esc close immediately (instead of
//! unzooming first) — or that swallowed the root Esc entirely — would be
//! invisible to a forward-only test. So we drive the full ladder:
//!
//!   open burndown → `z` zoom → Esc (unzoom, STAY on burndown) →
//!   Esc (close to home).
//!
//! The load-bearing assertion is the middle one: after the first Esc the
//! zoom breadcrumb is gone but `Usage Analytics` is still on screen — Esc
//! popped one level without leaving the panel. The second Esc, now at the
//! root, closes to the origin.
//!
//! Requires staged plugins (`scripts/build-plugins.sh`) + tmux; skips
//! gracefully otherwise.

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
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");

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
        thread::sleep(Duration::from_millis(500));
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

#[test]
fn esc_pops_zoom_then_closes_to_origin() {
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

    let session = format!("tripwire-esc-ladder-{}", std::process::id());
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
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    // Home → burndown (with real data so the zoom panels have content).
    if poll_capture(&session, Instant::now() + Duration::from_secs(45), |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }
    send_key(&session, "i");
    if poll_capture(&session, Instant::now() + Duration::from_secs(45), |c| {
        c.contains("Usage Analytics") && !c.contains("Waiting for session-reader plugin")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("burndown never rendered after `i`; last:\n---\n{last}\n---");
    }

    // Zoom in. The zoom breadcrumb `[ Zoomed: <panel> ]` is the marker.
    send_key(&session, "z");
    if poll_capture(&session, Instant::now() + Duration::from_secs(15), |c| {
        c.contains("Zoomed:")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`z` did not zoom the burndown view; last:\n---\n{last}\n---");
    }

    // First Esc: pop ONE level — the zoom closes but we STAY on burndown.
    // This is the assertion that distinguishes "Esc pops one level" from
    // the old "Esc closes immediately": `Usage Analytics` must still be
    // on screen while the `Zoomed:` breadcrumb is gone.
    send_key(&session, "Escape");
    let unzoomed = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("Usage Analytics") && !c.contains("Zoomed:")
    });
    if unzoomed.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "first Esc did not pop the zoom while staying on burndown \
             (expected `Usage Analytics` present, `Zoomed:` gone); last:\n---\n{last}\n---"
        );
    }

    // Second Esc: now at the burndown root, Esc closes the panel back to
    // the origin (home, since we opened it from the home menu).
    send_key(&session, "Escape");
    let back_home = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("Stats") && c.contains("[i]") && !c.contains("Usage Analytics")
    });
    let final_cap = capture_pane(&session);
    kill_session(&session);
    assert!(
        back_home.is_some(),
        "root Esc did not close burndown back to home within 10s. \
         Final capture:\n---\n{final_cap}\n---"
    );
}
