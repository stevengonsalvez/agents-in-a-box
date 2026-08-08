//! Tripwire: an ACP chat session created through the real CLI is VISIBLE in the
//! real Fleet panel, labelled `acp`, in a live terminal.
//!
//! Part 1 of the chat bus shipped a daemon and a CLI, and every end-to-end proof
//! it carried drove those two. The TUI is the operating surface, and it consumed
//! part 1's changes without anything ever opening it: `FleetProvider::Acp` maps
//! to the string `acp` at exactly one line of `ainb-plugin-hangar`
//! (`src/screen/fleet.rs`), and a wire token the panel does not recognise falls
//! to `unknown` SILENTLY. That is a one-line regression nothing else would
//! catch, on the screen the operator actually looks at.
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
fn seed_isolated_home(home: &Path) {
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

/// Press `f` until the Fleet panel is actually on screen.
fn open_fleet_panel(session: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let capture = capture_pane(session);
        if capture.contains("Fleet ·") {
            return Some(capture);
        }
        send_key(session, "f");
        thread::sleep(Duration::from_millis(500));
    }
    None
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
    seed_isolated_home(home_tmp.path());
    let hangar_home = home_tmp.path().join("hangar-home");
    let _ainb_home = EnvGuard::set("AINB_HOME", home_tmp.path().join(".agents-in-a-box"));
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
        .env("AINB_HOME", home_tmp.path().join(".agents-in-a-box"))
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
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_HANGAR_HOME={hangar} \
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

    let panel = open_fleet_panel(session).unwrap_or_else(|| {
        panic!("the Fleet panel never opened:\n{}", capture_pane(session));
    });

    // The pane must show the session AS an ACP session. `unknown` here is the
    // exact silent regression this tripwire exists for: the panel maps the wire
    // token in one place and falls back rather than failing.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = panel;
    let mut seen = false;
    while Instant::now() < deadline {
        last = capture_pane(session);
        if last.contains("acp") {
            seen = true;
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    send_key(session, "Escape");

    assert!(
        seen,
        "the Fleet panel never rendered the ACP session {session_key} as `acp`\npane:\n{last}"
    );
    assert!(
        !last.contains("unknown"),
        "the panel rendered a session as `unknown`, which is how an unmapped \
         provider token degrades silently\npane:\n{last}"
    );
}
