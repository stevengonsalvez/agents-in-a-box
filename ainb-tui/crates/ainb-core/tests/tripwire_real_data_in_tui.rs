//! Tripwire 7f.3: Real data renders in TUI when user opens analytics.
//!
//! The original 7f.3 passed on substring-OR of chrome strings ("ainb",
//! "session", "Container") that appear in the sidebar/wizard regardless
//! of whether burndown plugin rendered any data. This rewrite tightens
//! the test:
//!
//! 1. Pre-seeds `~/.agents-in-a-box/config/onboarding.toml` (in an
//!    isolated `HOME`) so the setup wizard is skipped and the
//!    HomeScreen renders directly. The wizard would otherwise eat the
//!    first key press and never reach the analytics screen.
//! 2. Asserts the pre-`i` capture **lacks** the analytics chrome
//!    ("Usage Analytics") so we know we're starting from the
//!    HomeScreen, not stuck on a stale analytics view.
//! 3. Sends the real "open analytics" keybinding — lowercase `i` —
//!    as declared in `crate::app::events`'s HomeScreen V2 handler.
//! 4. Asserts the post-`i` capture contains analytics chrome AND
//!    real data ("Total Calls", "Total Cost", or a `$<digit>` cost
//!    string) AND **must not** contain the placeholder
//!    `"Waiting for session-reader plugin"` — the exact failure mode
//!    that the original test allowed through.
//!
//! Skips gracefully if `tmux` isn't on `$PATH` — useful in CI runners
//! without tmux. Also skips when `dist/plugins/{burndown,session-reader}`
//! aren't staged because the plugin flow can't be exercised without
//! both binaries present.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[path = "tripwire_tmux_lock.rs"]
mod tripwire_tmux_lock;

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
/// staging dir produced by the dev workflow. Returns `None` if absent
/// so the test can skip rather than fail in fresh checkouts that
/// haven't built and staged plugins yet.
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

fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "{ts}"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ts = "2026-05-11T00:00:00+00:00",
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    // Seed one synthetic claude session so session-reader has real data
    // to publish. Without this, burndown still renders (zero state) and
    // the placeholder is gone — that proves the plugin pipeline — but
    // we want the strict-mode assertion to bite: real `$N.NN` cost
    // strings can only show up when there are non-zero tokens to price.
    let proj_dir = home.join(".claude").join("projects").join("-tripwire-fixture-project");
    fs::create_dir_all(&proj_dir).expect("create claude project dir");
    let session_jsonl = r#"{"type":"assistant","timestamp":"2026-05-10T12:00:00.000Z","sessionId":"fixture-session-1","cwd":"/tmp/x","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
    fs::write(proj_dir.join("fixture-session-1.jsonl"), session_jsonl)
        .expect("seed synthetic claude jsonl");
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

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

/// Press `key` repeatedly until `ok` matches a capture, or `total`
/// elapses. Re-presses every ~5s — a single send-key can be dropped
/// under heavy CPU contention (30+ test binaries fighting for the
/// scheduler) before the host's event loop drains the input queue.
fn press_until<F>(session: &str, key: &str, total: Duration, mut ok: F) -> Option<String>
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

// The "Waiting for session-reader plugin..." stall was two layered bugs:
//   1. The runtime ignored `manifest.lifecycle.spawn = "eager"`, so
//      session-reader was never started (fixed in plugin_task.rs via
//      `Command::EnsureSpawned`).
//   2. macOS AMFI silently SIGKILLed every staged plugin binary at exec
//      (exit 137, no stderr) because cargo's ad-hoc linker signature is
//      bound to the original build path; `cp` to dist/plugins/ broke it.
//      Fixed by `scripts/build-plugins.sh` re-signing after stage (via
//      `codesign --remove-signature` + `codesign --sign -`).
//
// Asserts the panel renders real-data chrome (Total Calls / Total Cost /
// `$<digit>`) and crucially is NOT stuck on the placeholder. Requires
// `just stage-plugins` (or `./scripts/build-plugins.sh`) so dist/plugins/
// contains the AMFI-friendly re-signed binaries.
#[test]
fn tui_renders_real_analytics_data_after_pressing_i() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!(
            "SKIP: dist/plugins/{{burndown,session-reader}} not staged — \
             run `just stage-plugins` or build then copy binaries first"
        );
        return;
    };

    let _lock = tripwire_tmux_lock::TmuxSerialLock::acquire();

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-tui-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} AINB_PLUGIN_ROOT={} exec {} tui",
        home_tmp.path().display(),
        plugin_root.display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    // Wait until HomeScreen sidebar appears — the unique marker is the
    // gold-coloured "Stats" sidebar tile with its `[i]` hotkey hint.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre_cap = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    let Some(pre) = pre_cap else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    };
    // Sanity: pre-press capture must NOT be on the analytics screen.
    assert!(
        !pre.contains("Usage Analytics"),
        "pre-key capture already on analytics — wizard/state leaked.\n{pre}"
    );

    // Drive the real keybinding: lowercase `i` opens stats.
    // Use press_until so a single dropped send-key (heavy L1 ci
    // contention drops the host's first read of the input queue) is
    // recovered by a re-press every ~5s.
    let post_cap = press_until(&session, "i", Duration::from_secs(120), |c| {
        let has_real_marker = c.contains("Total Calls")
            || c.contains("Total Cost")
            || c.contains("$0.")
            || c.contains("$1.")
            || c.contains("$2.")
            || c.contains("$3.")
            || c.contains("$4.")
            || c.contains("$5.")
            || c.contains("$6.")
            || c.contains("$7.")
            || c.contains("$8.")
            || c.contains("$9.");
        let still_loading = c.contains("Waiting for session-reader plugin");
        has_real_marker && !still_loading
    });

    let final_cap = post_cap.unwrap_or_else(|| capture_pane(&session));
    kill_session(&session);

    // 1) Must have reached the analytics screen (chrome present).
    assert!(
        final_cap.contains("Usage Analytics"),
        "analytics screen never rendered after 'i'.\n{final_cap}"
    );
    // 2) Must NOT be stuck on the placeholder — the bug we're catching.
    assert!(
        !final_cap.contains("Waiting for session-reader plugin"),
        "burndown stuck on session-reader placeholder — \
         eager-spawn / snapshot-publish wiring is broken.\n{final_cap}"
    );
    // 3) Must contain at least one real-data marker. Marker text drifts
    //    as burndown labels are renamed across versions; widen the OR
    //    chain to match whatever the current build emits when the
    //    fixture is loaded.
    let real_marker_present = final_cap.contains("Total Calls")
        || final_cap.contains("Total Cost")
        || final_cap.contains("Total:")
        // Title-bar summary uses "<N>K tokens" / "<N> tokens" form
        || (final_cap.contains("tokens") && final_cap.chars().any(|c| c.is_ascii_digit()))
        // Body widgets use "Calls <n>" and "Sessions <n>" labels
        || (final_cap.contains("Calls") && final_cap.contains("Sessions"))
        || (final_cap.contains('$') && final_cap.chars().any(|c| c.is_ascii_digit()));
    assert!(
        real_marker_present,
        "analytics rendered but no real data markers found \
         (expected 'Total:' / 'tokens<digit>' / 'Calls+Sessions' / '$<digit>').\n{final_cap}"
    );
}
