//! Tripwire (TDD plan §P5): the `learnings` Browse tab renders the fixture
//! KB and the filter-chip bar.
//!
//! PRIMARY user-visible gate for P5 (TUI-only — no CLI parity leg). It drives
//! the real `ainb` binary in a detached tmux session, opens the learnings
//! screen (`m`), and asserts that — over the committed fixture KB
//! (`crates/ainb-plugin-learnings/tests/fixtures/kb`) — the Browse list paints
//! a deterministic fixture learning id, and pressing `f` surfaces the scope
//! filter chip with its active value.
//!
//! Setup mirrors `tripwire_learnings_open.rs` (P3):
//! - Skips if tmux isn't on PATH or `dist/plugins/learnings` isn't staged
//!   (re-signed for macOS AMFI — `scripts/build-plugins.sh`).
//! - Seeds an isolated HOME with `onboarding.toml` (skip first-run wizard) +
//!   a dismissed notify-install prompt (notifyd) so neither modal eats `m`.
//! - ALSO seeds `config.toml` with `[plugins.learnings].learnings_dir` →
//!   the fixture KB so the Browse list is deterministic.
//! - Asserts EXACT unique tokens, never a substring-OR.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// A fixture learning id the Browse list must render over the fixture KB.
/// Sorted-first by id, so it's stable at the top of the list.
const FIXTURE_ID: &str = "lrn-audit-after-rebase";

/// The scope filter chip with its active value after one `f` press. Exact,
/// unique token — never a substring-OR.
const SCOPE_CHIP_ACTIVE: &str = "scope[universal]";

/// The title token the learnings screen paints (P3 contract — re-asserted
/// here so a P5 refactor that drops the title is caught by this tripwire too).
const TITLE_TOKEN: &str = "🧠 Learnings";

/// A bottom help/key-hint bar token the Browse tab must paint (P5-review
/// finding: the f/Tab/↑↓ keys are functional but undiscoverable without a
/// footer). Exact, unique token — never a substring-OR. Asserted via a
/// WRONG-value-first run to read the real render before locking.
const HELP_BAR_TOKEN: &str = "f filter";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Absolute path to the committed learnings fixture KB.
fn fixture_kb_dir() -> Option<PathBuf> {
    // Walk up from the ainb binary to the workspace root, then into the
    // learnings crate's committed fixtures.
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir
            .join("crates")
            .join("ainb-plugin-learnings")
            .join("tests")
            .join("fixtures")
            .join("kb");
        if candidate.join("lrn-audit-after-rebase.md").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Walk up from the ainb binary looking for `dist/plugins/learnings/`.
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

/// Seed an isolated HOME so the first keystroke lands on the HomeScreen, and
/// point the learnings plugin's `learnings_dir` at the fixture KB.
fn seed_isolated_home(home: &Path, kb_dir: &Path) {
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

    // Point the learnings plugin at the committed fixture KB via the
    // per-plugin config table the host resolves + injects at plugin/init.
    let config_toml = format!(
        "[plugins.learnings]\nlearnings_dir = {dir:?}\n",
        dir = kb_dir.display().to_string(),
    );
    fs::write(cfg.join("config.toml"), config_toml).expect("seed config.toml");

    // Suppress the notify-hooks install prompt (modal) so the HomeScreen is
    // fully interactive on first paint.
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

/// Like [`poll_capture`], but re-sends `key` on each iteration until `ok`
/// holds. The single-shot `send_key` + poll pattern is racy for the
/// screen-open step: the global `m` shortcut can land while the `HomeScreen`
/// is still settling (or be eaten by a transient), so the screen never opens and
/// the test flakes. Re-pressing is safe because the open key is idempotent —
/// `m` while already on the learnings screen is a no-op nav, so extra presses
/// can't overshoot. The first send happens before the loop so a fast open
/// still returns on the first capture.
fn poll_capture_resending<F>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    send_key(session, key);
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
        send_key(session, key);
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
fn learnings_browse_lists_fixture_id_and_filter_chip() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/learnings not staged — run `scripts/build-plugins.sh` first");
        return;
    };
    let Some(kb_dir) = fixture_kb_dir() else {
        eprintln!("SKIP: fixture KB not found");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path(), &kb_dir);

    let session = format!("tripwire-learnings-browse-{}", std::process::id());
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

    // Open learnings via the global `m` shortcut (single-char nav — NO Enter).
    // The plugin lazy-spawns + scans the fixture KB on first render. Poll for
    // the EXACT fixture id row (never a substring-OR), re-pressing `m` each
    // iteration so a keystroke lost to a HomeScreen-settle race doesn't flake
    // the test (`m` is idempotent once the screen is open).
    let open_deadline = Instant::now() + Duration::from_secs(45);
    let opened = poll_capture_resending(&session, "m", open_deadline, |c| {
        c.contains(TITLE_TOKEN) && c.contains(FIXTURE_ID)
    });
    let Some(opened_cap) = opened else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "learnings Browse never painted the fixture id {FIXTURE_ID:?}; last:\n---\n{last}\n---"
        );
    };

    // Press `f`: the scope filter chip becomes active (`scope[universal]`).
    send_key(&session, "f");
    let chip_deadline = Instant::now() + Duration::from_secs(15);
    let chip = poll_capture(&session, chip_deadline, |c| c.contains(SCOPE_CHIP_ACTIVE));

    kill_session(&session);

    assert!(
        opened_cap.contains(FIXTURE_ID),
        "Browse list did not render fixture id {FIXTURE_ID:?}:\n---\n{opened_cap}\n---"
    );
    assert!(
        opened_cap.contains(HELP_BAR_TOKEN),
        "Browse tab did not render the bottom help-bar token {HELP_BAR_TOKEN:?} \
         (P5-review finding — f/Tab/↑↓ keys are undiscoverable without it):\n---\n{opened_cap}\n---"
    );
    assert!(
        chip.is_some(),
        "pressing `f` did not activate the scope filter chip {SCOPE_CHIP_ACTIVE:?}; \
         last capture:\n---\n{}\n---",
        capture_pane(&session)
    );
}
