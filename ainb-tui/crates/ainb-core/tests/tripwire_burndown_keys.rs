//! Tripwire (plan §Phase 8): the burndown plugin's interactive keys
//! actually affect what the user sees.
//!
//! Earlier tripwires asserted that the analytics screen renders *some*
//! data after the user presses `i`. That's necessary but not sufficient
//! — every period chip, provider toggle, Tab focus cycle, and Esc
//! short-circuit was silently broken before the plugin/handle_key wire
//! existed (plan §Phase 1-4). This test sends each key in turn and
//! asserts the resulting capture differs from the previous one, proving
//! the key reached the plugin's state machine end-to-end:
//!
//!   tmux send-keys → host PluginScreen::handle_key → JSON-RPC
//!     `plugin/handle_key` notification → Burndown::handle_key →
//!     UsageViewState mutation → re-render → ratatui paint → tmux pane.
//!
//! The test runs against the deterministic `tripwire_keys` fixture so
//! period totals are stable across CI machines (see plan §Phase 7 and
//! `parses_tripwire_keys_fixture.rs`). HOME is an isolated tempdir with
//! the fixture's claude+codex transcripts copied in; `AINB_NOW` is
//! pinned to `FIXTURE_NOW.txt` so the `Today`/`7d`/`30d` chips bucket
//! reproducibly.
//!
//! Skips gracefully if `tmux` isn't on `$PATH` or if dist/plugins isn't
//! staged with re-signed binaries (`scripts/build-plugins.sh` /
//! `just stage-plugins`). Both gates already exist on
//! `tripwire_real_data_in_tui.rs` and we mirror them here for
//! consistency.

#[path = "tripwire_tmux_lock.rs"]
mod tripwire_tmux_lock;

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

/// Recursively copy a directory tree. Used to clone the fixture's
/// claude/codex transcript layout into the isolated HOME.
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

/// Seed an isolated HOME with:
/// - `~/.agents-in-a-box/config/onboarding.toml` so the wizard skips
/// - `~/.claude/projects/...` from the fixture's claude tree
/// - `~/.codex/sessions/...` from the fixture's codex tree
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

