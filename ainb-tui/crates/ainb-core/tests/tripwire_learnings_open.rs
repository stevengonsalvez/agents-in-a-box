//! Tripwire (TDD plan §P3): the `learnings` plugin screen opens and
//! paints its title token.
//!
//! This is the PRIMARY user-visible gate for P3 (the design is TUI-only —
//! no CLI parity leg). It drives the real `ainb` binary in a detached
//! tmux session, presses the global `m` shortcut to open the learnings
//! screen, and asserts the EXACT unique title token `🧠 Learnings`
//! renders end-to-end:
//!
//!   tmux send-keys `m` → host GoToLearnings → current_screen = learnings
//!     → tick_plugin_renders spawns + renders the learnings plugin →
//!     JSON-RPC `plugin/render` → LearningsPlugin::render → WireBuffer →
//!     PluginScreen paints cells → tmux pane.
//!
//! Per the tmux-ui-tripwire skill:
//! - Skips gracefully if tmux isn't on PATH or `dist/plugins/learnings`
//!   isn't staged (re-signed for macOS AMFI — `scripts/build-plugins.sh`).
//! - Seeds an isolated HOME with `onboarding.toml` so the first-run wizard
//!   doesn't eat the `m` keystroke.
//! - Asserts an EXACT unique token (`🧠 Learnings`), never a substring-OR.
//! - Pairs the forward open with a return-path assertion (Esc → home),
//!   because `plugin/handle_key` is a one-way notification that can
//!   silently swallow keys (skill hard-rule 6).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The exact title token the learnings empty-state shell paints. Kept as
/// a literal here (not imported from the plugin crate, which ainb-core
/// doesn't depend on) so the assertion is an independent contract.
const TITLE_TOKEN: &str = "🧠 Learnings";

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

/// Walk up from the ainb binary looking for `dist/plugins/learnings/`
/// staged by `scripts/build-plugins.sh`. Returns `None` if absent so the
/// test skips rather than fails in fresh checkouts.
fn plugins_staged() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("learnings").join("learnings").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Seed an isolated HOME so the first keystroke lands on the actual
/// HomeScreen instead of being eaten by a modal:
/// - `~/.agents-in-a-box/config/onboarding.toml` skips the first-run wizard.
/// - A "don't ask again" notify-hooks record suppresses the
///   "Get notified when a session needs you?" confirmation dialog that
///   otherwise pops on startup and intercepts the `m` keystroke (it's a
///   modal that returns `None` for any unrelated key — see
///   `EventHandler::handle_key_event`'s confirmation-dialog branch).
fn seed_isolated_home(home: &Path) {
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

    // Suppress the notify-hooks install prompt (modal dialog) so the
    // HomeScreen is fully interactive on first paint.
    let paths = ainb_plugin_notifyd::Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create agents-in-a-box base");
    ainb_plugin_notifyd::dismiss_prompt(&paths).expect("seed dismissed notify prompt");
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
fn learnings_screen_opens_and_renders_title() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/learnings not staged — run `scripts/build-plugins.sh` first");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-learnings-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} AINB_PLUGIN_ROOT={} exec {} tui",
        home_tmp.path().display(),
        plugin_root.display(),
        ainb.display()
    );
    // cmd + Enter as a single tmux invocation (avoids the truncation race
    // documented in tripwire_burndown_keys.rs).
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
    // Pre-press negative assertion — confirm we're not already on the
    // learnings screen (the title token must NOT be on the home screen).
    let pre_home = pre_home.unwrap();
    assert!(
        !pre_home.contains(TITLE_TOKEN),
        "title token present on HomeScreen before opening learnings:\n---\n{pre_home}\n---"
    );

    // Open learnings via the global `m` ("memory") shortcut. Single-char
    // nav — NO Enter (skill hard-rule 3).
    send_key(&session, "m");

    // The plugin lazy-spawns on first render. Poll for the EXACT title
    // token — never a substring-OR (skill hard-rule 2).
    let open_deadline = Instant::now() + Duration::from_secs(45);
    let opened = poll_capture(&session, open_deadline, |c| c.contains(TITLE_TOKEN));
    let Some(opened_cap) = opened else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "learnings screen never painted the title token {TITLE_TOKEN:?}; last:\n---\n{last}\n---"
        );
    };

    // Return path (skill hard-rule 6): Esc is host-reserved and must
    // navigate back to home, NOT get swallowed by the plugin. Assert the
    // title token is gone and a HomeScreen marker is back.
    send_key(&session, "Escape");
    let home_again_deadline = Instant::now() + Duration::from_secs(15);
    let home_again = poll_capture(&session, home_again_deadline, |c| {
        !c.contains(TITLE_TOKEN) && c.contains("Stats")
    });

    kill_session(&session);

    assert!(
        opened_cap.contains(TITLE_TOKEN),
        "learnings title token {TITLE_TOKEN:?} did not render after pressing `m`:\n---\n{opened_cap}\n---"
    );
    assert!(
        home_again.is_some(),
        "Esc from learnings did not return to HomeScreen (title token swallowed the key); \
         last capture:\n---\n{}\n---",
        capture_pane(&session)
    );
}
