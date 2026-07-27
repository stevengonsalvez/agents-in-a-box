//! Tripwire: the Fleet panel opens from Home, renders Hangar's authoritative
//! Fleet snapshot, switches across operator lenses, completes a tabbed
//! multi-question ASK, submits one versioned structured answer batch, and
//! returns to Home via `Esc`.
//!
//! This is the live-terminal sibling of the Fleet panel unit tests. It drives
//! the real `ainb` binary in tmux with an isolated HOME seeded with:
//!
//! 1. completed onboarding + a complete dismissed notify install record, so no
//!    first-run modal swallows the `f` key;
//! 2. a real isolated Hangar store, reducer, daemon socket, and auth token.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::Paths;
use ainb_plugin_notifyd::broker::{self, BrokerState, StructuredResolution};

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

/// Optional paced mode for one continuous VHS capture. Default test runs stay
/// immediate, while a positive value keeps each real TUI state on screen.
fn demo_pause() {
    demo_pause_with("AINB_FLEET_DEMO_PACING_MS");
}

fn demo_final_pause() {
    demo_pause_with("AINB_FLEET_DEMO_FINAL_PACING_MS");
}

fn demo_pause_with(variable: &str) {
    let Ok(milliseconds) = std::env::var(variable) else {
        return;
    };
    if let Ok(milliseconds) = milliseconds.parse::<u64>() {
        if milliseconds > 0 {
            thread::sleep(Duration::from_millis(milliseconds));
        }
    }
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
fn fleet_panel_opens_renders_answers_and_returns_home() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    // Keep approve.sock below AF_UNIX's path-length limit.
    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-fpan-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let hangar_home = home_tmp.path().join("hangar-home");
    let _ainb_home = EnvGuard::set("AINB_HOME", home_tmp.path().join(".agents-in-a-box"));
    let _disable_tmux_discovery = EnvGuard::set("AINB_FLEET_DISABLE_TMUX_DISCOVERY", "1");
    let hangar = FleetHangar::start(&hangar_home);
    hangar.apply_hook(
        "fleet-panel-ask-start",
        "fleet-panel-ask-1",
        &home_tmp.path().join("fleet-tripwire-project"),
        "SessionStart",
        serde_json::json!({ "source": "hook" }),
        4_000_000_000_299,
    );
    hangar.apply_hook(
        "fleet-panel-ask-question",
        "fleet-panel-ask-1",
        &home_tmp.path().join("fleet-tripwire-project"),
        "AskUserQuestion",
        serde_json::json!({
            "payload": {
                "tool_use_id": "ask-tool-1",
                "tool_input": {
                    "questions": [
                        {
                            "id": "scope",
                            "question": "What release scope should Fleet use?",
                            "header": "Scope",
                            "options": [
                                {"label": "Focused", "description": "ship only verified Fleet work"},
                                {"label": "Broad", "description": "include adjacent changes"}
                            ]
                        },
                        {
                            "id": "validation",
                            "question": "Which proof should gate launch?",
                            "header": "Validation",
                            "multiSelect": true,
                            "options": [
                                {"label": "Tests", "description": "run targeted Rust coverage"},
                                {"label": "Tripwire", "description": "capture live terminal truth"}
                            ]
                        },
                        {
                            "id": "rollout",
                            "question": "When should the release launch?",
                            "header": "Rollout",
                            "options": [
                                {"label": "Now", "description": "start after proof completes"},
                                {"label": "Later", "description": "hold for a manual window"}
                            ]
                        }
                    ]
                }
            }
        }),
        4_000_000_000_300,
    );
    hangar.apply_hook(
        "fleet-panel-wait",
        "fleet-panel-wait-1",
        &home_tmp.path().join("waiting-project"),
        "Notification",
        serde_json::json!({
            "reason": "permission_prompt",
            "message": "allow cargo test?"
        }),
        4_000_000_000_200,
    );
    hangar.apply_hook(
        "fleet-panel-running-start",
        "fleet-panel-running-1",
        &home_tmp.path().join("running-project"),
        "SessionStart",
        serde_json::json!({ "source": "hook" }),
        4_000_000_000_100,
    );
    hangar.apply_hook(
        "fleet-panel-running-prompt",
        "fleet-panel-running-1",
        &home_tmp.path().join("running-project"),
        "UserPromptSubmit",
        serde_json::json!({ "prompt": "Run workflow validation" }),
        4_000_000_000_101,
    );
    hangar.apply_hook(
        "fleet-panel-completed-start",
        "fleet-panel-completed-1",
        &home_tmp.path().join("completed-project"),
        "SessionStart",
        serde_json::json!({ "source": "hook" }),
        4_000_000_000_000,
    );
    hangar.apply_hook(
        "fleet-panel-completed-stop",
        "fleet-panel-completed-1",
        &home_tmp.path().join("completed-project"),
        "Stop",
        serde_json::json!({ "reason": "complete" }),
        4_000_000_000_001,
    );
    let seeded = hangar
        .session("claude:fleet-panel-ask-1")
        .expect("seeded ASK appears in authoritative snapshot");
    assert!(
        seeded
            .current_request
            .as_ref()
            .and_then(|request| request.pointer("/payload/tool_input/questions"))
            .is_some(),
        "authoritative ASK snapshot must preserve complete request: {seeded:?}"
    );
    let request_fingerprint = seeded
        .current_request_fingerprint
        .clone()
        .expect("seeded ASK has exact request fingerprint");

    // Real broker plus real structured hook waiter. Fleet answer must traverse
    // TUI -> fleet/action -> Hangar -> broker and unblock exact request.
    let paths = Paths::under(home_tmp.path().join(".agents-in-a-box"));
    let broker_runtime = tokio::runtime::Runtime::new().expect("broker runtime");
    let broker_state = BrokerState::new();
    {
        let sock = paths.approve_socket.clone();
        let state = broker_state.clone();
        broker_runtime.spawn(async move {
            let listener = tokio::net::UnixListener::bind(&sock).expect("bind approve.sock");
            broker::serve(listener, state).await;
        });
    }
    let waiter = {
        let sock = paths.approve_socket.clone();
        let fingerprint = request_fingerprint.clone();
        thread::spawn(move || {
            broker::client_await_structured(
                &sock,
                "fleet-panel-ask-1",
                &fingerprint,
                &[
                    serde_json::json!({
                        "id": "scope",
                        "question": "What release scope should Fleet use?",
                        "header": "Scope",
                        "options": [
                            {"label": "Focused", "description": "ship only verified Fleet work"},
                            {"label": "Broad", "description": "include adjacent changes"}
                        ]
                    }),
                    serde_json::json!({
                        "id": "validation",
                        "question": "Which proof should gate launch?",
                        "header": "Validation",
                        "multiSelect": true,
                        "options": [
                            {"label": "Tests", "description": "run targeted Rust coverage"},
                            {"label": "Tripwire", "description": "capture live terminal truth"}
                        ]
                    }),
                    serde_json::json!({
                        "id": "rollout",
                        "question": "When should the release launch?",
                        "header": "Rollout",
                        "options": [
                            {"label": "Now", "description": "start after proof completes"},
                            {"label": "Later", "description": "hold for a manual window"}
                        ]
                    }),
                ],
                Duration::from_secs(60),
            )
        })
    };

    let session_name = std::env::var("AINB_FLEET_TRIPWIRE_SESSION")
        .unwrap_or_else(|_| format!("tripwire-fleet-panel-{}", std::process::id()));
    let tmux = ExactTmuxSession::create(session_name, "180", "50");
    let session = tmux.name();

    let peers_db = home_tmp.path().join("peers.db");
    let jobs_dir = home_tmp.path().join("jobs");
    let cmd = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_HANGAR_HOME={hangar} AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home_tmp.path().display(),
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
    let Some(pre_cap) = pre else {
        let last = capture_pane(&session);
        panic!("HomeScreen never rendered Fleet shortcut; last capture:\n---\n{last}\n---");
    };
    assert!(
        !pre_cap.contains("What release scope should Fleet use?"),
        "pre-key capture already on Fleet panel: state leaked:\n{pre_cap}"
    );

    assert!(
        open_fleet_screen(&session).is_some(),
        "f did not open Fleet screen:\n{}",
        capture_pane(&session)
    );
    let opened = poll_capture(&session, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("Fleet")
            && c.contains("Hangar")
            && c.contains("1 Needs input 2")
            && c.contains("2 Idle 0")
            && c.contains("3 Completed 1")
            && c.contains("4 Running 1")
            && c.contains("5 All 4")
            && c.contains("NEEDS INPUT")
            && c.contains("What release scope should Fleet use?")
            && c.contains("CONNECTION")
            && c.contains("REMOTE")
    });
    let Some(open_cap) = opened else {
        let last = capture_pane(&session);
        panic!(
            "Fleet panel did not render authoritative Hangar snapshot; last capture:\n---\n{last}\n---"
        );
    };
    assert!(
        open_cap.contains("1-5 views") && open_cap.contains("q/Esc back"),
        "Fleet help bar missing answer/back controls:\n{open_cap}"
    );
    assert!(
        !open_cap.contains("current_state"),
        "Fleet must not expose or read legacy notifyd current_state:\n{open_cap}"
    );
    demo_pause();

    send_key(&session, "4");
    let running = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("running-project") && c.contains("RUNNING") && c.contains("1/1")
    });
    assert!(
        running.is_some(),
        "Running lens did not isolate active workflow:\n{}",
        capture_pane(&session)
    );
    demo_pause();

    send_key(&session, "3");
    let completed = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("completed-project") && c.contains("COMPLETED") && c.contains("1/1")
    });
    assert!(
        completed.is_some(),
        "Completed lens did not isolate finished session:\n{}",
        capture_pane(&session)
    );
    demo_pause();

    send_key(&session, "5");
    let all = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("running-project")
            && c.contains("completed-project")
            && c.contains("fleet-tripwire-project")
            && c.contains("waiting-project")
            && c.contains("5 All 4")
    });
    assert!(
        all.is_some(),
        "All lens did not render complete roster:\n{}",
        capture_pane(&session)
    );
    demo_pause();

    send_key(&session, "1");
    let needs_input = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("What release scope should Fleet use?")
            && c.contains("NEEDS INPUT")
            && c.contains("1/2")
    });
    assert!(
        needs_input.is_some(),
        "Needs input lens did not restore actionable queue:\n{}",
        capture_pane(&session)
    );
    demo_pause();

    send_key(&session, "Enter");
    let interview = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("STRUCTURED INTERVIEW") && c.contains("Scope") && c.contains("Validation")
    });
    let Some(interview_cap) = interview else {
        let last = capture_pane(&session);
        panic!("Fleet did not open tabbed interview; last capture:\n---\n{last}\n---");
    };
    assert!(
        interview_cap.contains("ship only verified Fleet work"),
        "first option description missing from interview:\n{interview_cap}"
    );
    demo_pause();

    send_key(&session, "Enter");
    send_key(&session, "Space");
    send_key(&session, "Down");
    send_key(&session, "Space");
    demo_pause();
    send_key(&session, "Tab");
    let tabbed = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("Rollout") && c.contains("When should the release launch?")
    });
    assert!(
        tabbed.is_some(),
        "Tab did not move to Rollout:\n{}",
        capture_pane(&session)
    );
    demo_pause();
    send_key(&session, "Enter");
    let answered = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("answered ask: Delivered") && c.contains("claude structured hook broker")
    });
    let Some(answer_cap) = answered else {
        let last = capture_pane(&session);
        panic!("Fleet answer dispatch feedback never rendered; last capture:\n---\n{last}\n---");
    };
    assert!(
        !answer_cap.contains("no live session matched"),
        "structured answer must use Hangar RPC, never legacy tmux discovery:\n{answer_cap}"
    );
    demo_final_pause();
    let receipt = hangar
        .latest_receipt("claude:fleet-panel-ask-1")
        .expect("structured answer persisted a Fleet receipt");
    assert_eq!(receipt.action_kind, "structured_answer");
    assert_eq!(receipt.status, "DELIVERED");
    assert_eq!(
        receipt.detail.as_deref(),
        Some("claude structured hook broker")
    );
    let resolution = waiter.join().expect("structured waiter thread");
    let StructuredResolution::Answered { answers } = resolution else {
        panic!("structured waiter did not receive answer: {resolution:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].question, "What release scope should Fleet use?");
    assert_eq!(answers[0].selected_options, ["Focused"]);
    assert_eq!(answers[1].question, "Which proof should gate launch?");
    assert_eq!(answers[1].selected_options, ["Tests", "Tripwire"]);
    assert_eq!(answers[2].question, "When should the release launch?");
    assert_eq!(answers[2].selected_options, ["Now"]);

    send_key(&session, "Escape");
    let back = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("Stats")
            && c.contains("[i]")
            && !c.contains("What release scope should Fleet use?")
    });
    let final_cap = capture_pane(&session);
    assert!(
        back.is_some(),
        "Esc from Fleet panel did not return to Home. Final capture:\n---\n{final_cap}\n---"
    );
}
