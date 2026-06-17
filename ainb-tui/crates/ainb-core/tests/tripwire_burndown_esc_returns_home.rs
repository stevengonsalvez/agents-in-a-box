//! Tripwire: Esc on the loaded burndown screen returns to its origin.
//!
//! User-visible contract we're locking down: when the burndown plugin
//! is fully rendered at its ROOT view (no zoom/overlay/chips), pressing
//! `Esc` MUST close the panel back to the screen it was opened from —
//! home when opened via the home sidebar (`i`), the session list when
//! opened from the session list (`i` mirrors there too).
//!
//! Mechanism (overlay-panels redesign): Esc is forwarded to the plugin
//! (`is_host_reserved_key` no longer reserves it). The plugin pops one
//! internal level per press; at the root it publishes
//! `ui.close_request` on the snapshot bus, and the host's
//! `tick_panel_close_requests` poll navigates back to the saved
//! `previous_screen`. So this tripwire exercises the full round trip:
//! key forward → root-Esc detection → publish → host poll → nav.
//! The earlier silent-swallow failure mode (one-way `plugin/handle_key`
//! with no close signal left the user stuck on analytics) is exactly
//! what these assertions would catch.
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

    // Suppress the ainb-hooks first-run install dialog — it overlays the
    // home screen and swallows the `i` keystroke this tripwire sends.
    // `prompt_dismissed` mirrors the user's "Don't ask again" choice
    // (see `ainb_plugin_notifyd::dismiss_prompt`).
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");

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
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
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

    // Wait for HomeScreen — sidebar + Stats entry visible.
    let home_deadline = Instant::now() + Duration::from_secs(90);
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
    let burndown_deadline = Instant::now() + Duration::from_secs(90);
    let on_burndown = poll_capture(&session, burndown_deadline, |c| {
        c.contains("Usage Analytics")
            && !c.contains("Waiting for session-reader plugin")
            && c.contains('$')
    });
    let Some(burndown_cap) = on_burndown else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("burndown never rendered real data after `i`; last:\n---\n{last}\n---");
    };

    // Sanity-check we're actually on burndown.
    assert!(
        burndown_cap.contains("Usage Analytics"),
        "burndown render missing `Usage Analytics`:\n---\n{burndown_cap}\n---"
    );

    // Press Esc. Wait for home chrome to reappear. This locks the full
    // close round trip: the host forwards Esc to the plugin, the
    // plugin (at its root view) publishes `ui.close_request`, and the
    // host's poll pops back to the origin screen — home here, because
    // burndown was opened from the home sidebar. A regression anywhere
    // along that chain leaves the user stuck on the analytics screen.
    send_key(&session, "Escape");
    let back_home_deadline = Instant::now() + Duration::from_secs(25);
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
        "Esc on burndown did not return to home within 25s. \
         Final capture:\n---\n{final_cap}\n---"
    );
}

/// Same close round trip, but with the panel opened FROM THE SESSION
/// LIST (`s` → `i`). Esc must return to the session list — not home —
/// proving the host pops the saved `previous_screen` rather than
/// hardcoding a destination. This is the core overlay-panels contract:
/// panels return to wherever they were opened from.
#[test]
fn esc_on_burndown_returns_to_session_list_when_opened_there() {
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

    let session = format!("tripwire-esc-sessions-{}", std::process::id());
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

    // Wait for HomeScreen, then hop to the session list.
    let home_deadline = Instant::now() + Duration::from_secs(90);
    if poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }
    send_key(&session, "s");

    // Session-list chrome: the four-line menu legend is unique to this
    // screen — `del-sel` only appears there.
    let sessions_deadline = Instant::now() + Duration::from_secs(40);
    if poll_capture(&session, sessions_deadline, |c| c.contains("del-sel")).is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("session list never rendered after `s`; last:\n---\n{last}\n---");
    }

    // Open burndown from the session list.
    send_key(&session, "i");
    let burndown_deadline = Instant::now() + Duration::from_secs(90);
    if poll_capture(&session, burndown_deadline, |c| {
        c.contains("Usage Analytics")
            && !c.contains("Waiting for session-reader plugin")
            && c.contains('$')
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("burndown never rendered real data after `i`; last:\n---\n{last}\n---");
    }

    // Esc at the burndown root must land back on the SESSION LIST.
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(25);
    let back_on_sessions = poll_capture(&session, back_deadline, |c| {
        c.contains("del-sel") && !c.contains("Usage Analytics")
    });

    let final_cap = capture_pane(&session);
    kill_session(&session);

    assert!(
        back_on_sessions.is_some(),
        "Esc on burndown (opened from session list) did not return to the \
         session list within 25s. Final capture:\n---\n{final_cap}\n---"
    );
}
