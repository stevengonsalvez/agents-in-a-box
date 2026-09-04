//! Tripwire: approving from the `ask` pane unblocks a REAL parked hook, with
//! the hangar daemon stopped.
//!
//! A permission request is not answerable by typing. The hook is blocked in
//! `client_await` on notifyd's approve socket and reads nothing from the pane,
//! so the sessions screen routes an APPROVE to the broker instead of the
//! terminal. The broker is notifyd's and local, which is why this journey works
//! with no hangar daemon anywhere.
//!
//! The parked waiter is the assertion. A cleared chip, a green tick and a
//! receipt can all be produced while the agent is still blocked; only the hook
//! returning proves the decision arrived where the agent reads it.
//!
//! Replaces the Fleet panel's `y`/`n` round-trip. That one went through
//! `fleet/action` and needed a daemon; that verb still exists on the hangar
//! plugin's `F` tab, which this epic does not touch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::broker::{self, BrokerState, DecisionKind};
use ainb_plugin_notifyd::{Envelope, Paths, RetentionPolicy, Store};
use serde_json::json;

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

struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    fn create(name: String) -> Self {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-x", "200", "-y", "50"])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session {name} failed");
        Self { name }
    }
}

impl Drop for ExactTmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &format!("={}", self.name)])
            .status();
    }
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_key(session: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

fn poll<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
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

/// Press `key` until `ok` holds, stopping the presses once `arrived` says the
/// screen is up — `s` opens the sessions screen and then means "favourite this
/// workspace", so re-pressing it there spams failed-favourite toasts.
fn press_until<F, G>(
    session: &str,
    key: &str,
    attempts: usize,
    mut arrived: G,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
    G: FnMut(&str) -> bool,
{
    let mut on_screen = false;
    for _ in 0..attempts {
        for _ in 0..4 {
            let cap = capture_pane(session);
            if ok(&cap) {
                return Some(cap);
            }
            on_screen = on_screen || arrived(&cap);
            thread::sleep(Duration::from_millis(250));
        }
        if !on_screen {
            send_key(session, key);
        }
    }
    None
}

fn init_git_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "tripwire")
            .env("GIT_AUTHOR_EMAIL", "tripwire@example.invalid")
            .env("GIT_COMMITTER_NAME", "tripwire")
            .env("GIT_COMMITTER_EMAIL", "tripwire@example.invalid")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--initial-branch=main"]);
    fs::write(dir.join("README.md"), "approve fixture\n").expect("seed a file");
    git(&["add", "README.md"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-m", "seed"]);
}

fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    fs::write(
        cfg.join("onboarding.toml"),
        format!(
            "completed = true\n\
             completed_at = \"2026-09-04T00:00:00+00:00\"\n\
             version = \"{ver}\"\n\
             skipped_dependencies = []\n\
             git_directories = []\n",
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

fn seed_session_registry(home: &Path, tmux_name: &str, worktree: &Path) {
    let entry = json!({
        "sessions": {
            tmux_name: {
                "session_id": "6f1f5f7e-0000-4000-8000-0000000000e1",
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "approving-project",
                "created_at": "2026-09-04T00:00:00Z",
                "agent_type": "Claude",
                "skip_permissions": true,
            }
        }
    });
    fs::write(
        home.join(".agents-in-a-box").join("sessions.json"),
        serde_json::to_vec_pretty(&entry).expect("encode sessions.json"),
    )
    .expect("seed sessions.json");
}

/// The `PermissionRequest` row that raises the APPROVE chip and carries the
/// agent session id the waiter is parked under.
fn seed_permission_request(base: &Path, cwd: &Path, agent_session_id: &str) {
    let paths = Paths::under(base);
    fs::create_dir_all(&paths.base).expect("create notifyd base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    store
        .insert_and_prune(
            &Envelope {
                protocol_version: 1,
                agent: "claude".into(),
                raw_event: "PermissionRequest".into(),
                session_id: agent_session_id.to_string(),
                cwd: cwd.to_string_lossy().into_owned(),
                project: "approving-project".into(),
                ts: chrono::Utc::now().timestamp_millis() - 20_000,
                payload: json!({ "message": "Allow Bash: rm -rf build/ ?" }),
            },
            &RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
        )
        .expect("seed the permission request row");
}

#[test]
fn approving_from_the_ask_pane_unblocks_a_parked_hook_with_no_daemon() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    // Short-path HOME: AF_UNIX socket paths cap at ~104 chars.
    let home = PathBuf::from(format!("/tmp/ainb-aprv-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("create short-path home");
    seed_isolated_home(&home);

    let project = home.join("approving-project");
    fs::create_dir_all(&project).expect("create the approving project");
    init_git_repo(&project);
    let agent_tmux = format!("tmux_aprv_{}", std::process::id());
    seed_session_registry(&home, &agent_tmux, &project);

    const AGENT_SESSION: &str = "claude-approve-1";
    let base = home.join(".agents-in-a-box");
    seed_permission_request(&base, &project, AGENT_SESSION);

    // The REAL broker on the isolated approve socket.
    let paths = Paths::under(&base);
    paths.ensure_base().expect("create approve dir");
    let (bound_tx, bound_rx) = std::sync::mpsc::channel();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let broker_state = BrokerState::with_timeout(broker::DEFAULT_AWAIT_TIMEOUT);
    {
        let sock = paths.approve_socket.clone();
        let state = broker_state.clone();
        runtime.spawn(async move {
            let listener = tokio::net::UnixListener::bind(&sock).expect("bind approve.sock");
            let _ = bound_tx.send(());
            broker::serve(listener, state).await;
        });
    }
    bound_rx.recv_timeout(Duration::from_secs(5)).expect("approve.sock bound");

    // A REAL parked waiter: the same blocking call a Claude PermissionRequest
    // hook makes. It blocks until the answer decides it.
    let waiter = {
        let sock = paths.approve_socket.clone();
        thread::spawn(move || {
            broker::client_await(
                &sock,
                AGENT_SESSION,
                "Bash",
                "rm -rf build/",
                Duration::from_secs(60),
            )
        })
    };

    let _agent_pane = ExactTmuxSession::create(agent_tmux);
    let tui_tmux = format!("tripwire-aprv-{}", std::process::id());
    let tui = ExactTmuxSession::create(tui_tmux.clone());
    // NO hangar home and NO daemon.
    let cmd = format!(
        "HOME={home} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        bin = ainb_bin().display()
    );
    assert!(
        Command::new("tmux")
            .args(["send-keys", "-t", &tui_tmux, &cmd, "Enter"])
            .status()
            .expect("launch ainb tui")
            .success(),
        "tmux refused the launch command"
    );

    assert!(
        poll(&tui_tmux, Instant::now() + Duration::from_secs(90), |c| {
            c.contains("Sessions") && c.contains("[s]")
        })
        .is_some(),
        "HomeScreen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );

    // 1. The APPROVE chip reaches the session's own row, with no daemon.
    let chipped = press_until(
        &tui_tmux,
        "s",
        20,
        |c| c.contains("Workspaces ("),
        |c| c.contains("APPROVE"),
    );
    assert!(
        chipped.is_some(),
        "the APPROVE chip never reached the row:\n{}",
        capture_pane(&tui_tmux)
    );

    // 2. The `ask` pane offers the only two answers a permission request takes.
    let ask_pane = press_until(
        &tui_tmux,
        "Tab",
        10,
        |_| false,
        |c| c.contains("approve") && c.contains("deny"),
    );
    let Some(ask_cap) = ask_pane else {
        panic!(
            "Tab never reached the ask pane:\n---\n{}\n---",
            capture_pane(&tui_tmux)
        );
    };
    assert!(
        ask_cap.contains("let the tool call proceed") && ask_cap.contains("block it"),
        "each answer has to say what it does — `deny` alone is not obviously \
         `block it`:\n{ask_cap}"
    );

    // 3. `approve` is the first option, so Enter sends it.
    send_key(&tui_tmux, "Enter");

    // THE proof: the blocked hook comes back, with the human's verdict.
    let decision = waiter.join().expect("waiter thread");
    assert_eq!(
        decision.decision,
        DecisionKind::Approve,
        "the parked PermissionRequest hook must receive the human's approve"
    );

    let settled = poll(&tui_tmux, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("approved the waiting hook") || c.contains("not answered")
    });
    let settled = settled.unwrap_or_else(|| capture_pane(&tui_tmux));
    assert!(
        settled.contains("approved the waiting hook"),
        "and the pane must report what it actually did:\n{settled}"
    );

    drop(tui);
    let _ = fs::remove_dir_all(&home);
}
