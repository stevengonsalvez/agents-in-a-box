//! Tripwire: the synchronous permission round-trip, end to end through the
//! real TUI binary.
//!
//! Proves, against a live `ainb tui` in tmux with an isolated HOME:
//!
//! 1. a `PermissionRequest`-derived state renders as the gold `APRV` badge and
//!    a `SessionStart`-derived state as the blue `STRT` badge in the Fleet
//!    panel (`f`);
//! 2. pressing `y` on the APPROVE row delivers a first-class approve to a REAL
//!    broker waiter parked on the isolated `approve.sock` — the same
//!    `client_await` call a blocked Claude `PermissionRequest` hook makes —
//!    and the waiter unblocks with `DecisionKind::Approve`.
//!
//! The broker end is the real `ainb_plugin_notifyd::broker::serve` accept
//! loop, not a stand-in, so the bytes on the socket are exactly what
//! production speaks. HOME lives under `/tmp` (not `TMPDIR`) because
//! `approve.sock` must stay inside AF_UNIX's ~104-char path limit.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::broker::{self, BrokerState, DecisionKind};
use ainb_plugin_notifyd::{Paths, StateRow, Store};

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

/// Seed onboarding + dismissed notify prompt + the two fleet states:
/// APPROVE (newest → selected row 0) and STARTING.
fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-06-30T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(base.join("install.json"), install_record).expect("seed install.json");

    let paths = Paths::under(&base);
    let store = Store::open(&paths.db).expect("open notifications.db");
    let approve = StateRow {
        session_id: "fleet-approve-1".to_string(),
        cwd: home.join("approving-project").display().to_string(),
        kind: "APPROVE".to_string(),
        context: Some(r#"{"tool":"Bash","input":"rm -rf build/"}"#.to_string()),
        parent: Some("atc-main".to_string()),
        last_event_ts: 400,
        source: "hook".to_string(),
    };
    let starting = StateRow {
        session_id: "fleet-start-1".to_string(),
        cwd: home.join("booting-project").display().to_string(),
        kind: "STARTING".to_string(),
        context: None,
        parent: Some("atc-main".to_string()),
        last_event_ts: 300,
        source: "hook".to_string(),
    };
    store.upsert_current_state(&approve).expect("seed APPROVE current_state");
    store.upsert_current_state(&starting).expect("seed STARTING current_state");
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

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn fleet_panel_approve_roundtrips_to_a_blocked_waiter() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    // Short-path HOME: AF_UNIX socket paths cap at ~104 chars.
    let home = PathBuf::from(format!("/tmp/ainb-aprv-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("create short-path home");
    seed_isolated_home(&home);

    // Real broker on the isolated approve.sock, riding a test-owned runtime.
    let paths = Paths::under(home.join(".agents-in-a-box"));
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let broker_state = BrokerState::new();
    {
        let sock = paths.approve_socket.clone();
        let state = broker_state.clone();
        rt.spawn(async move {
            let listener = tokio::net::UnixListener::bind(&sock).expect("bind approve.sock");
            broker::serve(listener, state).await;
        });
    }

    // Park a REAL waiter — the same blocking call a Claude PermissionRequest
    // hook makes. It blocks until the TUI's `y` decides it (or 60s deny-falls-back,
    // which the assertion below would catch as a wrong decision).
    let waiter = {
        let sock = paths.approve_socket.clone();
        thread::spawn(move || {
            broker::client_await(
                &sock,
                "fleet-approve-1",
                "Bash",
                "rm -rf build/",
                Duration::from_secs(60),
            )
        })
    };

    let session = format!("tripwire-fleet-aprv-{}", std::process::id());
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let peers_db = home.join("peers.db");
    let jobs_dir = home.join("jobs");
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_DISABLE_PLUGINS=1 CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home.display(),
        peers = peers_db.display(),
        jobs = jobs_dir.display(),
        bin = ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    let pre = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Fleet") && c.contains("[f]")
    });
    if pre.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        let _ = fs::remove_dir_all(&home);
        panic!("HomeScreen never rendered Fleet shortcut; last capture:\n---\n{last}\n---");
    }

    // 1. Frame truth: both new badges render, APPROVE row selected (newest ts).
    let opened = poll_capture_resending(
        &session,
        "f",
        Instant::now() + Duration::from_secs(30),
        |c| {
            c.contains("APRV")
                && c.contains("STRT")
                && c.contains("needs approval")
                && c.contains("starting")
                && c.contains("approving-project")
                && c.contains("booting-project")
        },
    );
    let Some(open_cap) = opened else {
        let last = capture_pane(&session);
        kill_session(&session);
        let _ = fs::remove_dir_all(&home);
        panic!("Fleet panel did not render APRV+STRT badges; last capture:\n---\n{last}\n---");
    };
    assert!(
        open_cap.contains("y") && open_cap.contains("approve"),
        "help bar must advertise the y approve lever:\n{open_cap}"
    );

    // 2. y on the selected APPROVE row → broker → parked waiter unblocks.
    send_key(&session, "y");
    let feedback = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("approved → fleet-approve-1")
    });
    let Some(feedback_cap) = feedback else {
        let last = capture_pane(&session);
        kill_session(&session);
        let _ = fs::remove_dir_all(&home);
        panic!("approve feedback never rendered; last capture:\n---\n{last}\n---");
    };
    assert!(
        feedback_cap.contains("delivered"),
        "approve must report a matched waiter, not a miss:\n{feedback_cap}"
    );

    let decision = waiter.join().expect("waiter thread");
    kill_session(&session);
    let _ = fs::remove_dir_all(&home);
    assert_eq!(
        decision.decision,
        DecisionKind::Approve,
        "the parked PermissionRequest waiter must receive the human's approve"
    );
}
