//! Tripwire (TDD plan §P2): the Settings ▸ Plugins category renders a loaded
//! plugin's `[[config]]` schema as editable rows, and an edit persists to
//! `config.toml [plugins.<name>]`.
//!
//! PRIMARY user-visible gate for P2 (the per-plugin config framework is
//! TUI-only — no CLI parity leg). It drives the real `ainb` binary in a
//! detached tmux session and exercises the full loop end-to-end:
//!
//!   tmux `C` → host GoToConfig → Configuration screen
//!     → `j`×5 to the Plugins category (Categories pane)
//!     → the learnings `[[config]]` rows paint (built from the loaded
//!       manifest by `ConfigScreenState::apply_plugin_manifests`, seeded
//!       from the persisted `[plugins.learnings]` table)
//!     → `l` (focus Settings pane) → `j` to the learnings_dir row → `Enter`
//!       opens the text popup → clear + type a new value → `Enter` confirms
//!       (the cf7fe1e apply→AppConfig::save() hook persists to config.toml)
//!     → `Esc` home → re-open Settings ▸ Plugins → the NEW value renders
//!     → the seeded config.toml carries `[plugins.learnings]` + the new value.
//!
//! Per the tmux-ui-tripwire skill:
//! - Skips gracefully if tmux isn't on PATH or `dist/plugins/learnings` isn't
//!   staged (re-signed for macOS AMFI — `scripts/build-plugins.sh`): the
//!   learnings manifest must actually load for its `[[config]]` rows to show.
//! - Seeds an isolated HOME with `onboarding.toml` (skip first-run wizard) +
//!   a dismissed notify-install prompt (notifyd) so neither modal eats `C`.
//! - ALSO seeds `config.toml` with `[plugins.learnings].learnings_dir` so the
//!   pre-edit render is deterministic AND the file already has the section the
//!   final grep checks.
//! - Re-presses `C` each poll on the open step (the resend pattern) — the
//!   first `C` can land before the host's first paint and be dropped; `C` is
//!   idempotent once on the Config screen.
//! - Asserts EXACT unique tokens, never a substring-OR.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The learnings `[[config]]` field label the Plugins category must paint for
/// `learnings_dir` (verbatim from `crates/ainb-plugin-learnings/manifest.toml`).
/// Exact, unique token — never a substring-OR.
const FIELD_LABEL: &str = "Learnings notes directory";

/// The value the config.toml is seeded with (the pre-edit render). Short +
/// unique so it survives the 70%-width settings panel without truncation.
const SEED_VALUE: &str = "/tmp/AINB_SEED_KB";

/// The value the tripwire edits the row to. Short + unique so the re-render
/// assertion and the config.toml grep both key off a token that can't appear
/// anywhere else.
const EDITED_VALUE: &str = "/tmp/AINB_EDITED_KB";

/// The Configuration screen title token (host config_screen.rs) — proves we
/// reached Settings before navigating categories.
const CONFIG_TITLE: &str = "Configuration";

/// The Plugins category label rendered in the Categories pane.
const PLUGINS_CATEGORY: &str = "Plugins";

/// A HomeScreen marker (mirrors the other learnings tripwires).
const HOME_MARKER: &str = "Stats";

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

/// Walk up from the ainb binary looking for `dist/plugins/learnings/` staged
/// by `scripts/build-plugins.sh`. Returns `None` if absent so the test skips
/// rather than fails in fresh checkouts.
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

