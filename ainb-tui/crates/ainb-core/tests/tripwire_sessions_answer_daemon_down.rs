//! Tripwire: an operator answers a waiting agent with the hangar daemon
//! STOPPED, and the answer lands in the agent's own pane.
//!
//! This is the phase the whole epic is judged on. The old Fleet panel died with
//! the daemon; the sessions screen must not. So: no daemon is started at all,
//! the chip comes from the local notifyd producer, and the answer travels the
//! one verified send path into a REAL tmux pane whose contents are then read
//! back.
//!
//! Reading the pane is the point. A receipt, a green tick and a cleared chip
//! can all be produced by a send that never arrived; the only proof is the text
//! showing up where the agent would have read it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::{Envelope, Paths, RetentionPolicy, Store};
use serde_json::json;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().map(|o| o.status.success()).unwrap_or(false)
}

/// A tmux session killed by EXACT name on drop.
struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    fn create(name: String, command: &[&str]) -> Self {
        let mut args: Vec<String> =
            ["new-session", "-d", "-s"].iter().map(|a| (*a).to_string()).collect();
        args.push(name.clone());
        args.extend(["-x", "200", "-y", "50"].iter().map(|a| (*a).to_string()));
        args.extend(command.iter().map(|part| (*part).to_string()));
        let status = Command::new("tmux").args(&args).status().expect("tmux new-session");
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

fn poll_until<F, G>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut arrived: G,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
    G: FnMut(&str) -> bool,
{
    let mut on_screen = false;
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        on_screen = on_screen || arrived(&cap);
        if !on_screen {
            send_key(session, key);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

/// Press `key` until `ok` holds, re-checking between presses so a pane that
/// needs more than one repaint is not walked straight past.
fn press_until<F>(
    session: &str,
    key: &str,
    attempts: usize,
    mut ok: F,
) -> Result<String, Vec<String>>
where
    F: FnMut(&str) -> bool,
{
    let mut seen = Vec::new();
    for _ in 0..attempts {
        for _ in 0..4 {
            let cap = capture_pane(session);
            if ok(&cap) {
                return Ok(cap);
            }
            if seen.last() != Some(&cap) {
                seen.push(cap);
            }
            thread::sleep(Duration::from_millis(250));
        }
        send_key(session, key);
    }
    Err(seen)
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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--initial-branch=main"]);
    fs::write(dir.join("README.md"), "answer fixture\n").expect("seed a file");
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
                "session_id": "6f1f5f7e-0000-4000-8000-0000000000c1",
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "answerdown",
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

fn seed_waiting_hook(home: &Path, cwd: &Path) {
    let paths = Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create notifyd base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    store
        .insert_and_prune(
            &Envelope {
                protocol_version: 1,
                agent: "claude".into(),
                raw_event: "Notification:idle_prompt".into(),
                session_id: "answerdown-1".into(),
                cwd: cwd.to_string_lossy().into_owned(),
                project: "answerdown".into(),
                ts: chrono::Utc::now().timestamp_millis() - 30_000,
                payload: json!({ "message": "Which sqlite path?" }),
            },
            &RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
        )
        .expect("seed the waiting hook row");
}

#[test]
fn an_answer_reaches_the_agents_pane_with_the_hangar_daemon_stopped() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-adown-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);

    let pid = std::process::id();
    let worktree = home.join("answerdown");
    fs::create_dir_all(&worktree).expect("create worktree dir");
    init_git_repo(&worktree);
    let agent_tmux = format!("tmux_adown_{pid}");
    seed_session_registry(home, &agent_tmux, &worktree);
    seed_waiting_hook(home, &worktree);

    // The agent's pane. `cat` echoes whatever is typed into it, which is how
    // this test reads back what the send path actually delivered — a receipt
    // proves a call was made, not that anything arrived.
    let _agent = ExactTmuxSession::create(agent_tmux.clone(), &["sh", "-c", "cat"]);

    let tui_tmux = format!("tripwire-adown-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    // NO hangar home and NO daemon. `AINB_FLEET_DISABLE_TMUX_DISCOVERY` is
    // deliberately NOT set: tmux discovery is the transport under test.
    let launch = format!(
        "HOME={home} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        bin = ainb_bin().display()
    );
    assert!(
        Command::new("tmux")
            .args(["send-keys", "-t", &tui_tmux, &launch, "Enter"])
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

    // The chip appears with no daemon anywhere.
    let chipped = poll_until(
        &tui_tmux,
        "s",
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Workspaces ("),
        |c| c.contains("ASK") && c.contains("needs you"),
    );
    assert!(
        chipped.is_some(),
        "no ASK chip with the daemon stopped — the local producer must carry \
         the surface on its own:\n{}",
        capture_pane(&tui_tmux)
    );

    // Open the `ask` pane. A local row carries no structured options, so the
    // composer is where the cursor starts.
    // Detected by the composer row, NOT by the question text: the `log` pane
    // renders the same hook message, so matching on it stops one tab early and
    // the answer is then typed into a pane where `d` deletes a session.
    let ask_pane = press_until(&tui_tmux, "Tab", 8, |c| {
        c.contains("\u{2460} answer") && c.contains("Which sqlite path?")
    })
        .unwrap_or_else(|seen| {
            panic!(
                "Tab never reached the ask pane. Panes visited:\n  {}\n---\n{}\n---",
                seen.iter()
                    .filter_map(|cap| cap.lines().nth(4))
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("\n  "),
                capture_pane(&tui_tmux)
            )
        });
    assert!(
        ask_pane.contains("Which sqlite path?"),
        "the ask pane must lead with what the agent said, even for a row the \
         LOCAL producer raised:\n{ask_pane}"
    );

    // Type the answer and send it.
    for key in ["d", "a", "t", "a", "-", "d", "i", "r"] {
        send_key(&tui_tmux, key);
        thread::sleep(Duration::from_millis(60));
    }
    let typed = poll(&tui_tmux, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("data-dir")
    });
    assert!(
        typed.is_some(),
        "the answer must be visible as it is typed — and none of those keys may \
         have fired a session shortcut:\n{}",
        capture_pane(&tui_tmux)
    );
    send_key(&tui_tmux, "Enter");

    // THE proof: the text arrives in the agent's OWN pane. Read from the pane,
    // not from a receipt — a receipt proves a call was made.
    let landed = poll(&agent_tmux, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("data-dir")
    });
    assert!(
        landed.is_some(),
        "the answer never reached the agent's pane with the daemon stopped. \
         Agent pane:\n---\n{}\n---\nTUI:\n---\n{}\n---",
        capture_pane(&agent_tmux),
        capture_pane(&tui_tmux)
    );

    // And the pane says what happened, rather than leaving a spinner up.
    let settled = poll(&tui_tmux, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("sent via tmux") || c.contains("not answered")
    });
    let settled = settled.unwrap_or_else(|| capture_pane(&tui_tmux));
    assert!(
        settled.contains("sent via tmux"),
        "the ask pane must report the delivery it actually got:\n{settled}"
    );
    assert!(
        !settled.contains("sending…"),
        "and stop showing an in-flight spinner once it has landed:\n{settled}"
    );

    if let Some(ms) = std::env::var("AINB_CHIP_DEMO_HOLD_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        eprintln!("holding {tui_tmux} for {ms}ms");
        thread::sleep(Duration::from_millis(ms));
    }

    drop(tui);
}
