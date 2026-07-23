//! Tripwire: the synchronous permission round-trip, end to end through the
//! real TUI binary.
//!
//! Proves, against a live `ainb tui` in tmux with an isolated HOME:
//!
//! 1. reducer-fed `PermissionRequest` and `SessionStart` sessions render from a
//!    real isolated Hangar daemon socket;
//! 2. pressing `y` submits versioned `fleet/action`, persists a DELIVERED
//!    receipt, then reaches the real Claude blocking-hook broker.
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

use ainb_plugin_notifyd::Paths;
use ainb_plugin_notifyd::broker::{self, BrokerState, DecisionKind};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, ExactTmuxSession, FleetHangar};

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

fn open_fleet_screen(session: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let capture = capture_pane(session);
        if capture.contains("Fleet ·") && capture.contains("sessions ·") {
            return Some(capture);
        }
        send_key(session, "f");
        thread::sleep(Duration::from_millis(500));
    }
    None
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
    let hangar_home = home.join("hangar-home");
    let _ainb_home = EnvGuard::set("AINB_HOME", home.join(".agents-in-a-box"));
    let _disable_tmux_discovery = EnvGuard::set("AINB_FLEET_DISABLE_TMUX_DISCOVERY", "1");
    let hangar = FleetHangar::start(&hangar_home);
    hangar.apply_hook(
        "fleet-approve-request",
        "fleet-approve-1",
        &home.join("approving-project"),
        "PermissionRequest",
        serde_json::json!({
            "matcher": "Bash",
            "payload": {
                "tool_use_id": "approve-tool-1",
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf build/"}
            }
        }),
        4_000_000_000_400,
    );
    hangar.apply_hook(
        "fleet-start-session",
        "fleet-start-1",
        &home.join("booting-project"),
        "SessionStart",
        serde_json::json!({ "source": "hook" }),
        4_000_000_000_300,
    );
    let approval_fingerprint = hangar
        .session("claude:fleet-approve-1")
        .and_then(|session| session.current_request_fingerprint)
        .expect("seeded approval has exact request fingerprint");

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

    // Park a REAL waiter: the same blocking call a Claude PermissionRequest
    // hook makes. It blocks until the TUI's `y` decides it (or 60s deny-falls-back,
    // which the assertion below would catch as a wrong decision).
    let waiter = {
        let sock = paths.approve_socket.clone();
        let fingerprint = approval_fingerprint.clone();
        thread::spawn(move || {
            broker::client_await_exact(
                &sock,
                "fleet-approve-1",
                "Bash",
                "rm -rf build/",
                &fingerprint,
                Duration::from_secs(60),
            )
        })
    };

    let tmux = ExactTmuxSession::create(
        format!("tripwire-fleet-aprv-{}", std::process::id()),
        "180",
        "50",
    );
    let session = tmux.name();

    let peers_db = home.join("peers.db");
    let jobs_dir = home.join("jobs");
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_HANGAR_HOME={hangar} AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home.display(),
        hangar = hangar_home.display(),
        peers = peers_db.display(),
        jobs = jobs_dir.display(),
        bin = ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    let pre = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Fleet") && c.contains("[f]")
    });
    if pre.is_none() {
        let last = capture_pane(&session);
        let _ = fs::remove_dir_all(&home);
        panic!("HomeScreen never rendered Fleet shortcut; last capture:\n---\n{last}\n---");
    }

    // 1. Frame truth: both canonical states render, APPROVAL row selected (newest ts).
    assert!(
        open_fleet_screen(&session).is_some(),
        "f did not open Fleet screen:\n{}",
        capture_pane(&session)
    );
    let opened = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("APPR")
            && c.contains("STARTI")
            && c.contains("State: IDLE / APPROVAL")
            && c.contains("claude:fleet-ap")
            && c.contains("claude:fleet-st")
            && c.contains("approving-project")
            && c.contains("hangar-authoritative")
    });
    let Some(open_cap) = opened else {
        let last = capture_pane(&session);
        let _ = fs::remove_dir_all(&home);
        panic!(
            "Fleet panel did not render APPROVAL+STARTING states; last capture:\n---\n{last}\n---"
        );
    };
    assert!(
        open_cap.contains("y") && open_cap.contains("approve"),
        "help bar must advertise the y approve lever:\n{open_cap}"
    );
    assert!(
        !open_cap.contains("current_state"),
        "Fleet must not expose or read legacy notifyd current_state:\n{open_cap}"
    );

    // 2. y on the selected APPROVE row → broker → parked waiter unblocks.
    send_key(&session, "y");
    let feedback = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("approved request: Delivered") && c.contains("claude blocking hook broker")
    });
    let Some(feedback_cap) = feedback else {
        let last = capture_pane(&session);
        let _ = fs::remove_dir_all(&home);
        panic!("approve feedback never rendered; last capture:\n---\n{last}\n---");
    };
    assert!(
        !feedback_cap.contains("approved failed"),
        "approve must succeed through Hangar action RPC:\n{feedback_cap}"
    );

    let receipt = hangar
        .latest_receipt("claude:fleet-approve-1")
        .expect("approval persisted a Fleet receipt");
    assert_eq!(receipt.action_kind, "approve");
    assert_eq!(receipt.status, "DELIVERED");
    assert_eq!(
        receipt.detail.as_deref(),
        Some("claude blocking hook broker")
    );

    let decision = waiter.join().expect("waiter thread");
    let _ = fs::remove_dir_all(&home);
    assert_eq!(
        decision.decision,
        DecisionKind::Approve,
        "the parked PermissionRequest waiter must receive the human's approve"
    );
}
