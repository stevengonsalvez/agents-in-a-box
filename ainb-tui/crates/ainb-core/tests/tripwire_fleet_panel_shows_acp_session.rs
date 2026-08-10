//! Tripwire: an ACP chat session created through the real CLI is VISIBLE in the
//! real Fleet panel, labelled `acp`, in a live terminal.
//!
//! Part 1 of the chat bus shipped a daemon and a CLI, and every end-to-end proof
//! it carried drove those two. The TUI is the operating surface, and it consumed
//! part 1's changes without anything ever opening it.
//!
//! Writing this found the bug it was written to imagine. `ainb-plugin-hangar`
//! maps a provider TWICE: `FleetSessionRow::from` turns `FleetProvider::Acp`
//! into the wire token `acp`, and `provider_label` turns that token into what
//! the operator reads. Part 1 updated the first and not the second, so a chat
//! session rendered as UNKNOWN on the panel while every daemon-level and
//! CLI-level test stayed green. That is the class of regression only this
//! surface can catch.
//!
//! So this drives the real `ainb` binary in tmux against an isolated Hangar,
//! creates the session the way a user would (`ainb fleet acp create`), and reads
//! the pane.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, ExactTmuxSession, FleetHangar};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// A HOME with onboarding complete and the notify prompt dismissed, so no
/// first-run modal eats the `f` that opens the panel.
fn seed_isolated_home(home: &Path, hangar_home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    fs::write(
        cfg.join("onboarding.toml"),
        format!(
            r#"completed = true
completed_at = "2026-08-08T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
            ver = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
    // `notifyd::Paths::from_home` resolves `AINB_HANGAR_HOME` BEFORE `AINB_HOME`,
    // and this test sets both, so the record has to exist under the hangar home
    // or the install offer fires and its modal swallows the `f`.
    seed_notify_dismissed(&base);
    seed_notify_dismissed(hangar_home);
}

fn seed_notify_dismissed(base: &Path) {
    fs::create_dir_all(base).expect("create notify record dir");
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn wait_for(session: &str, needle: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if capture_pane(session).contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Open Fleet after the home input loop is ready. A freshly launched terminal
/// can paint its legend before tmux accepts its first key, so retry the routing
/// shortcut within the bounded startup window.
fn open_fleet_panel(session: &str) -> bool {
    if !wait_for(session, "Enter select | Tab content", 60) {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        send_key(session, "f");
        if wait_for(session, "Fleet ·", 1) {
            return true;
        }
    }
    false
}

#[test]
fn fleet_panel_shows_an_acp_session_created_through_the_cli() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-acpv-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let hangar_home = home_tmp.path().join("hangar-home");
    seed_isolated_home(home_tmp.path(), &hangar_home);
    let _ainb_home = EnvGuard::set("AINB_HOME", &hangar_home);
    let _hangar_home_guard = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    let _no_discovery = EnvGuard::set("AINB_FLEET_DISABLE_TMUX_DISCOVERY", "1");

    // A real daemon on a real socket, same as every other Fleet tripwire.
    let _hangar = FleetHangar::start(&hangar_home);

    // The user's own path to an ACP session: the CLI, over the daemon socket.
    let cwd = home_tmp.path().join("acp-project");
    fs::create_dir_all(&cwd).expect("create project dir");
    let created = Command::new(ainb_bin())
        .args([
            "--format",
            "json",
            "fleet",
            "acp",
            "create",
            "--provider",
            "claude-agent-acp",
            "--cwd",
        ])
        .arg(&cwd)
        .env("HOME", home_tmp.path())
        .env("AINB_HOME", &hangar_home)
        .env("AINB_HANGAR_HOME", &hangar_home)
        .output()
        .expect("run ainb fleet acp create");
    assert!(
        created.status.success(),
        "acp create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let session_key = serde_json::from_slice::<serde_json::Value>(&created.stdout)
        .expect("acp create prints one JSON document")["session_key"]
        .as_str()
        .expect("a session_key")
        .to_string();
    assert!(
        session_key.starts_with("acp:"),
        "unexpected key: {session_key}"
    );

    let name = format!("ainb-acp-panel-{}", std::process::id());
    let tmux = ExactTmuxSession::create(name, "180", "50");
    let session = tmux.name();
    let peers_db = home_tmp.path().join("peers.db");
    let jobs_dir = home_tmp.path().join("jobs");
    let cmd = format!(
        "HOME={home} AINB_HOME={hangar} AINB_HANGAR_HOME={hangar} \
         AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 \
         CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home_tmp.path().display(),
        hangar = hangar_home.display(),
        peers = peers_db.display(),
        jobs = jobs_dir.display(),
        bin = ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "C-m"])
        .status()
        .expect("launch the TUI");

    assert!(
        open_fleet_panel(session),
        "the Fleet panel did not open:\n{}",
        capture_pane(session)
    );

    // The panel lands on the action queue, which shows only what needs the
    // operator; an idle chat session is not on it. `5` is the All view, the
    // key the pane itself advertises, and the first screen that renders rows.
    send_key(session, "5");

    // A row reads `<branch>  ·  <PROVIDER>  ·  <attachment>`, so match the
    // provider IN its row rather than anywhere in the pane: a bare `contains`
    // would pass on any incidental occurrence and prove nothing.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = String::new();
    let mut labelled = None;
    while Instant::now() < deadline {
        last = capture_pane(session);
        labelled = last.lines().find(|line| line.contains("·  ACP  ·")).map(str::to_string);
        if labelled.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    send_key(session, "Escape");

    // UNKNOWN in the provider column is the exact silent regression this
    // tripwire exists for: the panel maps a provider twice (wire token, then
    // display label) and the display half falls back instead of failing.
    assert!(
        !last.contains("·  UNKNOWN  ·"),
        "the panel rendered a session's provider as UNKNOWN, which is how an \
         unmapped provider degrades silently\npane:\n{last}"
    );
    assert!(
        labelled.is_some(),
        "the Fleet panel never rendered the ACP session {session_key} as ACP\npane:\n{last}"
    );
}
