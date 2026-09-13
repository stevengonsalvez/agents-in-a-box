//! Tripwire: a client resizing WHILE the daemon answers a picker (S-D).
//!
//! The design pass could not rule this interaction out. Answering a structured
//! request is not one write: the daemon claims the row, resolves the target
//! session, and sends the answer into that session's tmux pane. A second client
//! attaching or resizing in the middle of that reflows the window under the
//! send.
//!
//! ```text
//!   client 80x24  ─┐
//!   client 120x40 ─┼─▶ one tmux session ◀── the daemon's last-mile send
//!   client 100x30 ─┘        ▲
//!    (resized mid-answer) ──┘
//! ```
//!
//! Two things are asserted, and the second is the subtle one:
//!
//! 1. the answer still lands: the row flips to `answered`, by the surface that
//!    sent it.
//! 2. `window-size` is still `latest`. tmux latches that option to `manual` the
//!    first time anything calls `resize-window`, and a latched window stops
//!    following its clients forever after. The resize here goes through
//!    `refresh-client -C`, which is the one that does not latch, so this
//!    assertion is what stops a future "fix" from reaching for the other.
//!
//! Follows the tmux-ui-tripwire rules: isolated HOME, no bare sleeps before a
//! capture, kill by exact session name, skip rather than fail when the host
//! cannot support it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use serde_json::json;
use sqlx::Row;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, FleetHangar};

const ATTENTION_ID: &str = "att-multiattach-1";

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// A tmux session killed by its exact name on drop, whatever the test did.
struct Session {
    name: String,
}

impl Session {
    fn spawn(name: String, cwd: Option<&Path>, width: u16, height: u16, command: &[&str]) -> Self {
        let mut args = vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            name.clone(),
        ];
        if let Some(cwd) = cwd {
            args.push("-c".to_string());
            args.push(cwd.display().to_string());
        }
        args.push("-x".to_string());
        args.push(width.to_string());
        args.push("-y".to_string());
        args.push(height.to_string());
        for part in command {
            args.push((*part).to_string());
        }
        let status = Command::new("tmux").args(&args).status().expect("tmux new-session");
        assert!(status.success(), "tmux refused to create {name}");
        Self { name }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let target = format!("={}", self.name);
        let _ = Command::new("tmux").args(["kill-session", "-t", &target]).status();
    }
}

