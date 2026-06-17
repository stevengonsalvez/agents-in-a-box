//! Tripwire: Codex usage quota renders on the TUI top bar (status line).
//!
//! Proves the P1→P2→P4 chain end-to-end in a real terminal:
//!   - a fresh `codex-live.json` in the isolated cache dir is read by
//!     `live_window::current()` (P1 overlay),
//!   - rendered as `cx5h`/`cxwk` segments on the session-list status bar
//!     (P2), reachable because the watcher refreshes the snapshot (P4),
//!   - and ABSENT when there is no Codex cache (hide-on-fail).
//!
//! The seeded cache carries a fresh `updated_at`, which trips the codex
//! pull throttle (~60s) so the in-TUI poller — running against an isolated
//! HOME with no `~/.codex/auth.json` — skips its network attempt and does
//! NOT clobber the seed during the test window.
//!
//! Navigation mirrors tripwire_sessions_screen: `s` on the HomeScreen
//! fires `AppEvent::GoToSessionList`; the status bar (with the live widget)
//! only renders on the non-registry SESSION_LIST view.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
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

fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).unwrap();
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{}"
skipped_dependencies = []
git_directories = []
"#,
        env!("CARGO_PKG_VERSION")
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).unwrap();
    // Suppress the ainb-hooks first-run install dialog — it overlays the
    // home screen and swallows the `s` navigation keystroke.
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .unwrap();
}

/// Write a fresh `codex-live.json` into the isolated cache dir (macOS:
/// `~/Library/Caches/ainb`, Linux: `~/.cache/ainb`). `updated_at` is set
/// to now so the reader treats it as fresh AND the poller throttle skips a
/// clobbering write.
fn seed_codex_cache(home: &Path, five_pct: u8, week_pct: u8) {
    let cache_dir = if cfg!(target_os = "macos") {
        home.join("Library").join("Caches").join("ainb")
    } else {
        home.join(".cache").join("ainb")
    };
    fs::create_dir_all(&cache_dir).unwrap();
    let now = chrono::Utc::now();
    let updated = now.to_rfc3339();
    let reset_5h = (now + chrono::Duration::hours(3)).to_rfc3339();
    let reset_wk = (now + chrono::Duration::days(4)).to_rfc3339();
    let json = format!(
        r#"{{
  "version": 1,
  "updated_at": "{updated}",
  "five_hour": {{ "pct": {five_pct}, "resets_at": "{reset_5h}" }},
  "seven_day": {{ "pct": {week_pct}, "resets_at": "{reset_wk}" }},
  "plan_type": "prolite"
}}"#
    );
    fs::write(cache_dir.join("codex-live.json"), json).unwrap();
}

fn is_on_sessions_screen(c: &str) -> bool {
    c.contains("Session Details")
        || c.contains("Select a session to view details")
        || (c.contains("attach") && c.contains("restart") && c.contains("cleanup"))
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("capture");
    String::from_utf8_lossy(&out.stdout).to_string()
}

struct TmuxSessionGuard {
    name: String,
}

impl Drop for TmuxSessionGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

fn poll<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let c = capture(session);
        if ok(&c) {
            return Some(c);
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Launch the TUI against an isolated HOME and navigate Home → SessionList.
/// Returns the tmux session name (guarded by `_guard`, which the caller
/// must keep alive).
fn launch_and_goto_sessions(home: &Path, session: &str) {
    let ainb = ainb_bin();
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "180", "-y", "50"])
        .status()
        .expect("new-session");
    assert!(status.success());

    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("launch");

    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre = poll(session, home_deadline, |c| {
        c.contains("Sessions") && c.contains("[s]")
    });
    assert!(
        pre.is_some(),
        "HomeScreen never rendered; last:\n{}",
        capture(session)
    );

    Command::new("tmux")
        .args(["send-keys", "-t", session, "s"])
        .status()
        .expect("send s");

    let nav_deadline = Instant::now() + Duration::from_secs(20);
    let post = poll(session, nav_deadline, is_on_sessions_screen);
    assert!(
        post.is_some(),
        "sessions screen never rendered after 's'.\n{}",
        capture(session)
    );
}

#[test]
fn tui_top_bar_shows_codex_quota_when_cache_present() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_codex_cache(home_tmp.path(), 24, 50);

    let session = format!("tripwire-codex-{}", std::process::id());
    let _guard = TmuxSessionGuard {
        name: session.clone(),
    };

    launch_and_goto_sessions(home_tmp.path(), &session);

    // The watcher refreshes the snapshot every 5s; give it a few ticks to
    // overlay the seeded codex cache and the status bar to repaint. The
    // rendered cluster is `codex 5h 24% ↻ … · wk 50% ↻ …`.
    let deadline = Instant::now() + Duration::from_secs(20);
    let shown = poll(&session, deadline, |c| {
        c.contains("codex 5h") && c.contains("50%")
    });

    let final_cap = shown.unwrap_or_else(|| capture(&session));
    assert!(
        final_cap.contains("codex 5h"),
        "codex cluster (codex 5h) never rendered on the top bar.\n{final_cap}"
    );
    assert!(
        final_cap.contains("wk"),
        "codex weekly window (wk) not rendered.\n{final_cap}"
    );
    // The seeded percentages must be visible too.
    assert!(
        final_cap.contains("24%"),
        "codex 5h percentage (24%) not visible.\n{final_cap}"
    );
    assert!(
        final_cap.contains("50%"),
        "codex weekly percentage (50%) not visible.\n{final_cap}"
    );
    // And the critical reset date/time stamp is present.
    assert!(
        final_cap.contains('↻'),
        "codex reset stamp (↻) not visible.\n{final_cap}"
    );
}

#[test]
fn tui_top_bar_hides_codex_when_no_cache() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    // Deliberately NO codex cache seeded — and the isolated HOME has no
    // ~/.codex/auth.json, so the in-TUI poller's pull fails (hide-on-fail).

    let session = format!("tripwire-codex-none-{}", std::process::id());
    let _guard = TmuxSessionGuard {
        name: session.clone(),
    };

    launch_and_goto_sessions(home_tmp.path(), &session);

    // Give the watcher several ticks; the codex segment must never appear.
    thread::sleep(Duration::from_secs(12));
    let cap = capture(&session);
    assert!(
        is_on_sessions_screen(&cap),
        "expected to still be on sessions screen.\n{cap}"
    );
    assert!(
        !cap.contains("codex 5h"),
        "codex segment rendered despite no cache / no auth (hide-on-fail broken).\n{cap}"
    );
}
