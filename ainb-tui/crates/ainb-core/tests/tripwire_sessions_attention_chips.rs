//! Tripwire: a session that needs a human wears its chip on its own row.
//!
//! The sessions screen is the ONE attention surface. Phase 1 of that is the
//! chip strip: `ASK` / `APPROVE` / `ERR` / `DONE` with an age, replacing the
//! `[?]` `[!]` `[✓]` markers, plus a header badge counting only the states that
//! are actually blocking an agent.
//!
//! Unit tests and insta snapshots paint chips into a `TestBackend` from a state
//! a test built by hand. Neither proves the chip reaches a REAL session row: the
//! producer (notifyd's store), the matcher (agent + cwd), the status gate (a
//! generating agent shows no chip) and the renderer all sit between a hook event
//! and a painted `ASK`. So this drives the real `ainb` binary in a real tmux
//! pane, against:
//!
//! 1. an isolated `$HOME` with onboarding completed and the hooks-install
//!    prompt dismissed, so no modal eats the `s` keystroke;
//! 2. a REAL idle tmux session plus a `sessions.json` entry, which is what makes
//!    a row appear in the tree at all;
//! 3. one REAL notifications row written through the same `Store` API the
//!    notifyd daemon writes through.
//!
//! Unseeded in the house sense: nothing about the chip is injected into the
//! TUI's state. The only input is a hook event on disk.
//!
//! No hangar daemon runs. That is the point — the local producer has to carry
//! the chip on its own.

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
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A tmux session killed by EXACT name on drop, success or panic.
///
/// `-t name` matches by PREFIX; `-t =name` is the exact form. Every kill here
/// uses the exact form so a concurrently-running tripwire whose name shares a
/// prefix is never collateral.
struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    fn create(name: String, command: &[&str]) -> Self {
        let mut args = vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            name.clone(),
            "-x".to_string(),
            "200".to_string(),
            "-y".to_string(),
            "50".to_string(),
        ];
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

/// Capture the session's active pane.
///
/// Plain `-t <name>`, NOT `-t =<name>`: tmux 3.6a rejects the exact-match `=`
/// prefix on a PANE target (`can't find pane: =name`). `=` is only understood
/// where a SESSION is expected — `has-session` and `kill-session`, which is
/// exactly where the drop guard below uses it.
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

/// Poll the pane without pressing anything.
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

/// Poll the pane, re-pressing `key` each round UNTIL `arrived` is true, then
/// stop pressing and keep polling for `ok`.
///
/// The two phases matter. Before the tree loads, `s` can land on the home
/// screen and be swallowed, so it has to be re-pressed. Once the sessions
/// screen is up, `s` means "favourite this workspace" — re-pressing it there
/// spams failed-favourite toasts across the pane, which is both a false signal
/// and unusable as a recording.
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

/// A real repo with one commit, so the workspace resolves to a name and a
/// branch instead of `(broken)` / `unknown`.
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
    fs::write(dir.join("README.md"), "chipwire fixture\n").expect("seed a file to commit");
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
    // Full record shape: a partial blob fails to deserialize and the first-run
    // hooks-install modal reappears over the home screen, swallowing `s`.
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
}

/// Register the tmux sessions in `sessions.json` so the TUI's tmux discovery
/// resolves each into a real workspace row.
fn seed_session_registry(home: &Path, rows: &[Fixture]) {
    let sessions: serde_json::Map<String, serde_json::Value> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            (
                row.tmux_name.clone(),
                json!({
                    "session_id": format!("6f1f5f7e-0000-4000-8000-00000000000{}", index + 1),
                    "tmux_session_name": row.tmux_name,
                    "worktree_path": row.worktree,
                    "workspace_name": row.workspace,
                    "created_at": "2026-09-04T00:00:00Z",
                    "agent_type": "Claude",
                    "skip_permissions": true,
                }),
            )
        })
        .collect();
    fs::write(
        home.join(".agents-in-a-box").join("sessions.json"),
        serde_json::to_vec_pretty(&json!({ "sessions": sessions })).expect("encode sessions.json"),
    )
    .expect("seed sessions.json");
}

/// One seeded session: a real git worktree, a real idle tmux pane, and the one
/// hook event that decides which chip its row wears.
struct Fixture {
    workspace: String,
    tmux_name: String,
    worktree: PathBuf,
    /// The `raw_event` notifyd classifies, or `None` for a quiet row.
    raw_event: Option<&'static str>,
    /// How long ago the event fired — this is the age the chip must render.
    age: Duration,
}

/// Write the real notification rows, the way the notifyd daemon writes them.
fn seed_hooks(home: &Path, rows: &[Fixture]) {
    let paths = Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create notifyd base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    let now_ms = chrono::Utc::now().timestamp_millis();
    for row in rows {
        let Some(raw_event) = row.raw_event else {
            continue;
        };
        let envelope = Envelope {
            protocol_version: 1,
            agent: "claude".into(),
            raw_event: raw_event.into(),
            session_id: format!("chipwire-{}", row.workspace),
            cwd: row.worktree.to_string_lossy().into_owned(),
            project: row.workspace.clone(),
            ts: now_ms - i64::try_from(row.age.as_millis()).expect("age fits i64"),
            payload: json!({ "message": "Which sqlite path?" }),
        };
        store
            .insert_and_prune(
                &envelope,
                &RetentionPolicy {
                    retention_days: 0,
                    max_rows: 0,
                },
            )
            .expect("seed hook row");
    }
}

