//! Tripwire: Esc on the loaded burndown screen pops back to home.
//!
//! User-visible regression we're locking down: when the burndown
//! plugin is fully rendered, pressing `Esc` MUST return the user to
//! the home screen (sidebar + welcome panel). Before the fix in
//! `is_host_reserved_key`, every keystroke — including `Esc` — was
//! forwarded to the plugin via the one-way `plugin/handle_key`
//! notification. The plugin's Esc handler popped filter chips or
//! closed zoom, but when there was nothing to pop it silently
//! swallowed the key, leaving the user stuck on the analytics screen.
//!
//! The fix: Esc is now host-reserved (see
//! `crates/ainb-core/src/app/screens/builtin.rs::is_host_reserved_key`)
//! so it bypasses the plugin forwarder and routes through the central
//! key dispatch to `GoToHomeScreen`. The plugin's internal pop-state
//! semantics moved to `Backspace` (see `tripwire_burndown_keys.rs`).
//!
//! Skips gracefully if `tmux` isn't on `$PATH` or `dist/plugins/` isn't
//! staged — mirrors the gate pattern in
//! `tripwire_burndown_keys.rs`/`tripwire_real_data_in_tui.rs`.

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
            && candidate
                .join("session-reader")
                .join("session-reader")
                .exists()
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
        let dst = home.join(".claude").join("projects");
        copy_dir_all(&claude_src, &dst);
    }
    let codex_src = fixture.join("codex").join("sessions");
    if codex_src.is_dir() {
        let dst = home.join(".codex").join("sessions");
        copy_dir_all(&codex_src, &dst);
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
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

#[test]
fn esc_on_burndown_returns_to_home() {
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

    let session = format!("tripwire-esc-home-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "200",
            "-y",
            "50",
        ])
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

    // Wait for HomeScreen — sidebar + Stats entry visible.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre_home = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    if pre_home.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // Pre-press negative assertion: we are NOT on burndown yet.
    let pre_cap = capture_pane(&session);
    assert!(
        !pre_cap.contains("Usage Analytics"),
        "before pressing `i`, burndown chrome must not be visible"
    );

    // Open burndown.
    send_key(&session, "i");
    let burndown_deadline = Instant::now() + Duration::from_secs(45);
    let on_burndown = poll_capture(&session, burndown_deadline, |c| {
        c.contains("Usage Analytics")
            && !c.contains("Waiting for session-reader plugin")
            && c.contains('$')
    });
    let Some(burndown_cap) = on_burndown else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "burndown never rendered real data after `i`; last:\n---\n{last}\n---"
        );
    };

    // Sanity-check we're actually on burndown.
    assert!(
        burndown_cap.contains("Usage Analytics"),
        "burndown render missing `Usage Analytics`:\n---\n{burndown_cap}\n---"
    );

    // Press Esc. Wait for home chrome to reappear. This is the
    // assertion that locks the fix: before host-reserving Esc, the
    // plugin silently swallowed it and the user stayed on burndown.
    send_key(&session, "Escape");
    let back_home_deadline = Instant::now() + Duration::from_secs(10);
    let back_home = poll_capture(&session, back_home_deadline, |c| {
        // Home chrome: sidebar with Stats entry visible AND the
        // burndown's "Usage Analytics" title gone. Either alone is
        // ambiguous (Stats[i] persists in some intermediate states);
        // both together prove the navigation completed.
        c.contains("Stats") && c.contains("[i]") && !c.contains("Usage Analytics")
    });

    let final_cap = capture_pane(&session);
    kill_session(&session);

    assert!(
        back_home.is_some(),
        "Esc on burndown did not return to home within 10s. \
         Final capture:\n---\n{final_cap}\n---"
    );
}
