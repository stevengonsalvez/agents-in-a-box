//! Tripwire (witr epic cfx.9): the witr plugin's full interactive loop
//! works end-to-end against the real `ainb` binary in tmux.
//!
//! This is the user-visible proof that the witr plugin is wired into the
//! host as a first-class plugin screen (mirroring burndown). It exercises
//! the entire chain that unit tests can't reach:
//!
//!   tmux send-keys → host key dispatch → `GoToWitr` nav → PluginScreen
//!     render kick (`tick_plugin_renders`) → JSON-RPC `plugin/render` →
//!     witr plugin paints WireBuffer → ratatui paint → tmux pane.
//!
//! And for keys: host `forward_key_to_focused_plugin` → JSON-RPC
//! `plugin/handle_key` → witr `dispatch_key_pure` / async scan →
//! `witr --json` subprocess → snapshot decode → re-render.
//!
//! Three host bugs this test locks down (all were live before cfx.9):
//!   1. A plugin screen with no host-published snapshot feed painted
//!      blank forever — its render-dirty flag was consumed at the seed
//!      `(0, 0)` viewport and never re-kicked at the real size. Fixed by
//!      the viewport-change render trigger in `tick_plugin_renders`.
//!   2. The host's `:` slash-palette swallowed `:` before the plugin saw
//!      it, breaking witr's `port:5432` / `pid:N` / `file:/x` target
//!      syntax. Fixed by suppressing the `:` open-trigger on plugin
//!      screens.
//!   3. The witr screen wasn't registered (`PluginScreen::new(WITR)`),
//!      so nav set `current_screen = "witr"` but nothing rendered.
//!
//! Determinism: the test stubs `witr` on `PATH` with a fixed
//! `--version` (== `MIN_VERSION`) and a fixed `--json` snapshot, so the
//! rendered process tree is identical across machines (real witr output
//! varies per host — the "real witr on mac" leg lives in the plugin
//! crate's `real_witr_smoke.rs`).
//!
//! Skips gracefully if `tmux` isn't on `$PATH` or if `dist/plugins/witr`
//! isn't staged (`scripts/build-plugins.sh`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

/// Walk up from the ainb binary looking for the `dist/plugins/witr`
/// staging dir produced by `scripts/build-plugins.sh`. Returns `None`
/// if absent so the test can skip rather than fail in fresh checkouts.
fn plugins_staged() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("witr").join("witr").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Seed an isolated HOME with `onboarding.toml` so the wizard skips and
/// the TUI boots straight to the home screen.
fn seed_home(home: &Path) {
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
}

