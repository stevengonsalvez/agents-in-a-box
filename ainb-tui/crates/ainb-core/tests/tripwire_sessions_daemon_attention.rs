//! Tripwire: what only the daemon knows reaches the session row it belongs to.
//!
//! Phase 1 proved the LOCAL producer carries chips with no daemon at all. This
//! is the other half: a real hangar daemon on an isolated socket, a real
//! `attention` row in its store, and the chip that has to appear on the ainb
//! session whose worktree the row was raised in.
//!
//! Three things sit between the row and the chip, and none of them is covered
//! by a unit test:
//!
//! 1. the poller thread actually dialling `attention/list` from inside the TUI;
//! 2. the cwd match between the daemon's `cwd` and the host session's worktree;
//! 3. the request families no local hook classifies — `approval` here, which
//!    notifyd has no event for on a session that never fired a hook.
//!
//! Runs the same journey TWICE against one screen: an `approval` row (a
//! blocking APPROVE, counted in the badge) and an `escalation` row on a cwd no
//! session owns (counted as `elsewhere`, never silently dropped).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use serde_json::json;

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
///
/// `-t =name` is the SESSION form and is what `kill-session` wants; the capture
/// and send helpers below deliberately use the plain form, because tmux rejects
/// `=` where a PANE is expected.
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
        thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Press `key` until the screen arrives, then stop pressing and keep polling.
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
        thread::sleep(Duration::from_millis(500));
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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--initial-branch=main"]);
    fs::write(dir.join("README.md"), "daemon attention fixture\n").expect("seed a file");
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
                "session_id": "6f1f5f7e-0000-4000-8000-0000000000a1",
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "daemon-chip",
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

/// Insert one OPEN attention row the way the daemon's ingest does.
fn seed_attention(
    hangar: &FleetHangar,
    id: &str,
    kind: AttentionKind,
    cwd: &Path,
    payload: serde_json::Value,
) {
    hangar.block_on(async {
        AttentionRepo::insert(
            hangar.pool(),
            &NewAttention {
                id: id.to_string(),
                session_id: format!("provider-{id}"),
                cwd: cwd.to_string_lossy().into_owned(),
                workspace_id: None,
                kind,
                payload: payload.to_string(),
                degraded: false,
                created_at: chrono::Utc::now().timestamp_millis() - 3 * 60_000,
                raise_transcript: None,
                channels: ainb_hangar_proto::ChannelSet::default(),
            },
        )
        .await
        .expect("seed attention row");
    });
}

#[test]
fn a_daemon_raised_approval_reaches_its_session_row_and_an_orphan_is_counted_elsewhere() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    // Keep the daemon socket path under AF_UNIX's length limit.
    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-dchip-")
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
    .expect("dismiss notification prompt in the daemon home");
    let _hangar_home_guard = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    let hangar = FleetHangar::start(&hangar_home);

    let pid = std::process::id();
    let worktree = home.join("daemon-chip");
    fs::create_dir_all(&worktree).expect("create worktree dir");
    init_git_repo(&worktree);
    let agent_tmux = format!("tmux_dchip_{pid}");
    seed_session_registry(home, &agent_tmux, &worktree);

    // An `approval` — a family notifyd never classifies for a session that
    // fired no local hook, so a chip here can only have come from the daemon.
    seed_attention(
        &hangar,
        "att-approve-1",
        AttentionKind::Approval,
        &worktree,
        json!({ "message": "allow cargo test?" }),
    );
    // And an escalation in a directory no session on this screen owns.
    seed_attention(
        &hangar,
        "att-orphan-1",
        AttentionKind::AskUserQuestion,
        &home.join("not-a-session"),
        json!({ "message": "nobody owns this cwd" }),
    );

    let _pane = ExactTmuxSession::create(agent_tmux, &["sh", "-c", "sleep 900"]);

    let tui_tmux = format!("tripwire-dchip-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    let launch = format!(
        "HOME={home} AINB_HANGAR_HOME={hangar} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        hangar = hangar_home.display(),
        bin = ainb_bin().display()
    );
    let launched = Command::new("tmux")
        .args(["send-keys", "-t", &tui_tmux, &launch, "Enter"])
        .status()
        .expect("launch ainb tui");
    assert!(launched.success(), "tmux refused the launch command");

    let home_screen = poll(&tui_tmux, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sessions") && c.contains("[s]")
    });
    let Some(home_capture) = home_screen else {
        panic!(
            "HomeScreen never rendered; last capture:\n{}",
            capture_pane(&tui_tmux)
        );
    };
    assert!(
        !home_capture.contains("APPROVE"),
        "an APPROVE chip is visible before the sessions screen opens:\n{home_capture}"
    );

    let chipped = poll_until(
        &tui_tmux,
        "s",
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Workspaces ("),
        |c| c.contains("APPROVE") && c.contains("waiting elsewhere"),
    );
    let Some(capture) = chipped else {
        panic!(
            "the daemon's rows never reached the sessions screen; last capture:\n---\n{}\n---",
            capture_pane(&tui_tmux)
        );
    };

    // The matched row wears its chip, on the row whose worktree the daemon
    // named — and its age comes from the daemon's own `created_at`.
    let row = capture
        .lines()
        .find(|line| line.contains("APPROVE"))
        .expect("the APPROVE chip is on a row");
    // Tolerant: the row is seeded three minutes old and the TUI takes a few
    // seconds to reach it, so the reading is 3m or 4m — never seconds, which is
    // what an age measured from TUI start would show.
    assert!(
        row.contains("3m") || row.contains("4m"),
        "the chip must age from the daemon row's created_at, not from TUI start: {row}"
    );
    assert!(
        capture.contains("1 needs you"),
        "the daemon's approval is blocking, so the badge must count it:\n{capture}"
    );
    // The orphan is COUNTED, never silently swallowed.
    assert!(
        capture.contains("1 waiting elsewhere"),
        "a daemon row whose cwd matched no row must still be reported, and the \
         whole phrase must survive the default sidebar width:\n{capture}"
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
