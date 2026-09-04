//! Tripwire: an operator answers a STRUCTURED request through the daemon, and
//! the row is really answered.
//!
//! The sibling of the daemon-down journey. Here a real hangar daemon holds an
//! open `attention` row with two options; the operator picks one from the `ask`
//! pane and `attention/answer` carries it.
//!
//! Two things are checked that the daemon-down test cannot reach:
//!
//! 1. the option list is the agent's own, and picking the SECOND one sends the
//!    second one — an option cursor that renders correctly and sends the
//!    highlighted row's neighbour is the bug this catches;
//! 2. the daemon's `attention` row actually flips to answered, read straight
//!    out of its store. The pane's green tick is what the client believed; the
//!    row is what happened.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use serde_json::json;
use sqlx::Row;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, FleetHangar};

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

/// A tmux session killed by EXACT name on drop.
struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    fn create(name: String, command: &[&str]) -> Self {
        Self::create_inner(name, None, command)
    }

    /// Create the session with its pane STARTED IN `cwd`, so anything
    /// correlating a session to a directory can find it.
    fn create_in(name: String, cwd: &Path, command: &[&str]) -> Self {
        Self::create_inner(name, Some(cwd), command)
    }

    fn create_inner(name: String, cwd: Option<&Path>, command: &[&str]) -> Self {
        let mut args: Vec<String> =
            ["new-session", "-d", "-s"].iter().map(|a| (*a).to_string()).collect();
        args.push(name.clone());
        args.extend(["-x", "200", "-y", "50"].iter().map(|a| (*a).to_string()));
        if let Some(cwd) = cwd {
            args.push("-c".to_string());
            args.push(cwd.to_string_lossy().into_owned());
        }
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
                "workspace_name": "answerup",
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

/// Insert one OPEN structured attention row, the way the daemon's ingest does.
fn seed_structured_ask(hangar: &FleetHangar, cwd: &Path) {
    hangar.block_on(async {
        AttentionRepo::insert(
            hangar.pool(),
            &NewAttention {
                id: "att-answerup-1".to_string(),
                session_id: "provider-answerup-1".to_string(),
                cwd: cwd.to_string_lossy().into_owned(),
                workspace_id: None,
                kind: AttentionKind::AskUserQuestion,
                payload: json!({
                    "payload": {
                        "tool_input": {
                            "questions": [{
                                "question": "Decide the sqlite path",
                                "options": [
                                    {"label": "data/box.db", "description": "repo-root data dir"},
                                    {"label": "api/src/db.sqlite", "description": "beside the API"}
                                ]
                            }]
                        }
                    }
                })
                .to_string(),
                degraded: false,
                created_at: chrono::Utc::now().timestamp_millis() - 30_000,
                raise_transcript: None,
                channels: ainb_hangar_proto::ChannelSet::default(),
            },
        )
        .await
        .expect("seed the structured ASK");
    });
}

/// The daemon's own verdict on the row: its `state` column, and who answered.
fn attention_row_state(hangar: &FleetHangar) -> Option<(String, Option<String>)> {
    hangar.block_on(async {
        sqlx::query("SELECT state, answered_by FROM attention WHERE id = ?")
            .bind("att-answerup-1")
            .fetch_optional(hangar.pool())
            .await
            .expect("read the attention row")
            .map(|row| (row.get("state"), row.get("answered_by")))
    })
}

#[test]
fn picking_the_second_option_answers_the_daemons_row_with_the_second_option() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-aup-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);

    let hangar_home = home.join("hangar-home");
    fs::create_dir_all(&hangar_home).expect("create isolated hangar home");
    fs::write(
        hangar_home.join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .expect("dismiss the notification prompt in the daemon home");
    let _hangar_home_guard = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    // The daemon runs IN THIS PROCESS, and its answer router discovers targets
    // by shelling `ainb list --format json`, which inherits this process's env.
    // Without both of these it lists the developer's real sessions and answers
    // `no live session matched` for the fixture's.
    let _ainb_home_guard = EnvGuard::set("AINB_HOME", home);
    let _ainb_bin_guard = EnvGuard::set("AINB_BIN", ainb_bin());
    let hangar = FleetHangar::start(&hangar_home);

    let pid = std::process::id();
    let worktree = home.join("answerup");
    fs::create_dir_all(&worktree).expect("create worktree dir");
    init_git_repo(&worktree);
    let agent_tmux = format!("tmux_aup_{pid}");
    seed_session_registry(home, &agent_tmux, &worktree);
    seed_structured_ask(&hangar, &worktree);

    // A real pane at the cwd the row names, so the daemon's own last-mile send
    // has somewhere to land. `cat` echoes what arrives.
    let _agent = ExactTmuxSession::create_in(agent_tmux.clone(), &worktree, &["sh", "-c", "cat"]);

    let tui_tmux = format!("tripwire-aup-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    let launch = format!(
        "HOME={home} AINB_HANGAR_HOME={hangar} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        hangar = hangar_home.display(),
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

    let chipped = poll_until(
        &tui_tmux,
        "s",
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Workspaces ("),
        |c| c.contains("ASK") && c.contains("needs you"),
    );
    assert!(
        chipped.is_some(),
        "the daemon's ASK never reached the row:\n{}",
        capture_pane(&tui_tmux)
    );

    // Open the `ask` pane on the structured request. Detected by the option
    // list, which only this pane renders.
    let ask_pane = press_until(&tui_tmux, "Tab", 8, |c| {
        c.contains("data/box.db") && c.contains("api/src/db.sqlite")
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
        ask_pane.contains("Decide the sqlite path"),
        "the pane must lead with the agent's own question:\n{ask_pane}"
    );
    assert!(
        ask_pane.contains("other (type it)"),
        "a structured request still offers free text — an agent's question is \
         not always answerable with one of its own options:\n{ask_pane}"
    );

    // Move to the SECOND option and send it. A cursor that renders on one row
    // and sends its neighbour is exactly what this checks.
    send_key(&tui_tmux, "Down");
    let moved = poll(&tui_tmux, Instant::now() + Duration::from_secs(10), |c| {
        c.lines()
            .any(|line| line.contains("api/src/db.sqlite") && line.contains('\u{25b8}'))
    });
    assert!(
        moved.is_some(),
        "the cursor never moved to the second option:\n{}",
        capture_pane(&tui_tmux)
    );
    send_key(&tui_tmux, "Enter");

    // THE proof, read from the daemon's own store rather than from the pane:
    // the row is answered, and the TUI is recorded as the winner.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut answered = None;
    while Instant::now() < deadline {
        if let Some((state, by)) = attention_row_state(&hangar) {
            if state == "answered" {
                answered = Some((state, by));
                break;
            }
        }
        thread::sleep(Duration::from_millis(300));
    }
    let (state, answered_by) = answered.unwrap_or_else(|| {
        panic!(
            "the daemon's attention row never flipped to answered. TUI:\n---\n{}\n---",
            capture_pane(&tui_tmux)
        )
    });
    assert_eq!(state, "answered");
    assert_eq!(
        answered_by.as_deref(),
        Some("tui"),
        "the sessions screen must be recorded as the surface that answered"
    );

    // And the SECOND option is what the agent received, not the first.
    let landed = poll(&agent_tmux, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("api/src/db.sqlite")
    });
    assert!(
        landed.is_some(),
        "the highlighted option is what must be delivered. Agent pane:\n---\n{}\n---",
        capture_pane(&agent_tmux)
    );
    assert!(
        !capture_pane(&agent_tmux).contains("data/box.db"),
        "the FIRST option must not have been sent:\n{}",
        capture_pane(&agent_tmux)
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