/// Write a deterministic `witr` stub into `dir` and mark it executable.
/// `--version` reports exactly `MIN_VERSION` (0.3.2) so detect gates to
/// Ready; any other invocation prints the fixed snapshot. The stub
/// ignores all addressing flags — the plugin's exec passes `--json
/// --port 5432` etc., none of which the stub needs to honour for a
/// fixed fixture.
fn write_stub_witr(dir: &Path) {
    let stub = dir.join("witr");
    let body = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'witr v0.3.2 (commit abc1234, built 2025-04-10T00:00:00Z)\n'
  exit 0
fi
cat <<'JSON'
{"Target":{"Type":"port","Value":"5432"},"ResolvedTarget":"postgres","Process":{"PID":4242,"PPID":800,"Command":"postgres","User":"pguser"},"Ancestry":[{"PID":800,"PPID":1,"Command":"systemd"},{"PID":1,"PPID":0,"Command":"init"}],"Source":{"Type":"systemd","Name":"postgresql.service"},"Warnings":["sample-warning"]}
JSON
exit 0
"#;
    fs::write(&stub, body).expect("write witr stub");
    let mut perms = fs::metadata(&stub).expect("stub metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).expect("chmod stub");
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

/// Send literal text (the `-l` flag) — used for the target string so the
/// `:` in `port:5432` is typed verbatim rather than interpreted as a
/// tmux key name.
fn send_literal(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux send-keys -l");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

fn new_session(session: &str) {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");
}

/// Launch `ainb tui` in the session with the given HOME, plugin root,
/// and PATH prefix (where the stub witr lives). Sent as a single
/// send-keys + Enter to avoid the truncation race documented in
/// `tripwire_burndown_keys.rs`.
fn launch_ainb(session: &str, home: &Path, plugin_root: &Path, path_prefix: &Path) {
    let cmd = format!(
        "HOME={} PATH={}:$PATH AINB_PLUGIN_ROOT={} exec {} tui",
        home.display(),
        path_prefix.display(),
        plugin_root.display(),
        ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");
}

fn wait_for_home(session: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(45);
    // HomeScreen advertises the Stats sidebar entry with its `[i]`
    // shortcut — absent on every other screen. (Mirrors the home gate in
    // `tripwire_burndown_keys.rs`.)
    poll_capture(session, deadline, |c| c.contains("Stats") && c.contains("[i]"))
}

#[test]
fn witr_screen_full_interaction() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/witr not staged — run `scripts/build-plugins.sh` first");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());
    let stub_dir = tempfile::tempdir().expect("stub tempdir");
    write_stub_witr(stub_dir.path());

    let session = format!("tripwire-witr-{}", std::process::id());
    new_session(&session);
    launch_ainb(&session, home_tmp.path(), &plugin_root, stub_dir.path());

    let Some(_home) = wait_for_home(&session) else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    };

    // === Navigate to witr (`w`) ===
    // Positive: the four-tab strip. Negative: the home welcome panel
    // must be gone (the witr plugin renders full-screen, so a lingering
    // "Getting Started" proves nav didn't actually switch screens).
    send_key(&session, "w");
    let witr_deadline = Instant::now() + Duration::from_secs(15);
    let screen = poll_capture(&session, witr_deadline, |c| {
        c.contains("Processes")
            && c.contains("Ports")
            && c.contains("Containers")
            && c.contains("Locks")
    });
    let Some(screen) = screen else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("witr screen never rendered after `w`; last capture:\n---\n{last}\n---");
    };
    assert!(
        !screen.contains("Getting Started"),
        "`w` left the home welcome panel visible — nav didn't switch screens:\n---\n{screen}\n---"
    );
    assert!(
        screen.contains("no target selected"),
        "fresh witr screen should show the empty-target hint:\n---\n{screen}\n---"
    );

    // === Enter target mode (`t`) ===
    send_key(&session, "t");
    let prompt_deadline = Instant::now() + Duration::from_secs(10);
    let prompt = poll_capture(&session, prompt_deadline, |c| c.contains("target>"));
    assert!(
        prompt.is_some(),
        "`t` didn't open the target prompt; last:\n---\n{}\n---",
        capture_pane(&session)
    );

    // === Type a colon-bearing target (`port:5432`) ===
    // This is the load-bearing assertion for the `:` palette-suppression
    // fix: if `:` were swallowed by the host slash palette, the buffer
    // would read `target> port` and freeze.
    send_literal(&session, "port:5432");
    let typed_deadline = Instant::now() + Duration::from_secs(10);
    let typed = poll_capture(&session, typed_deadline, |c| c.contains("target> port:5432"));
    let Some(_typed) = typed else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "target buffer never showed `port:5432` (the `:` was likely swallowed by the host \
             slash palette); last capture:\n---\n{last}\n---"
        );
    };

    // === Commit (Enter) then scan (`r`) ===
    send_key(&session, "Enter");
    send_key(&session, "r");
    let scan_deadline = Instant::now() + Duration::from_secs(20);
    // Positive: the resolved process tree from the stub snapshot.
    let scanned = poll_capture(&session, scan_deadline, |c| {
        c.contains("4242") && c.contains("postgres") && c.contains("systemd") && c.contains("init")
    });
    let Some(scanned) = scanned else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("scan never rendered the process tree after `r`; last:\n---\n{last}\n---");
    };
    // Negative: the pre-scan placeholders must be gone.
    assert!(
        !scanned.contains("no target selected") && !scanned.contains("snapshot missing"),
        "process tree rendered but a pre-scan placeholder lingered:\n---\n{scanned}\n---"
    );

    // === Switch to Ports tab (`2`) ===
    // The active tab marker `[...]` moves from Processes to Ports. The
    // stub snapshot has no port data, so the body shows the per-tab
    // empty hint — proving the tab switch reached the plugin.
    send_key(&session, "2");
    let ports_deadline = Instant::now() + Duration::from_secs(10);
    let ports = poll_capture(&session, ports_deadline, |c| c.contains("[Ports]"));
    let Some(ports) = ports else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`2` didn't switch to the Ports tab; last:\n---\n{last}\n---");
    };
    assert!(
        !ports.contains("[Processes]"),
        "Ports tab active but Processes still marked active:\n---\n{ports}\n---"
    );

    // === Open the detail overlay (`/`) ===
    send_key(&session, "/");
    let detail_deadline = Instant::now() + Duration::from_secs(10);
    let detail = poll_capture(&session, detail_deadline, |c| {
        c.contains("Process detail") && c.contains("4242")
    });
    let Some(_detail) = detail else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`/` didn't open the detail overlay; last:\n---\n{last}\n---");
    };

    // === Close the overlay (`q`) ===
    send_key(&session, "q");
    let closed_deadline = Instant::now() + Duration::from_secs(10);
    let closed = poll_capture(&session, closed_deadline, |c| !c.contains("Process detail"));
    kill_session(&session);
    assert!(
        closed.is_some(),
        "`q` didn't close the detail overlay — it lingered after the close key"
    );
}

#[test]
fn witr_screen_missing_binary_shows_empty_state() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/witr not staged — run `scripts/build-plugins.sh` first");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_home(home_tmp.path());
    // Empty PATH-prefix dir: no `witr` stub. PATH still chains `:$PATH`
    // for system tools, but witr installs live in brew/cargo dirs that
    // aren't on the minimal launch PATH... so to *guarantee* witr is
    // absent we point PATH at the empty dir ONLY (ainb is exec'd by
    // absolute path, so it doesn't need PATH for itself).
    let empty_dir = tempfile::tempdir().expect("empty tempdir");

    let session = format!("tripwire-witr-missing-{}", std::process::id());
    new_session(&session);
    // Launch with PATH set to the empty dir only (no `:$PATH`) so no
    // installed witr can satisfy detect.
    let cmd = format!(
        "HOME={} PATH={} AINB_PLUGIN_ROOT={} exec {} tui",
        home_tmp.path().display(),
        empty_dir.path().display(),
        plugin_root.display(),
        ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    let Some(_home) = wait_for_home(&session) else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    };

    send_key(&session, "w");
    let deadline = Instant::now() + Duration::from_secs(15);
    // Positive: the missing-binary empty state names witr and an install
    // path. Negative: no process tree / target prompt.
    let empty = poll_capture(&session, deadline, |c| {
        c.contains("witr not found") && c.contains("install")
    });
    let last = capture_pane(&session);
    kill_session(&session);
    let Some(empty) = empty else {
        panic!("witr-missing empty state never rendered after `w`; last:\n---\n{last}\n---");
    };
    assert!(
        !empty.contains("no target selected") && !empty.contains("Process detail"),
        "missing-binary state should not show the live UI:\n---\n{empty}\n---"
    );
}