/// How far the rendered age may drift from the seeded one.
///
/// Generous on purpose: everything between seeding and reading (four git repos,
/// four tmux panes, a TUI cold start, a preview sweep) sits inside the drift,
/// and the property under test is "the age comes from the hook, not from TUI
/// start" — which a near-zero reading fails by two orders of magnitude.
const AGE_TOLERANCE: Duration = Duration::from_secs(30);

/// The age the chip strip painted for `chip` on this row, e.g. `ASK 42s`.
fn rendered_age(row: &str, chip: &str) -> Option<Duration> {
    let rest = row.split_once(chip)?.1.trim_start();
    let token: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
    let (value, unit) = token.split_at(token.len().checked_sub(1)?);
    let value: u64 = value.parse().ok()?;
    Some(Duration::from_secs(match unit {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3_600,
        "d" => value * 86_400,
        _ => return None,
    }))
}

#[test]
fn waiting_sessions_wear_their_chips_and_the_badge_counts_only_the_blocking_ones() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-chip-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);

    let pid = std::process::id();
    // Three rows, three states. The DONE row is the one that proves the badge
    // is "what is BLOCKING an agent" and not "what is open" — it renders a chip
    // and is deliberately not counted.
    let fixtures: Vec<Fixture> = [
        ("acp-chat", Some("Notification:idle_prompt"), 40),
        ("api-stats", Some("PermissionRequest"), 3 * 60),
        ("site-build", Some("Stop"), 60),
        ("quiet-repo", None, 0),
    ]
    .into_iter()
    .map(|(workspace, raw_event, age_secs)| Fixture {
        workspace: workspace.to_string(),
        // The `tmux_` prefix is what tmux discovery filters on; without it the
        // session is invisible to the TUI.
        tmux_name: format!("tmux_chip{pid}_{workspace}"),
        worktree: home.join(workspace),
        raw_event,
        age: Duration::from_secs(age_secs),
    })
    .collect();

    for fixture in &fixtures {
        fs::create_dir_all(&fixture.worktree).expect("create worktree dir");
        init_git_repo(&fixture.worktree);
    }
    seed_session_registry(home, &fixtures);
    seed_hooks(home, &fixtures);

    // REAL panes running an idle shell. They must NOT look like generating
    // agents: the chip is suppressed while a pane shows a Claude status bar,
    // which is the gate that stops a busy session nagging.
    let _panes: Vec<ExactTmuxSession> = fixtures
        .iter()
        .map(|fixture| {
            ExactTmuxSession::create(fixture.tmux_name.clone(), &["sh", "-c", "sleep 900"])
        })
        .collect();

    let tui_tmux = format!("tripwire-chips-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    let launch = format!(
        "HOME={home} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
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
        !home_capture.contains("ASK"),
        "an ASK chip is visible BEFORE the sessions screen opens — the assertions \
         below would pass on leaked state:\n{home_capture}"
    );

    // `s` opens the sessions screen; the chips then have to survive one preview
    // cycle, which is what flips each pane from Running to Idle.
    let chipped = poll_until(
        &tui_tmux,
        "s",
        Instant::now() + Duration::from_secs(90),
        |c| c.contains("Workspaces ("),
        |c| c.contains("ASK") && c.contains("APPROVE") && c.contains("DONE"),
    );
    let Some(capture) = chipped else {
        panic!(
            "the chip strip never reached the session rows; last capture:\n---\n{}\n---",
            capture_pane(&tui_tmux)
        );
    };

    // The badge counts the ASK and the APPROVE, and NOT the DONE.
    assert!(
        capture.contains("2 need you"),
        "the header badge must count only the blocking rows:\n{capture}"
    );
    assert!(
        !capture.contains("3 need you") && !capture.contains("4 need you"),
        "a DONE row must not be counted as blocking:\n{capture}"
    );

    // Each chip lands on its OWN row, carrying the age of its own hook event —
    // a three-minute-old approval must not read as brand new just because the
    // TUI started a second ago.
    for (workspace, chip, seeded) in [
        ("acp-chat", "ASK", Duration::from_secs(40)),
        ("api-stats", "APPROVE", Duration::from_secs(3 * 60)),
        ("site-build", "DONE", Duration::from_secs(60)),
    ] {
        let row = capture
            .lines()
            .find(|line| line.contains(chip))
            .unwrap_or_else(|| panic!("no row wears the {chip} chip:\n{capture}"));
        let rendered = rendered_age(row, chip)
            .unwrap_or_else(|| panic!("the {chip} chip carries no age: {row}"));
        // A tolerance, not a constant. The seeded age is measured at seeding
        // time and the chip is read after the TUI has started, discovered four
        // tmux panes and captured them, so a fixed `40s` is a test that fails on
        // a slow machine for the right behaviour.
        let drift = rendered.as_secs().abs_diff(seeded.as_secs());
        assert!(
            drift <= AGE_TOLERANCE.as_secs(),
            "the {chip} chip must carry its hook's own age (~{}s, read {}s) — an age \
             measured from TUI start instead would read near zero: {row}",
            seeded.as_secs(),
            rendered.as_secs(),
        );
        assert!(
            capture.contains(workspace),
            "the {workspace} row must be present, not a degraded one:\n{capture}"
        );
    }
    assert!(
        !capture.contains("(broken)"),
        "every seeded row must resolve to a real workspace:\n{capture}"
    );

    // The vocabulary it replaced is gone.
    for retired in ["[?]", "[!]", "[\u{2713}]"] {
        assert!(
            !capture.contains(retired),
            "the retired `{retired}` marker must not render beside the chips:\n{capture}"
        );
    }

    // Optional hold so one clean run can be recorded off the same code path the
    // assertions above just proved. Mirrors `AINB_FLEET_DEMO_PACING_MS`.
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