/// Seed an isolated HOME: skip the first-run wizard, dismiss the notify
/// prompt, and pre-write `[plugins.learnings].learnings_dir = SEED_VALUE` so
/// the Plugins category renders a deterministic value AND the config.toml the
/// final grep inspects already has the section.
fn seed_isolated_home(home: &Path) -> PathBuf {
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

    let config_path = cfg.join("config.toml");
    let config_toml = format!("[plugins.learnings]\nlearnings_dir = {SEED_VALUE:?}\n");
    fs::write(&config_path, config_toml).expect("seed config.toml");

    // Suppress the notify-hooks install prompt (modal) so the HomeScreen is
    // fully interactive on first paint.
    let paths = ainb_plugin_notifyd::Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create agents-in-a-box base");
    ainb_plugin_notifyd::dismiss_prompt(&paths).expect("seed dismissed notify prompt");

    config_path
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
/// holds. Used for the Settings-open step: the first `C` can land before the
/// host's first paint and be dropped; `C` is idempotent once on the Config
/// screen (a no-op nav).
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

/// Open Settings (`C`), then step the Categories pane down to the Plugins
/// category and assert the learnings `[[config]]` row renders. Returns the
/// capture once the Plugins category shows the field label.
fn open_plugins_category(session: &str) -> Option<String> {
    // Open Settings — resend `C` until the Configuration title paints.
    let open_deadline = Instant::now() + Duration::from_secs(30);
    poll_capture_resending(session, "C", open_deadline, |c| c.contains(CONFIG_TITLE))?;

    // Focus starts on the Categories pane. Step down to Plugins (index 5 of
    // ConfigCategory::all(): Authentication, Workspace, Docker, AgentDefaults,
    // Editor, Plugins, ...). Re-press `j` each poll until the learnings row is
    // visible — selecting the Plugins category swaps the Settings panel to the
    // plugin rows. `j` past Plugins is harmless (we poll on the field label,
    // not a fixed index), but 5 presses lands exactly on it.
    let cat_deadline = Instant::now() + Duration::from_secs(20);
    for _ in 0..5 {
        send_key(session, "j");
        thread::sleep(Duration::from_millis(150));
    }
    poll_capture(session, cat_deadline, |c| {
        c.contains(PLUGINS_CATEGORY) && c.contains(FIELD_LABEL)
    })
}

/// Edit the learnings_dir row from the Plugins category: focus the Settings
/// pane, move to the first plugin row (one Down past the `installed_plugins`
/// placeholder — learnings_dir is the first manifest field), open the text
/// popup, clear the seeded value, type [`EDITED_VALUE`], and confirm. The
/// confirm triggers the host's apply→`AppConfig::save()` hook (cf7fe1e), so
/// the value persists to config.toml without an explicit save-all.
fn edit_learnings_dir_value(session: &str) {
    send_key(session, "l"); // focus Settings pane
    thread::sleep(Duration::from_millis(200));
    send_key(session, "j"); // installed_plugins -> learnings_dir
    thread::sleep(Duration::from_millis(200));
    send_key(session, "Enter"); // open the text-input popup

    // Wait for the popup to mount (it pre-fills with the seeded value).
    let popup_deadline = Instant::now() + Duration::from_secs(15);
    if poll_capture(session, popup_deadline, |c| c.contains(SEED_VALUE)).is_none() {
        let last = capture_pane(session);
        let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
        panic!("edit popup for learnings_dir never mounted; last:\n---\n{last}\n---");
    }

    // Clear the pre-filled value (cursor sits at end) — over-backspace is safe
    // once the field is empty — then type the new value.
    for _ in 0..(SEED_VALUE.len() + 8) {
        send_key(session, "BSpace");
    }
    // Type the edited value as one literal string so tmux delivers each char
    // as a keystroke (the popup's input_char path).
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", EDITED_VALUE])
        .status()
        .expect("tmux type edited value");
    thread::sleep(Duration::from_millis(200));
    send_key(session, "Enter"); // confirm popup → apply_to_app_config + save()
}

#[test]
fn settings_plugins_category_renders_and_edit_persists() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/learnings not staged — run `scripts/build-plugins.sh` first");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let config_path = seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-config-plugins-{}", std::process::id());
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
    if poll_capture(&session, home_deadline, |c| {
        c.contains(HOME_MARKER) && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // 1. Open Settings ▸ Plugins; assert the learnings config row renders with
    //    its seeded value.
    let Some(plugins_cap) = open_plugins_category(&session) else {
        let last = capture_pane(&session);
        let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();
        panic!(
            "Settings ▸ Plugins never painted the learnings config row {FIELD_LABEL:?}; \
             last:\n---\n{last}\n---"
        );
    };
    assert!(
        plugins_cap.contains(SEED_VALUE),
        "Plugins category rendered the learnings row but not the seeded value {SEED_VALUE:?}; \
         capture:\n---\n{plugins_cap}\n---"
    );

    // 2. Edit the learnings_dir row in place; the confirm auto-saves (cf7fe1e).
    edit_learnings_dir_value(&session);

    // The edited value should re-render in the row (popup closed).
    let edited_deadline = Instant::now() + Duration::from_secs(15);
    let edited_cap = poll_capture(&session, edited_deadline, |c| {
        c.contains(EDITED_VALUE) && c.contains(FIELD_LABEL)
    });
    if edited_cap.is_none() {
        let last = capture_pane(&session);
        let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();
        panic!(
            "edited value {EDITED_VALUE:?} did not re-render in the learnings row; \
             last:\n---\n{last}\n---"
        );
    }

    // 3. Leave Settings and re-open to prove the edit survived (it's persisted,
    //    not just held in the live screen state). Esc → home, then re-open
    //    Settings ▸ Plugins and assert the NEW value renders.
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(15);
    if poll_capture(&session, back_deadline, |c| {
        c.contains(HOME_MARKER) && !c.contains(CONFIG_TITLE)
    })
    .is_none()
    {
        let last = capture_pane(&session);
        let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();
        panic!("Esc from Settings did not return to HomeScreen; last:\n---\n{last}\n---");
    }

    let reopened = open_plugins_category(&session);
    let _ = Command::new("tmux").args(["kill-session", "-t", &session]).status();

    let reopened = reopened.unwrap_or_else(|| {
        panic!("re-opening Settings ▸ Plugins after the edit failed to render the learnings row")
    });
    assert!(
        reopened.contains(EDITED_VALUE),
        "re-opened Plugins category did not show the persisted value {EDITED_VALUE:?}; \
         capture:\n---\n{reopened}\n---"
    );

    // 4. The on-disk config.toml carries the section + the persisted value.
    let on_disk = fs::read_to_string(&config_path).expect("read seeded config.toml");
    assert!(
        on_disk.contains("[plugins.learnings]"),
        "config.toml lost the [plugins.learnings] section; contents:\n---\n{on_disk}\n---"
    );
    assert!(
        on_disk.contains(EDITED_VALUE),
        "config.toml did not persist the edited learnings_dir value {EDITED_VALUE:?}; \
         contents:\n---\n{on_disk}\n---"
    );
}
