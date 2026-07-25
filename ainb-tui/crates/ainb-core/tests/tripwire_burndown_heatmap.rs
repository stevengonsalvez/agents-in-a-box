//! Tripwire: the Activity tab's contribution heatmap renders in the real TUI.
//!
//! `cargo test` proves the grid math (`heatmap.rs` has its own unit tests). It
//! cannot prove the tab is reachable, that the plugin's key wire delivers `]`,
//! `M` and the arrows, or that the grid actually paints — the Activity tab
//! replaced the Daily and Weekly tabs, so a mistake in the tab enum or the
//! render dispatch would leave a blank panel that no unit test would notice.
//!
//! Assertions pair a POSITIVE marker (grid chrome that only the heatmap draws)
//! with a NEGATIVE placeholder check, per the tripwire rules — asserting on
//! chrome like "Usage Analytics" alone would pass with an empty panel.
//!
//! Runs against the deterministic `tripwire_keys` fixture with `AINB_NOW`
//! pinned, so the grid's "today" column and the detail strip's date are stable
//! across machines. Skips gracefully without tmux or staged plugins.

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

    // Suppress the ainb-hooks first-run dialog; it overlays home and eats `i`.
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
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

/// Send a key once, then wait for the frame to change and settle.
///
/// Send-once matters here: `M` cycles the metric, so re-pressing it on every
/// poll iteration would keep rotating past the state under assertion.
fn send_key_and_settle(session: &str, key: &str) -> String {
    let before = capture_pane(session);
    send_key(session, key);
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut last = before.clone();
    let mut changed = false;
    let mut stable_since: Option<Instant> = None;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(150));
        let cur = capture_pane(session);
        if cur != before {
            changed = true;
        }
        if cur == last {
            if changed
                && stable_since.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(350)
            {
                return cur;
            }
        } else {
            last = cur;
            stable_since = None;
        }
    }
    last
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

/// The grid is present only when its own chrome is: the weekday gutter, the
/// legend ends, and at least one cell glyph. None of these appear on any other
/// burndown tab, so this cannot pass on a blank or wrong panel.
fn heatmap_rendered(cap: &str) -> bool {
    cap.contains("Mon")
        && cap.contains("Wed")
        && cap.contains("Fri")
        && cap.contains("Less")
        && cap.contains("More")
        && cap.chars().any(|c| matches!(c, '░' | '▒' | '▓' | '█' | '·'))
}

#[test]
fn activity_tab_renders_heatmap_and_responds_to_keys() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins not staged — run `scripts/build-plugins.sh`");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_fixture_home(home_tmp.path());

    let session = format!("tripwire-heatmap-{}", std::process::id());
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
        .expect("tmux send launch cmd");

    let home_deadline = Instant::now() + Duration::from_secs(90);
    if poll_capture(&session, home_deadline, |c| c.contains("Stats") && c.contains("[i]")).is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    send_key(&session, "i");
    let data_deadline = Instant::now() + Duration::from_secs(90);
    let Some(burndown) = poll_capture(&session, data_deadline, |c| {
        c.contains("Usage Analytics")
            && !c.contains("Waiting for session-reader plugin")
            && c.contains('$')
    }) else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("burndown never rendered real data; last:\n---\n{last}\n---");
    };

    // Pre-press negative: we must NOT already be looking at the heatmap, or
    // the post-press assertion proves nothing.
    assert!(
        !heatmap_rendered(&burndown),
        "expected to start on Burndown, not Activity:\n---\n{burndown}\n---"
    );
    assert!(
        burndown.contains("Activity"),
        "Activity tab missing from the tab strip:\n---\n{burndown}\n---"
    );

    // `]` walks Burndown → Activity.
    let activity = send_key_and_settle(&session, "]");
    if !heatmap_rendered(&activity) {
        kill_session(&session);
        panic!("Activity tab did not render the heatmap grid:\n---\n{activity}\n---");
    }
    // `[cost]` — brackets mark the active metric, so this asserts which metric
    // is selected rather than merely that the word "cost" appears somewhere.
    assert!(
        activity.contains("Metric:") && activity.contains("[cost]"),
        "metric selector missing or not defaulting to cost:\n---\n{activity}\n---"
    );
    // The detail strip carries the selected day's real figures, which is where
    // the exact numbers went when Daily was removed.
    assert!(
        activity.contains("tokens") && activity.contains("calls"),
        "per-day detail strip missing:\n---\n{activity}\n---"
    );

    // `M` cycles the metric — cost → tokens. Send once (it's a cycle, not a
    // toggle to re-assert).
    let after_metric = send_key_and_settle(&session, "M");
    assert!(
        after_metric != activity,
        "M did not change the render:\n---\n{after_metric}\n---"
    );
    assert!(
        heatmap_rendered(&after_metric),
        "grid vanished after switching metric:\n---\n{after_metric}\n---"
    );
    assert!(
        after_metric.contains("[tokens]") && !after_metric.contains("[cost]"),
        "M did not advance the selected metric cost -> tokens:\n---\n{after_metric}\n---"
    );

    // Arrow moves the day cursor, which retitles the detail strip.
    let after_left = send_key_and_settle(&session, "Left");
    assert!(
        heatmap_rendered(&after_left),
        "grid vanished after cursor move:\n---\n{after_left}\n---"
    );
    assert!(
        after_left != after_metric,
        "Left did not move the heatmap cursor:\n---\n{after_left}\n---"
    );

    // Return path: `[` walks back to Burndown and the grid goes away. Tab
    // navigation that only works forwards is exactly how the Esc-swallow bug
    // shipped past four tripwires.
    let back = send_key_and_settle(&session, "[");
    kill_session(&session);
    assert!(
        !heatmap_rendered(&back),
        "`[` did not leave the Activity tab:\n---\n{back}\n---"
    );
    assert!(
        back.contains("Usage Analytics"),
        "returning from Activity left the burndown screen entirely:\n---\n{back}\n---"
    );
}
