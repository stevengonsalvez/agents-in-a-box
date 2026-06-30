//! Tripwire: the Daemons observability screen opens via `d` from the home
//! menu and renders the four-daemon runtime-health table, with a seeded bridge
//! heartbeat flipping the phone-bridge row to "running" + "Telegram".
//!
//! Drives the real `ainb` binary inside a tmux pane (genuine ratatui frames),
//! pre-seeds an isolated `$HOME`/`$AINB_HOME` with:
//!
//! 1. A completed-onboarding marker + a dismissed ainb-hooks install record so
//!    no dialog intercepts the first key press.
//! 2. A `daemons/bridge.json` heartbeat for the CURRENT test process pid
//!    (alive → the aggregator's pid cross-check reports it running, not stale),
//!    connected to "Telegram (@tripwire_bot)".
//!
//! Then sends `d`, polls the pane for the Daemons chrome + all four daemon rows
//! + the running bridge's channel label.
//!
//! Skips gracefully if `tmux` isn't on `$PATH`.

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

fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");

    // Skip onboarding so the first keystroke lands on the HomeScreen.
    let onboarding = format!(
        r#"completed = true
completed_at = "{ts}"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ts = "2026-05-27T00:00:00+00:00",
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    // Suppress the ainb-hooks first-run install dialog (it overlays home and
    // would swallow the `d` keystroke).
    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(base.join("install.json"), install_record).expect("seed install.json");

    // Seed a LIVE bridge heartbeat: pid = this test process (definitely alive),
    // a fresh last_heartbeat_at, connected to Telegram. The aggregator's pid
    // cross-check then reports the bridge running + connected (not stale).
    //
    // The H1 identity guard matches `started_at` against the live process's REAL
    // OS start time, so the seed must use the test process's actual start time
    // (not an arbitrary "10 minutes ago") — otherwise the live-but-mismatched
    // pid would correctly classify as a recycled-pid tombstone (Stopped).
    let daemons = base.join("daemons");
    fs::create_dir_all(&daemons).expect("create daemons dir");
    let now_ms = chrono::Utc::now().timestamp_millis();
    let started_at = ainb::fleet::daemons::heartbeat::process_start_ms(std::process::id())
        .expect("test process start time readable");
    let bridge = format!(
        r#"{{"pid":{pid},"started_at":{started},"last_heartbeat_at":{beat},"last_activity_at":{act},"connected":true,"channel":"Telegram (@tripwire_bot)","error_count":0}}"#,
        pid = std::process::id(),
        started = started_at,
        beat = now_ms - 1_000,
        act = now_ms - 5_000,
    );
    fs::write(daemons.join("bridge.json"), bridge).expect("seed bridge heartbeat");
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
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

fn poll_capture_resending<F>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        send_key(session, key);
        thread::sleep(Duration::from_millis(500));
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
    }
    None
}

#[test]
fn daemons_opens_and_renders_four_rows() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-daemons-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Point both HOME and AINB_HOME at the isolated tmp so the daemons
    // aggregator reads the seeded heartbeat (bridge/fleet/ATC honour
    // $AINB_HOME; notifyd reads $HOME — both land in the tmp here).
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box exec {bin} tui",
        home = home_tmp.path().display(),
        bin = ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    // Wait for HomeScreen — same marker the other tripwire tests use.
    let home_deadline = Instant::now() + Duration::from_secs(90);
    let pre_cap = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    let Some(pre) = pre_cap else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    };

    // Pre-assertion: must not already be on the Daemons screen.
    assert!(
        !pre.contains("runtime health"),
        "pre-key capture already on daemons — state leaked.\n{pre}"
    );

    // Discoverability gate: the HomeScreen V2 sidebar MUST list the Daemons
    // tile with its `[d]` shortcut, else the screen is unreachable by a fresh
    // user who can't see it.
    assert!(
        pre.contains("Daemons") && pre.contains("[d]"),
        "HomeScreen V2 sidebar missing Daemons tile — discoverability regression.\n{pre}"
    );

    let post_cap = poll_capture_resending(
        &session,
        "d",
        Instant::now() + Duration::from_secs(30),
        |c| {
            c.contains("Daemons")
                && c.contains("phone bridge")
                && c.contains("notifyd")
                && c.contains("fleet daemon")
        },
    );
    if post_cap.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "Daemons screen didn't render all four rows after pressing d; \
             last capture:\n---\n{last}\n---"
        );
    }
    let post = post_cap.unwrap();

    // The four daemon rows are present.
    assert!(post.contains("phone bridge"), "bridge row missing:\n{post}");
    assert!(post.contains("notifyd"), "notifyd row missing:\n{post}");
    assert!(post.contains("ATC"), "ATC row missing:\n{post}");
    assert!(
        post.contains("fleet daemon"),
        "fleet daemon row missing:\n{post}"
    );

    // The seeded live bridge heartbeat flips the bridge to running + connected,
    // surfacing the Telegram channel label — the original blind spot, closed.
    assert!(
        post.contains("running"),
        "no running daemon shown despite seeded live heartbeat:\n{post}"
    );
    assert!(
        post.contains("Telegram (@tripwire_bot)"),
        "bridge channel label missing — heartbeat not surfaced:\n{post}"
    );

    kill_session(&session);
}
