//! Tripwire: the right pane becomes a switchboard, and `Enter` stops being
//! ambiguous.
//!
//! Phases 1 and 2 put chips on rows. This is the pane that answers them. What
//! has to be true on a real screen, and is not provable from a unit test:
//!
//! 1. all five labels render, with the unavailable ones DIMMED rather than
//!    hidden — a strip that reflows as sessions change state is a strip nobody
//!    learns;
//! 2. `Tab` walks it and skips what is unavailable;
//! 3. the `ask` pane shows the actual question and its options, taken from the
//!    daemon's payload;
//! 4. the `log` pane shows this session's own notification history;
//! 5. `Enter` is scoped — on `log` it does NOT attach, which is the surprise
//!    the scoping exists to remove.
//!
//! Runs against a real hangar daemon on an isolated socket, so the `ask` pane
//! is rendering a structured request that actually travelled the wire.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
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

/// A tmux session killed by EXACT name on drop. `=name` is the SESSION form;
/// capture and send below use the plain form, which is the only one tmux
/// accepts where a PANE is expected.
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

/// Press `key` until `arrived`, then stop pressing and keep polling for `ok`.
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

/// Press `key` up to `attempts` times, stopping as soon as `ok` holds.
///
/// Re-checks between presses rather than only after each one: a pane can take
/// more than one repaint to settle (the copilot tab dials the daemon when it
/// opens), and pressing again in that window walks straight past the pane the
/// caller was waiting for.
///
/// Returns the matching capture, or every capture it saw, so a failure says
/// which panes it actually visited instead of only the last one.
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

/// The right pane's first content line from each capture, for a failure that
/// has to say which panes were actually visited.
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
    fs::write(dir.join("README.md"), "tab strip fixture\n").expect("seed a file");
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
                "session_id": "6f1f5f7e-0000-4000-8000-0000000000b1",
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "tabstrip",
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

/// One notifyd row, so the `log` tab has real history to render.
///
/// `base` is the notifyd home, NOT `$HOME`. `Paths::from_home` prefers
/// `AINB_HANGAR_HOME` over both `AINB_HOME` and `$HOME`, and this test sets
/// one — so a row written under `$HOME/.agents-in-a-box` is a row the TUI never
/// reads, and the log pane renders its (correct) empty state forever.
fn seed_notification(base: &Path, cwd: &Path) {
    let paths = Paths::under(base);
    fs::create_dir_all(&paths.base).expect("create notifyd base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    store
        .insert_and_prune(
            &Envelope {
                protocol_version: 1,
                agent: "claude".into(),
                raw_event: "Notification:idle_prompt".into(),
                session_id: "tabstrip-1".into(),
                cwd: cwd.to_string_lossy().into_owned(),
                project: "tabstrip".into(),
                ts: chrono::Utc::now().timestamp_millis() - 120_000,
                payload: json!({ "message": "earlier turn ended" }),
            },
            &RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
        )
        .expect("seed notification row");
}

#[test]
fn the_tab_strip_opens_every_pane_and_enter_stops_meaning_attach() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-tabs-")
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
    let worktree = home.join("tabstrip");
    fs::create_dir_all(&worktree).expect("create worktree dir");
    init_git_repo(&worktree);
    let agent_tmux = format!("tmux_tabs_{pid}");
    seed_session_registry(home, &agent_tmux, &worktree);
    seed_notification(&hangar_home, &worktree);

    // A structured ASK, so the `ask` pane has a real question and real options
    // that travelled the wire rather than a fixture built in-process.
    hangar.block_on(async {
        AttentionRepo::insert(
            hangar.pool(),
            &NewAttention {
                id: "att-tabs-1".to_string(),
                session_id: "provider-tabs-1".to_string(),
                cwd: worktree.to_string_lossy().into_owned(),
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
                created_at: chrono::Utc::now().timestamp_millis() - 40_000,
                raise_transcript: None,
                channels: ainb_hangar_proto::ChannelSet::default(),
            },
        )
        .await
        .expect("seed the structured ASK");
    });

    let _pane = ExactTmuxSession::create(agent_tmux, &["sh", "-c", "sleep 900"]);

    let tui_tmux = format!("tripwire-tabs-{pid}");
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

    // The strip renders every label, on the default `preview` pane, before any
    // tab key is pressed.
    let opened = poll_until(
        &tui_tmux,
        "s",
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Workspaces ("),
        |c| c.contains("preview") && c.contains("ask") && c.contains("ASK"),
    );
    let Some(strip) = opened else {
        panic!(
            "the tab strip never rendered on the sessions screen:\n---\n{}\n---",
            capture_pane(&tui_tmux)
        );
    };
    for label in ["preview", "ask", "thread", "copilot", "log"] {
        assert!(
            strip.contains(label),
            "the strip must show every tab, dimmed rather than hidden — `{label}` \
             is missing:\n{strip}"
        );
    }
    assert!(
        strip.contains("attach (any tab)"),
        "the footer must say the attach digits are not scoped:\n{strip}"
    );

    // Tab walks to `ask`, which renders the question and both options straight
    // out of the daemon's payload.
    // BOTH halves in the predicate, not just the pane. `capture-pane` can catch
    // a frame mid-flush: the right pane has repainted and the footer has not, so
    // a capture that matched on the question alone could carry the PREVIOUS
    // tab's footer and fail the `send answer` assertion below on a screen that
    // was correct a frame later.
    let ask_pane = press_until(&tui_tmux, "Tab", 8, |c| {
        c.contains("Decide the sqlite path") && c.contains("send answer")
    })
    .unwrap_or_else(|seen| {
        panic!(
            "Tab never reached the ask pane. Panes visited:\n  {}\n---\n{}\n---",
            visited(&seen),
            capture_pane(&tui_tmux)
        )
    });
    for expected in ["data/box.db", "repo-root data dir", "api/src/db.sqlite"] {
        assert!(
            ask_pane.contains(expected),
            "the ask pane must render the structured options: `{expected}` \
             missing:\n{ask_pane}"
        );
    }
    assert!(
        ask_pane.contains("send answer"),
        "and the footer must say what Enter does HERE:\n{ask_pane}"
    );

    // Keep walking to `log`, which shows this session's own history.
    let log_pane = press_until(&tui_tmux, "Tab", 8, |c| c.contains("earlier turn ended"))
        .unwrap_or_else(|seen| {
            panic!(
                "Tab never reached the log pane. Panes visited:\n  {}\n---\n{}\n---",
                visited(&seen),
                capture_pane(&tui_tmux)
            )
        });
    assert!(
        log_pane.contains("Notification"),
        "the log pane shows the hook event names:\n{log_pane}"
    );

    // THE scoping proof: Enter on `log` does nothing. It used to attach, which
    // would replace this pane with a live terminal.
    send_key(&tui_tmux, "Enter");
    thread::sleep(Duration::from_millis(1500));
    let after_enter = capture_pane(&tui_tmux);
    assert!(
        after_enter.contains("earlier turn ended"),
        "Enter on the log pane must be a no-op, not an attach:\n{after_enter}"
    );
    assert!(
        after_enter.contains("Workspaces ("),
        "and the sessions screen must still be here:\n{after_enter}"
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
