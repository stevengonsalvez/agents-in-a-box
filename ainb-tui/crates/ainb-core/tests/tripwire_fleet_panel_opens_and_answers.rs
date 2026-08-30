//! Tripwire: the Fleet panel opens from Home, renders Hangar's authoritative
//! Fleet snapshot, switches across operator lenses, completes a vertical-card
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::Paths;
use ainb_plugin_notifyd::broker::{self, BrokerState};

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
    fs::create_dir_all(&hangar_home).expect("create isolated hangar home");
    fs::write(
        hangar_home.join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .expect("dismiss notification prompt in daemon home");
    let _ainb_home = EnvGuard::set("AINB_HOME", home_tmp.path().join(".agents-in-a-box"));
    let _hangar_home_guard = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    let _disable_tmux_discovery = EnvGuard::set("AINB_FLEET_DISABLE_TMUX_DISCOVERY", "1");
    let hangar = FleetHangar::start(&hangar_home);
    let questions = serde_json::json!([
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
    ]);
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
                "tool_input": { "questions": questions.clone() }
            }
        }),
        4_000_000_000_300,
    );
    // Claude emits both of these around AskUserQuestion. They must not
    // downgrade the active structured interview to WAITING.
    hangar.apply_hook(
        "fleet-panel-ask-permission",
        "fleet-panel-ask-1",
        &home_tmp.path().join("fleet-tripwire-project"),
        "PermissionRequest",
        serde_json::json!({
            "matcher": "AskUserQuestion",
            "payload": {
                "tool_use_id": "ask-tool-1",
                "tool_input": { "questions": questions.clone() }
            }
        }),
        4_000_000_000_301,
    );
    hangar.apply_hook(
        "fleet-panel-ask-notification",
        "fleet-panel-ask-1",
        &home_tmp.path().join("fleet-tripwire-project"),
        "Notification",
        serde_json::json!({ "payload": { "notification_type": "permission_prompt" } }),
        4_000_000_000_302,
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

    // Real broker plus the actual lifecycle-hook executable. Fleet answer must
    // traverse TUI -> fleet/action -> Hangar -> broker -> hook stdout, which is
    // the exact JSON Claude receives to resume its AskUserQuestion tool call.
    let paths = Paths::under(&hangar_home);
    let broker_runtime = tokio::runtime::Runtime::new().expect("broker runtime");
    let broker_state =
        BrokerState::with_timeout(ainb_plugin_notifyd::broker::DEFAULT_AWAIT_TIMEOUT);
    {
        let sock = paths.approve_socket.clone();
        let state = broker_state.clone();
        broker_runtime.spawn(async move {
            let listener = tokio::net::UnixListener::bind(&sock).expect("bind approve.sock");
            broker::serve(listener, state).await;
        });
    }
    let hook_payload = serde_json::json!({
        "tool_use_id": "ask-tool-1",
        "tool_input": { "questions": questions }
    });
    let hook_cwd = home_tmp.path().join("fleet-tripwire-project");
    let mut hook = Command::new(ainb_bin())
        .env("HOME", home_tmp.path())
        .env("AINB_HOME", &hangar_home)
        .env("AINB_HANGAR_HOME", &hangar_home)
        .env("AINB_PARENT_SESSION", "fleet-panel-parent")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "PreToolUse",
            "--session-id",
            "fleet-panel-ask-1",
            "--cwd",
            hook_cwd.to_str().expect("UTF-8 hook cwd"),
            "--matcher",
            "AskUserQuestion",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn active Claude structured hook");
    hook.stdin
        .take()
        .expect("hook stdin")
        .write_all(hook_payload.to_string().as_bytes())
        .expect("write active Claude hook payload");
    let hook_waiting = Instant::now() + Duration::from_secs(10);
    while Instant::now() < hook_waiting {
        if broker::client_list(&paths.approve_socket)
            .map(|pending| {
                pending.iter().any(|entry| {
                    entry.session_id == "fleet-panel-ask-1"
                        && entry.request_fingerprint.as_deref()
                            == Some(request_fingerprint.as_str())
                })
            })
            .unwrap_or(false)
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let pending = broker::client_list(&paths.approve_socket).expect("list active hook waiter");
    assert!(
        pending.iter().any(|entry| {
            entry.session_id == "fleet-panel-ask-1"
                && entry.request_fingerprint.as_deref() == Some(request_fingerprint.as_str())
        }),
        "actual lifecycle hook never registered matching structured waiter, expected {request_fingerprint}, seeded {seeded:#?}, pending {pending:#?}"
    );

    let session_name = std::env::var("AINB_FLEET_TRIPWIRE_SESSION")
        .unwrap_or_else(|_| format!("tripwire-fleet-panel-{}", std::process::id()));
    let tmux = ExactTmuxSession::create(session_name, "180", "50");
    let session = tmux.name();

    let peers_db = home_tmp.path().join("peers.db");
    let jobs_dir = home_tmp.path().join("jobs");
    let cmd = format!(
        "HOME={home} AINB_HOME={hangar} AINB_HANGAR_HOME={hangar} AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
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
            && c.contains("2 INPUT")
            && c.contains("1 RUN")
            && c.contains("ACTION QUEUE")
            && c.contains("What release scope should Fleet use?")
            && c.contains("NEEDS YOU")
            && c.contains("REMOTE")
    });
    let Some(open_cap) = opened else {
        let last = capture_pane(&session);
        panic!(
            "Fleet panel did not render authoritative Hangar snapshot; last capture:\n---\n{last}\n---"
        );
    };
    assert!(
        open_cap.contains("1 Needs input") && open_cap.contains("q back"),
        "Fleet help bar missing answer/back controls:\n{open_cap}"
    );
    assert!(
        !open_cap.contains("current_state"),
        "Fleet must not expose or read legacy notifyd current_state:\n{open_cap}"
    );
    assert!(
        !open_cap.contains("REPOSITORY / BRANCH") && !open_cap.contains("CONNECTION"),
        "Fleet home must use compact priority cards, not dense table detail:\n{open_cap}"
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
        c.contains("completed-project") && c.contains("DONE") && c.contains("1/1")
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
            && c.contains("INPUT")
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
        c.contains("STRUCTURED INTERVIEW")
            && c.contains("Scope")
            && c.contains("Validation")
            && c.contains("┌")
            && c.contains("└")
    });
    let Some(interview_cap) = interview else {
        let last = capture_pane(&session);
        panic!("Fleet did not open vertical-card interview; last capture:\n---\n{last}\n---");
    };
    assert!(
        interview_cap.contains("What release scope should Fleet use?"),
        "first structured question missing from interview:\n{interview_cap}"
    );
    demo_pause();

    send_key(&session, "Enter");
    send_key(&session, "Space");
    send_key(&session, "Down");
    send_key(&session, "Space");
    demo_pause();
    send_key(&session, "Right");
    let rollout_card = poll_capture(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("Rollout") && c.contains("When should the release launch?")
    });
    assert!(
        rollout_card.is_some(),
        "Right arrow did not focus Rollout card:\n{}",
        capture_pane(&session)
    );
    demo_pause();
    send_key(&session, "Enter");
    let answered = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("broker accepted, awaiting session resume") && c.contains("STRUCTURED INTERVIEW")
    });
    let Some(answer_cap) = answered else {
        let last = capture_pane(&session);
        panic!("Fleet answer confirmation never rendered; last capture:\n---\n{last}\n---");
    };
    assert!(
        !answer_cap.contains("no live session matched"),
        "structured answer must use Hangar RPC, never legacy tmux discovery:\n{answer_cap}"
    );
    demo_final_pause();
    let receipt_deadline = Instant::now() + Duration::from_secs(10);
    let receipt = loop {
        if let Some(receipt) = hangar.latest_receipt("claude:fleet-panel-ask-1") {
            if receipt.status == "DELIVERED" {
                break receipt;
            }
        }
        assert!(
            Instant::now() < receipt_deadline,
            "structured answer receipt never reached DELIVERED"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(receipt.action_kind, "structured_answer");
    assert_eq!(receipt.status, "DELIVERED");
    assert_eq!(
        receipt.detail.as_deref(),
        Some("claude structured hook broker")
    );
    let hook_output = hook.wait_with_output().expect("wait active Claude hook");
    assert!(
        hook_output.status.success(),
        "active Claude hook failed: {}",
        String::from_utf8_lossy(&hook_output.stderr)
    );
    let hook_response: serde_json::Value =
        serde_json::from_slice(&hook_output.stdout).expect("active Claude hook response JSON");
    assert_eq!(
        hook_response["hookSpecificOutput"]["permissionDecision"], "allow",
        "Claude must receive an allow response, not generic injected text"
    );
    assert_eq!(
        hook_response["hookSpecificOutput"]["updatedInput"]["answers"]["What release scope should Fleet use?"],
        "Focused"
    );
    assert_eq!(
        hook_response["hookSpecificOutput"]["updatedInput"]["answers"]["Which proof should gate launch?"],
        "Tests, Tripwire"
    );
    assert_eq!(
        hook_response["hookSpecificOutput"]["updatedInput"]["answers"]["When should the release launch?"],
        "Now"
    );

    // Claude receives the hook output then emits its next lifecycle signal.
    // The authoritative state must clear the answered interview.
    hangar.apply_hook(
        "fleet-panel-ask-resumed",
        "fleet-panel-ask-1",
        &home_tmp.path().join("fleet-tripwire-project"),
        "PostToolUse",
        serde_json::json!({"tool_name": "AskUserQuestion"}),
        4_000_000_000_303,
    );
    let resumed_snapshot = hangar
        .session("claude:fleet-panel-ask-1")
        .expect("resumed session remains in authoritative snapshot");
    assert_eq!(
        resumed_snapshot.attention,
        ainb_hangar_proto::fleet::AttentionState::None
    );

    // First Esc leaves the open interview draft. Second Esc returns Fleet to Home.
    send_key(&session, "Escape");
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