/// Press `key` repeatedly until `ok` matches a capture, or `total`
/// elapses. Re-presses every ~5s — a single send-key can be dropped
/// under heavy CPU contention (30+ test binaries fighting for the
/// scheduler) before the host's event loop drains the input queue.
fn press_until<F>(
    session: &str,
    key: &str,
    total: Duration,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    let deadline = Instant::now() + total;
    let mut last_press = Instant::now();
    send_key(session, key);
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        if last_press.elapsed() > Duration::from_secs(5) {
            send_key(session, key);
            last_press = Instant::now();
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
/// changes for two consecutive polls, or hard timeout). Returns the
/// final capture. Settling beats fixed sleeps because plugin frames
/// arrive asynchronously over JSON-RPC and the prior frame's bytes can
/// linger in the capture for a brief window.
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

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

/// Extract the burndown chip strip's `p filter: X` token (`All`,
/// `Claude`, or `Codex`). Returns the empty string if the chip is
/// not present in the capture. Used as a stable, data-independent
/// signal for "did the provider filter actually change".
fn filter_token(cap: &str) -> String {
    for line in cap.lines() {
        if let Some(idx) = line.find("p filter:") {
            let rest = &line[idx + "p filter:".len()..];
            let token: String = rest
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !token.is_empty() {
                return token;
            }
        }
    }
    String::new()
}

/// Extract the cost-looking header substring (`Cost $NN.NN` or
/// `Total $NN.NN` or any `$<digits>.<digits>`) from a capture, joined
/// by spaces. Used as a stable signal for "did the rendered numbers
/// change between key presses".
fn cost_signal(cap: &str) -> String {
    let mut out = Vec::new();
    for line in cap.lines() {
        // Header line(s) on the burndown screen carry "Cost", "Total",
        // or a `$N` dollar amount. Capture each match verbatim — order
        // matters and is stable per render so equality comparisons
        // catch any state delta.
        for tok in line.split_whitespace() {
            if tok.starts_with('$') && tok.chars().skip(1).any(|c| c.is_ascii_digit()) {
                out.push(tok.to_string());
            }
        }
    }
    out.join(" ")
}

#[test]
fn burndown_interactive_keys_change_render() {
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

    let _lock = tripwire_tmux_lock::TmuxSerialLock::acquire();

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_fixture_home(home_tmp.path());

    let session = format!("tripwire-keys-{}", std::process::id());
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
    // Send cmd + Enter as a single tmux invocation. Splitting into two
    // calls races: a second send-keys can land before the first has
    // fully flushed its character sequence, which on some tmux builds
    // truncates the launch command. The working tripwire
    // (`tripwire_real_data_in_tui.rs`) uses the combined form, so we
    // mirror it.
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

    // Enter burndown. Wait for real data — fixture totals are non-zero
    // so a `$<digit>` token must appear inside 30s once session-reader
    // publishes the first usage_data chunk. press_until re-sends `i`
    // every ~5s in case the first keypress was dropped during CPU
    // contention.
    let initial = press_until(&session, "i", Duration::from_secs(90), |c| {
        c.contains("Usage Analytics")
            && !c.contains("Waiting for session-reader plugin")
            && c.contains('$')
            && c.chars().any(|ch| ch.is_ascii_digit())
    });
    let Some(baseline) = initial else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("burndown never rendered real data; last:\n---\n{last}\n---");
    };

    // Probe sequence. Each probe captures the post-press screen and
    // extracts a stable cost signal. The key is "did the rendered
    // dollar amounts change?" — a true byte-diff across the entire
    // capture is too noisy (mouse cursor, blinking indicators, etc.)
    // but the cost tokens are stable per render.
    let signal_today = {
        let cap = send_key_and_settle(&session, "1");
        cost_signal(&cap)
    };
    let signal_7d = {
        let cap = send_key_and_settle(&session, "2");
        cost_signal(&cap)
    };
    let signal_30d = {
        let cap = send_key_and_settle(&session, "3");
        cost_signal(&cap)
    };
    let signal_all = {
        let cap = send_key_and_settle(&session, "a");
        cost_signal(&cap)
    };

    // Provider switch via Right arrow. The cost chip in the top
    // summary bar is intentionally a global total (filter-independent),
    // so the only stable, render-visible delta is the chip strip's
    // `p filter:` token. Default is `All`, Right cycles to the next
    // provider's filter state. We assert that token shifts.
    let cap_pre_right = capture_pane(&session);
    let cap_post_right = send_key_and_settle(&session, "Right");
    let filter_pre_right = filter_token(&cap_pre_right);
    let filter_post_right = filter_token(&cap_post_right);
    // Restore provider to a known state so downstream assertions
    // operate on the same dataset. Left cycles backwards.
    let _ = send_key_and_settle(&session, "Left");

    // Tab cycles focused panel. capture-pane returns plain text so we
    // can't see the focus highlight directly, but the chip strip or
    // panel headers shift their position/title — easier to assert by
    // recording the full capture and demanding change.
    let cap_pre_tab = capture_pane(&session);
    let cap_post_tab = send_key_and_settle(&session, "Tab");

    // Zoom toggle. A zoomed view occupies the full body region, so the
    // bottom-half panel titles drop out vs. the grid view.
    let cap_pre_zoom = capture_pane(&session);
    let cap_post_zoom = send_key_and_settle(&session, "z");
    // Restore (toggle off) so we don't leave the plugin in a zoomed
    // state for downstream assertions.
    let _ = send_key_and_settle(&session, "z");

    // Backspace must be safe when no filter chip is set — a no-op
    // rather than crashing the plugin. We don't assert a delta; we
    // assert the burndown screen is still rendered after Backspace.
    // (`Esc` is host-reserved — see `is_host_reserved_key` — so the
    // navigation-back tripwire lives in its own test file
    // `tripwire_burndown_esc_returns_home.rs`.)
    let cap_post_backspace = send_key_and_settle(&session, "BSpace");

    kill_session(&session);

    let _ = baseline; // baseline establishment was the precondition

    // === Period switching ===
    // Fixture totals satisfy `today < 7d < 30d < all` for at least one
    // project, so the cost signal must change when periods shift.
    assert!(
        !signal_today.is_empty(),
        "Today render produced no cost signal — burndown didn't repaint after `1`"
    );
    assert!(
        !signal_all.is_empty(),
        "All render produced no cost signal — burndown didn't repaint after `a`"
    );
    assert_ne!(
        signal_today, signal_all,
        "pressing `a` (All) didn't change the dollar amounts vs `1` (Today) — \
         the period key never reached the plugin's state machine"
    );
    assert_ne!(
        signal_today, signal_7d,
        "pressing `2` (7d) didn't change the dollar amounts vs `1` (Today) — \
         the period key never reached the plugin's state machine"
    );
    assert_ne!(
        signal_7d, signal_30d,
        "pressing `3` (30d) didn't change the dollar amounts vs `2` (7d) — \
         the period key never reached the plugin's state machine"
    );

    // === Provider switch ===
    // The chip strip's `p filter: X` token reflects the active
    // provider filter and is the cleanest, least-coupled assertion.
    assert!(
        !filter_pre_right.is_empty() && !filter_post_right.is_empty(),
        "could not locate `p filter: X` chip in either capture — \
         the burndown chip strip layout may have changed:\n\
         pre:  {filter_pre_right:?}\n\
         post: {filter_post_right:?}"
    );
    assert_ne!(
        filter_pre_right, filter_post_right,
        "Right arrow didn't change the `p filter:` chip — \
         the provider switch never reached the plugin"
    );

    // === Tab focus ===
    assert_ne!(
        cap_pre_tab, cap_post_tab,
        "Tab key didn't change any rendered byte — \
         focus_next_panel never reached the plugin"
    );

    // === Zoom ===
    assert_ne!(
        cap_pre_zoom, cap_post_zoom,
        "`z` (zoom) didn't change the rendered layout — \
         toggle_zoom never reached the plugin"
    );

    // === Backspace safety ===
    assert!(
        cap_post_backspace.contains("Usage Analytics")
            && !cap_post_backspace.contains("Waiting for session-reader plugin"),
        "Backspace with no active filter broke the burndown render:\n---\n{cap_post_backspace}\n---"
    );
}
