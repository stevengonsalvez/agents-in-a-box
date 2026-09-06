//! Tripwire: one message to the checked rows, and every leg's outcome.
//!
//! The old broadcast had its own recipient picker inside a panel — a second
//! roster and a second idea of who a message goes to, sitting beside the
//! checkboxes the sessions list already had. What has to be true on a real
//! screen, and is not provable from a unit test:
//!
//! 1. ticking rows with Space CHANGES the tab strip. `thread` becomes
//!    `broadcast (N)`, so an operator cannot send to four sessions believing
//!    they are writing to one;
//! 2. the recipients are spelled out on the pane, not counted — the checkboxes
//!    are on the other pane and may be scrolled off;
//! 3. every leg renders, including the ones that failed, with a tally. A list
//!    of only the failures cannot be told apart from a list that failed to
//!    render, and a partial failure is the case this pane exists to show.
//!
//! The recipients here are sessions the daemon knows about but cannot actually
//! reach, so the legs come back refused. That is the harder case on purpose: a
//! pane that only ever renders success is one nobody has seen fail.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::{Envelope, Paths, RetentionPolicy, Store};
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
        for _ in 0..6 {
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

fn visited(captures: &[String]) -> String {
    captures
        .iter()
        .filter_map(|cap| cap.lines().nth(4))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n  ")
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
    fs::write(dir.join("README.md"), "broadcast fixture\n").expect("seed a file");
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

/// One notifyd row per session.
///
/// This is where the HOST learns a session's `provider_session_id`: the session
/// tree carries ainb's own UUID, which the agent never sees, so the hook's own
/// id is the only bridge to a `claude:<id>` chat key. No id, no scope to
/// deliver to, and the pane would correctly report both rows as unreachable —
/// a different assertion than the one this test makes.
///
/// `base` is the notifyd home, NOT `$HOME`: `Paths::from_home` prefers
/// `AINB_HANGAR_HOME`, and this test sets one.
fn seed_notification(base: &Path, cwd: &Path, provider_session: &str) {
    let paths = Paths::under(base);
    fs::create_dir_all(&paths.base).expect("create notifyd base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    store
        .insert_and_prune(
            &Envelope {
                protocol_version: 1,
                agent: "claude".into(),
                raw_event: "Notification:idle_prompt".into(),
                session_id: provider_session.into(),
                cwd: cwd.to_string_lossy().into_owned(),
                project: "castfix".into(),
                ts: chrono::Utc::now().timestamp_millis() - 30_000,
                payload: json!({ "message": "turn ended" }),
            },
            &RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
        )
        .expect("seed notification row");
}

/// Two sessions in one workspace, so the multi-select has something to select.
fn seed_session_registry(home: &Path, sessions: &[(&str, &Path)]) {
    let mut map = serde_json::Map::new();
    for (index, (tmux_name, worktree)) in sessions.iter().enumerate() {
        map.insert(
            (*tmux_name).to_string(),
            json!({
                "session_id": format!("6f1f5f7e-0000-4000-8000-00000000000{index}"),
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "castfix",
                "created_at": "2026-09-04T00:00:00Z",
                "agent_type": "Claude",
                "skip_permissions": true,
            }),
        );
    }
    fs::write(
        home.join(".agents-in-a-box").join("sessions.json"),
        serde_json::to_vec_pretty(&json!({ "sessions": map })).expect("encode sessions.json"),
    )
    .expect("seed sessions.json");
}

#[test]
fn checking_rows_turns_the_thread_tab_into_a_broadcast_and_shows_every_leg() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-cast-")
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
    let hangar = FleetHangar::start(&hangar_home);

    let pid = std::process::id();
    let first = home.join("castfix-one");
    let second = home.join("castfix-two");
    for worktree in [&first, &second] {
        fs::create_dir_all(worktree).expect("create worktree dir");
        init_git_repo(worktree);
    }
    let tmux_one = format!("tmux_castone_{pid}");
    let tmux_two = format!("tmux_casttwo_{pid}");
    seed_session_registry(
        home,
        &[(tmux_one.as_str(), &first), (tmux_two.as_str(), &second)],
    );

    // Both halves, because the two identities are learned in different places:
    // the notifyd row is where the HOST learns the provider session id, and the
    // daemon hook is what makes the same session exist on the fleet side, so
    // `fleet/broadcast` has a row to deliver against rather than an unknown key.
    let now = chrono::Utc::now().timestamp_millis();
    for (index, (worktree, provider_session)) in
        [(&first, "cast-one"), (&second, "cast-two")].iter().enumerate()
    {
        seed_notification(&hangar_home, worktree, provider_session);
        hangar.apply_hook(
            &format!("cast-hook-{index}"),
            provider_session,
            worktree,
            "Stop",
            json!({ "message": "turn ended" }),
            now - 30_000,
        );
    }

    let _pane_one = ExactTmuxSession::create(tmux_one, &["sh", "-c", "sleep 900"]);
    let _pane_two = ExactTmuxSession::create(tmux_two, &["sh", "-c", "sleep 900"]);

    let tui_tmux = format!("tripwire-cast-{pid}");
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
    let listed = poll_until(
        &tui_tmux,
        "s",
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Workspaces ("),
        |c| c.contains("preview") && c.contains("thread"),
    );
    assert!(
        listed.is_some(),
        "the sessions screen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );

    // Tick both rows. Space toggles the row under the cursor, so this is
    // check-then-move-then-check, exactly what an operator does.
    send_key(&tui_tmux, "Space");
    thread::sleep(Duration::from_millis(400));
    send_key(&tui_tmux, "Down");
    thread::sleep(Duration::from_millis(400));
    send_key(&tui_tmux, "Space");

    // THE strip claim: the tab stops saying `thread` and says what it will
    // actually do, with the count. An operator must not be able to send to two
    // sessions believing they are writing to one.
    let strip = poll(&tui_tmux, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("broadcast (2)")
    })
    .unwrap_or_else(|| {
        panic!(
            "checking two rows never relabelled the thread tab:\n{}",
            capture_pane(&tui_tmux)
        )
    });
    assert!(
        !strip.contains("│ thread │"),
        "the strip must not still offer a private thread while a broadcast is armed:\n{strip}"
    );

    // Open it, and the recipients are SPELLED OUT: the checkboxes are on the
    // other pane and may be scrolled off at the moment of sending.
    let pane = press_until(&tui_tmux, "Tab", 8, |c| {
        c.contains("claude:cast-one") && c.contains("claude:cast-two")
    })
    .unwrap_or_else(|seen| {
        panic!(
            "Tab never reached the broadcast pane. Panes visited:\n  {}\n---\n{}\n---",
            visited(&seen),
            capture_pane(&tui_tmux)
        )
    });
    assert!(
        pane.contains("broadcast to 2"),
        "the footer must say what Enter does HERE, with the count:\n{pane}"
    );

    // Type and send. The recipients are sessions the daemon knows but cannot
    // reach, so the legs come back refused — the harder case, and the one a
    // pane that only ever renders success has never been shown.
    for key in ["ship", "Space", "it"] {
        send_key(&tui_tmux, key);
        thread::sleep(Duration::from_millis(150));
    }
    assert!(
        poll(&tui_tmux, Instant::now() + Duration::from_secs(20), |c| c
            .contains("ship it"))
        .is_some(),
        "the composer never showed the typed message:\n{}",
        capture_pane(&tui_tmux)
    );
    send_key(&tui_tmux, "Enter");

    let sent = poll(&tui_tmux, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("delivered to ")
    })
    .unwrap_or_else(|| {
        panic!(
            "the broadcast never reported a tally:\n{}",
            capture_pane(&tui_tmux)
        )
    });
    // Both legs render, whatever they did. A list of only the failures cannot
    // be told apart from a list that failed to render.
    for target in ["claude:cast-one", "claude:cast-two"] {
        assert!(
            sent.lines().any(|line| line.contains(target)
                && (line.contains("DELIVERED")
                    || line.contains("REJECTED")
                    || line.contains("FAILED")
                    || line.contains("PENDING")
                    || line.contains("UNKNOWN"))),
            "no receipt row for {target}:\n{sent}"
        );
    }
    assert!(
        sent.contains("delivered to ") && sent.contains(" of 2"),
        "the tally must say how many of how many:\n{sent}"
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