fn tmux_out(args: &[&str]) -> String {
    Command::new("tmux")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Every client attached to `session`, by tty. The tty is how `refresh-client`
/// addresses one client rather than all of them.
fn client_ttys(session: &str) -> Vec<String> {
    tmux_out(&["list-clients", "-t", session, "-F", "#{client_tty}"])
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect()
}

/// The `window-size` this session's window actually resolves to.
///
/// The option is unset per-window by default and inherited from the global,
/// so a naive `show-options -w -v` returns an empty string on a healthy
/// session and would make the assertions below pass for the wrong reason.
/// `resize-window` is what SETS it at the window scope, to `manual`, which is
/// the state this reading exists to catch.
fn resolved_window_size(session: &str) -> String {
    let on_window = tmux_out(&["show-options", "-w", "-t", session, "-v", "window-size"]);
    if on_window.is_empty() {
        tmux_out(&["show-options", "-g", "-w", "-v", "window-size"])
    } else {
        on_window
    }
}

fn poll_clients(session: &str, want: usize, deadline: Instant) -> Vec<String> {
    loop {
        let ttys = client_ttys(session);
        if ttys.len() >= want || Instant::now() >= deadline {
            return ttys;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        Command::new("git").args(args).current_dir(dir).output().expect("git");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "tripwire@example.com"]);
    run(&["config", "user.name", "Tripwire"]);
    fs::write(dir.join("README.md"), "multi attach resize\n").expect("seed a file");
    run(&["add", "."]);
    run(&["commit", "-qm", "seed"]);
}

fn seed_isolated_home(home: &Path) {
    let state = home.join(".agents-in-a-box");
    fs::create_dir_all(state.join("config")).expect("create the state dir");
    fs::write(
        state.join("config").join("config.toml"),
        "[ui_preferences]\nshow_container_status = false\n",
    )
    .expect("seed config.toml");
    fs::write(
        state.join("onboarding.toml"),
        format!(
            "completed = true\nversion = \"{}\"\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .expect("seed onboarding.toml");
}

fn seed_session_registry(home: &Path, tmux_name: &str, worktree: &Path) {
    let entry = json!({
        "sessions": {
            tmux_name: {
                "session_id": "6f1f5f7e-0000-4000-8000-0000000000d4",
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "multiattach",
                "created_at": "2026-09-13T00:00:00Z",
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

/// One OPEN structured row, the shape the daemon's ingest writes.
fn seed_structured_ask(hangar: &FleetHangar, cwd: &Path) {
    hangar.block_on(async {
        AttentionRepo::insert(
            hangar.pool(),
            &NewAttention {
                id: ATTENTION_ID.to_string(),
                session_id: "provider-multiattach-1".to_string(),
                cwd: cwd.to_string_lossy().into_owned(),
                workspace_id: None,
                kind: AttentionKind::AskUserQuestion,
                payload: json!({
                    "payload": {
                        "tool_input": {
                            "questions": [{
                                "question": "Which path holds the db",
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

fn attention_row_state(hangar: &FleetHangar) -> Option<(String, Option<String>)> {
    hangar.block_on(async {
        sqlx::query("SELECT state, answered_by FROM attention WHERE id = ?")
            .bind(ATTENTION_ID)
            .fetch_optional(hangar.pool())
            .await
            .expect("read the attention row")
            .map(|row| (row.get("state"), row.get("answered_by")))
    })
}

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/multi-attach-resize.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

fn answer_request() -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::ATTENTION_ANSWER.to_string(),
        params: json!({
            "attention_id": ATTENTION_ID,
            "answer": "data/box.db",
            "answered_by": "tui",
            "op_id": "op-multiattach-1",
        }),
    }
}

#[test]
fn a_resize_mid_answer_leaves_the_answer_landed_and_window_size_latest() {
    if !tmux_available() {
        eprintln!("SKIP: tmux is not on PATH");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-mar-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);

    let hangar_home = home.join("hangar-home");
    fs::create_dir_all(&hangar_home).expect("create the isolated hangar home");
    fs::write(
        hangar_home.join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .expect("dismiss the notification prompt in the daemon home");

    // The daemon resolves its target by shelling `ainb list --format json`,
    // which inherits this process's env. Without these it lists the
    // developer's real sessions and answers "no live session matched".
    let _hangar_home_guard = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    let _ainb_home_guard = EnvGuard::set("AINB_HOME", home);
    let _ainb_bin_guard = EnvGuard::set("AINB_BIN", ainb_bin());
    let hangar = FleetHangar::start(&hangar_home);

    let pid = std::process::id();
    let worktree = home.join("multiattach");
    fs::create_dir_all(&worktree).expect("create the worktree dir");
    init_git_repo(&worktree);

    // The pane the answer is sent into. `cat` keeps it open and echoes what
    // arrives, so a send that lands is visible rather than inferred.
    let agent_name = format!("tmux_mar_{pid}");
    let agent = Session::spawn(
        agent_name.clone(),
        Some(&worktree),
        200,
        50,
        &["sh", "-c", "cat"],
    );
    seed_session_registry(home, &agent_name, &worktree);
    seed_structured_ask(&hangar, &worktree);

    // Two more clients on the SAME session, at sizes that disagree with the
    // origin client and with each other. Nested `tmux attach` is how a test
    // gets a second client with a size of its own.
    //
    // The attach is the holder's own command, not something typed into its
    // shell. `send-keys` into a session that has only just been created races
    // the shell's startup and the keystrokes are simply lost, which shows up
    // as "no clients attached" with nothing anywhere saying why.
    //
    // Three details, each of which produces the same silent nothing when it is
    // missing: no client, no error, and a holder session that has already
    // exited by the time anything looks at it.
    //
    // - `TMUX=` because tmux refuses to nest a client while it is set.
    // - `-S <socket>` because the holder does not inherit `TMUX_TMPDIR`, so
    //   without it the attach looks for a server on the default socket.
    // - `TERM` because a client needs a terminal type; with none, `attach`
    //   exits immediately and takes the holder session with it.
    let socket = tmux_out(&["display-message", "-p", "#{socket_path}"]);
    let attach =
        format!("sh -c 'TERM=xterm-256color TMUX= exec tmux -S {socket} attach -t ={agent_name}'");
    let _holder_a = Session::spawn(format!("mar-hold-a-{pid}"), None, 80, 24, &[&attach]);
    let _holder_b = Session::spawn(format!("mar-hold-b-{pid}"), None, 120, 40, &[&attach]);

    let ttys = poll_clients(&agent_name, 2, Instant::now() + Duration::from_secs(15));
    assert!(
        ttys.len() >= 2,
        "the extra clients never attached; saw {ttys:?}. Without two clients this \
         test is not exercising the reflow path it exists for"
    );

    // The precondition for the second assertion: tmux is still following its
    // clients. If this were already `manual` the end-state check would pass
    // for the wrong reason.
    assert_eq!(
        resolved_window_size(&agent_name),
        "latest",
        "window-size was not `latest` before the answer, so the assertion after \
         it would prove nothing"
    );

    // Resize one client WHILE the answer is in flight. `refresh-client -C` is
    // deliberate: `resize-window` latches `window-size` to `manual`, which is
    // the state the assertion below exists to catch.
    let resize_tty = ttys[0].clone();
    let resize_target = agent_name.clone();
    let resizer = thread::spawn(move || {
        for _ in 0..20 {
            Command::new("tmux")
                .args(["refresh-client", "-t", &resize_tty, "-C", "100x30"])
                .status()
                .ok();
            Command::new("tmux")
                .args(["refresh-client", "-t", &resize_tty, "-C", "90x28"])
                .status()
                .ok();
            thread::sleep(Duration::from_millis(25));
        }
        // Named so a failure says which session was being reflowed.
        resize_target
    });

    let outcome = hangar.block_on(async {
        rpc::dispatch_as(
            hangar.pool(),
            &answer_request(),
            &health(),
            hangar.events(),
            &Caller::Operator,
        )
        .await
    });
    let outcome = serde_json::to_value(outcome).expect("encode the answer outcome");
    let resized_session = resizer.join().expect("the resizer thread panicked");
    assert_eq!(resized_session, agent_name);

    // The row is the daemon's own verdict, read from its store rather than
    // from a pane: a capture could agree while the write never happened.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut answered = None;
    while Instant::now() < deadline {
        if let Some((state, by)) = attention_row_state(&hangar) {
            if state == "answered" {
                answered = Some((state, by));
                break;
            }
        }
        thread::sleep(Duration::from_millis(200));
    }

    let window_size_after = resolved_window_size(&agent_name);
    let clients_after = client_ttys(&agent_name);
    drop(agent);

    let (state, answered_by) = answered.unwrap_or_else(|| {
        panic!(
            "the row never flipped to answered while a client was resizing. \
             outcome={outcome:?} clients={clients_after:?}"
        )
    });
    assert_eq!(state, "answered");
    let answered_by = answered_by.expect("the winner must be recorded on the row");
    assert_eq!(
        answered_by.split('@').next(),
        Some("tui"),
        "the surface that answered must be the one recorded: {answered_by}"
    );
    // NOT asserted here: the `<kind>@<host>` form. `answer::answered_by`
    // stamps that in the SOCKET handler, from the live connection row, so that
    // "client input can therefore never forge another surface". This test
    // dispatches in-process and has no connection, so the wire value passes
    // through unchanged, and a bare kind is the correct result rather than a
    // regression. The host form belongs to a test that owns a real connection;
    // asserting it from here would only pin the harness.

    assert_eq!(
        window_size_after, "latest",
        "the mid-answer resize latched window-size to `manual`; the session has \
         stopped following its clients for good"
    );
}
