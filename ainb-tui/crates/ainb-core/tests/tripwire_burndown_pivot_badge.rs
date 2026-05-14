//! Tripwire: the `↻ updated` chip-strip badge actually flashes on a
//! cache-miss recompute and clears on the next render.
//!
//! Lives at the user-visible chrome layer because PRs E (dimension
//! indices) and F (Arc<UsageData>) made the chip pivot sub-millisecond
//! — fast enough that a regression to "pivot silently broken" wouldn't
//! produce a wall-clock pause. The badge is the *only* visible
//! confirmation that a chip key actually reached the plugin and the
//! recompute ran. Unit tests assert the `pivot_seq` invariant; this
//! test proves the whole render path from `cached_filtered`'s seq bump
//! through the chip-strip span builder reaches the tmux pane.
//!
//! Drives the same fixture as `tripwire_burndown_keys` so totals are
//! reproducible. Skips gracefully if `tmux` isn't installed or if
//! `dist/plugins/{burndown,session-reader}` isn't staged.

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

/// Send a key and wait for the capture to settle (no further byte
/// changes for two consecutive polls, or hard timeout).
fn send_key_and_settle(session: &str, key: &str) -> String {
    send_key(session, key);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = capture_pane(session);
    let mut stable_since: Option<Instant> = None;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
        let cur = capture_pane(session);
        if cur == last {
            match stable_since {
                None => stable_since = Some(Instant::now()),
                Some(t) if t.elapsed() >= Duration::from_millis(400) => return cur,
                _ => {}
            }
        } else {
            last = cur;
            stable_since = None;
        }
    }
    last
}

/// Send a key and poll the pane aggressively for up to `window` looking
/// for `marker`. Returns `Some(capture)` on the first frame containing
/// the marker, `None` if it never appeared. Used instead of
/// `send_key_and_settle` for transient render markers (like the
/// `↻ updated` badge) that can be overwritten by a subsequent render
/// before the settle predicate fires — async scan_progress / tick
/// events can race a settle loop and produce false negatives.
fn send_key_and_poll_for(session: &str, key: &str, marker: &str, window: Duration) -> Option<String> {
    send_key(session, key);
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if cap.contains(marker) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

/// Marker the plugin emits on the chip strip when `cached_filtered`
/// just recomputed. Kept as a const so any future rename surfaces a
/// single failing assertion instead of a string-mismatch puzzle.
const PIVOT_BADGE: &str = "↻ updated";

#[test]
fn fresh_pivot_badge_appears_on_recompute_and_clears_on_next_render() {
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

    let session = format!("tripwire-pivot-badge-{}", std::process::id());
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

    // Wait for HomeScreen.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre_home = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    if pre_home.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // Enter burndown. Wait for real data — the badge ride along with
    // the first render after the fixture's usage_data lands, so we
    // need the screen to be data-bearing before asserting on chip
    // strip state.
    send_key(&session, "i");
    let data_deadline = Instant::now() + Duration::from_secs(45);
    let initial = poll_capture(&session, data_deadline, |c| {
        c.contains("Usage Analytics")
            && !c.contains("Waiting for session-reader plugin")
            && c.contains('$')
    });
    let Some(initial) = initial else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "burndown never rendered data with the fixture HOME; last:\n---\n{last}\n---"
        );
    };

    // The first frame after initial ingest may legitimately bump
    // `pivot_seq` (the plugin's startup snapshot is a "fresh pivot" by
    // construction). Press a non-pivot key (Tab) to force a render
    // pass that hits the filter cache, and use THAT settled state as
    // the no-badge baseline. This makes the subsequent "press a
    // pivot key → badge re-appears" assertion robust against any
    // startup-frame ambiguity.
    let baseline = send_key_and_settle(&session, "Tab");
    assert!(
        !baseline.contains(PIVOT_BADGE),
        "after Tab (cache hit), badge must clear; got:\n---\n{baseline}\n---\n\
         (initial capture for reference:\n---\n{initial}\n---)"
    );

    // Press period chip `3` → `UsagePeriod::ThirtyDays`. The default
    // period is `Week`, so this is a real state delta:
    // `(filters, period, provider)` changes → cache miss → seq bumps
    // → render must include the badge on the FIRST frame after the
    // key is dispatched. Subsequent renders (tick / scan_progress /
    // any host kick) hit the cache and clear it — so we poll
    // aggressively rather than waiting for the pane to settle.
    //
    // NOTE on key selection: `2` maps to `Week` which IS the default,
    // so pressing `2` from a fresh state is a no-op (cache hit, no
    // bump, no badge). Always pick a key whose target period differs
    // from the current state.
    let post_pivot = send_key_and_poll_for(
        &session,
        "3",
        PIVOT_BADGE,
        Duration::from_secs(3),
    );
    if post_pivot.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "after pressing period chip `3` (cache miss from default Week → \
             ThirtyDays), `{PIVOT_BADGE}` badge never appeared within 3s; \
             last capture:\n---\n{last}\n---"
        );
    }

    // Press Tab — focus shift, no period/filter/provider change.
    // cached_filtered hits the cache, seq doesn't bump, badge clears.
    let after_tab = send_key_and_settle(&session, "Tab");
    assert!(
        !after_tab.contains(PIVOT_BADGE),
        "after pressing Tab (cache hit), badge must clear; got:\n---\n{after_tab}\n---"
    );

    // Press `1` (Today period). Differs from ThirtyDays → cache miss
    // → badge re-appears.
    //
    // NOTE on key selection: pressing `a` (All) here would NOT trigger
    // a badge — when every dimension returns to its default state
    // (no filters, period=All, provider=All), `cached_filtered`
    // takes its no-op fast path and never bumps `pivot_seq`. The
    // plugin uses the raw data reference in that case. That's
    // correct behavior — nothing was actually filtered — but it
    // makes `a` a poor choice for "second pivot must show badge".
    let post_second_pivot = send_key_and_poll_for(
        &session,
        "1",
        PIVOT_BADGE,
        Duration::from_secs(3),
    );
    if post_second_pivot.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "after pressing `1` (cache miss from ThirtyDays → Today), badge \
             never reappeared within 3s; last capture:\n---\n{last}\n---"
        );
    }

    kill_session(&session);
}
