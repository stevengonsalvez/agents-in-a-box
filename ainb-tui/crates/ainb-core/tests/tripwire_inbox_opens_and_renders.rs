//! Tripwire: the Inbox screen opens via `b` and renders the
//! ainb-hooks notification rows that ainb-plugin-notifyd persisted
//! to `~/.agents-in-a-box/notifications.db`.
//!
//! Drives the real `ainb` binary inside a tmux pane (so we get
//! genuine ratatui frames), pre-seeds an isolated `$HOME` with:
//!
//! 1. A completed-onboarding marker so the setup wizard doesn't
//!    intercept the first key press.
//! 2. A notifications.db file populated with two synthetic rows
//!    (claude+codex), inserted via the same in-process `Store`
//!    API the daemon uses — proving the schema + reader stay in
//!    sync.
//!
//! Then sends `b` ("in-Box" — `i` is bound to Stats), polls the
//! pane capture for inbox chrome + both notification rows, and
//! also confirms the bottom-menu badge shows `● 2 · I inbox`.
//!
//! Skips gracefully if `tmux` isn't on `$PATH`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::{Envelope, Paths, RetentionPolicy, Store};
use serde_json::json;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn seed_isolated_home(home: &Path) {
    // Skip the onboarding wizard so the first keystroke lands on
    // the actual HomeScreen.
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "{ts}"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ts = "2026-05-27T00:00:00+00:00",
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");

    // Suppress the ainb-hooks first-run install dialog — it overlays the home
    // screen and swallows the `b` keystroke this tripwire sends. Keep the full
    // record shape in sync with `ainb_plugin_notifyd::dismiss_prompt()`: a
    // partial JSON blob fails deserialization and lets the modal reappear.
    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");

    // Seed two synthetic notifications via the real Store API the
    // daemon uses. This proves the same code path produces rows
    // the Inbox screen will read.
    let paths = Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create agents-in-a-box base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    let no_retention = RetentionPolicy {
        retention_days: 0,
        max_rows: 0,
    };
    // Use ts values in the recent past so any future default
    // retention policy doesn't sweep them.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let claude = Envelope {
        protocol_version: 1,
        agent: "claude".into(),
        raw_event: "Notification:idle_prompt".into(),
        session_id: "tripwire-sess-claude-1".into(),
        cwd: "/tmp/fixture-project".into(),
        project: "tripwire-fixture".into(),
        ts: now_ms - 60_000,
        payload: json!({ "message": "Input needed" }),
    };
    let codex = Envelope {
        protocol_version: 1,
        agent: "codex".into(),
        raw_event: "agent-turn-complete".into(),
        session_id: "tripwire-sess-codex-1".into(),
        cwd: "/tmp/fixture-codex-project".into(),
        project: "tripwire-codex".into(),
        ts: now_ms - 30_000,
        payload: json!({}),
    };
    store.insert_and_prune(&claude, &no_retention).expect("seed claude row");
    store.insert_and_prune(&codex, &no_retention).expect("seed codex row");
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

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn poll_capture_resending<F>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        send_key(session, key);
        thread::sleep(Duration::from_millis(500));
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
    }
    None
}

#[test]
fn inbox_opens_and_renders_seeded_notifications() {
    let _serial = serial_lock();
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-inbox-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} exec {} tui",
        home_tmp.path().display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");

    // Wait for HomeScreen — same marker the other tripwire tests use.
    let home_deadline = Instant::now() + Duration::from_secs(90);
    let pre_cap = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    let Some(pre) = pre_cap else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    };

    // Pre-assertion 1: must not already be on the Inbox screen.
    assert!(
        !pre.contains("📥 Inbox"),
        "pre-key capture already on inbox — state leaked.\n{pre}"
    );

    // Pre-assertion 2 (discoverability gate): the HomeScreen V2
    // sidebar MUST list the Inbox tile. Without this, a fresh user
    // has no visual cue that an inbox exists — the `b` shortcut
    // works but is undiscoverable. Tile shape:
    //
    //     📥  Inbox          [b]
    //
    // We tolerate trailing whitespace by checking for the icon +
    // label + shortcut tag separately. The shortcut moved from
    // `I` (Shift+i) to `b` ("in-Box") to avoid the case-pair
    // confusion with Stats (`i`).
    assert!(
        pre.contains("📥") && pre.contains("Inbox") && pre.contains("[b]"),
        "HomeScreen V2 sidebar missing Inbox tile — \
         discoverability regression. Pre-capture:\n{pre}"
    );

    let post_cap = poll_capture_resending(
        &session,
        "b",
        Instant::now() + Duration::from_secs(30),
        |c| {
            c.contains("📥 Inbox")
                && c.contains("Notification:idle_prompt")
                && c.contains("agent-turn-complete")
        },
    );
    if post_cap.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "Inbox didn't render both seeded rows after pressing b; \
             last capture:\n---\n{last}\n---"
        );
    }
    let post = post_cap.unwrap();

    // Each agent label visible in the list rendering.
    assert!(post.contains("claude"), "claude row missing:\n{post}");
    assert!(post.contains("codex"), "codex row missing:\n{post}");

    // Help bar present (proves the screen rendered fully, not truncated).
    assert!(
        post.contains("Enter") && post.contains("dismiss"),
        "help bar missing — partial render:\n{post}"
    );

    kill_session(&session);
}

/// Launch ainb in a detached tmux session and return once the
/// HomeScreen has rendered. Shared by the origin-return tests below.
fn launch_to_home(session: &str, home: &Path) {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");
    let cmd = format!("HOME={} exec {} tui", home.display(), ainb_bin().display());
    Command::new("tmux")
        .args(["send-keys", "-t", session, &cmd, "Enter"])
        .status()
        .expect("send launch cmd");
    if poll_capture(session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(session);
        kill_session(session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }
}

/// The inbox SCREEN title is `📥 Inbox` (single space). The home sidebar
/// tile is `📥  Inbox` (double space) and the session-list legend says
/// `b inbox` (lowercase, no emoji), so this single-space form uniquely
/// identifies "we are on the inbox screen".
fn on_inbox_screen(c: &str) -> bool {
    c.contains("📥 Inbox")
}

#[test]
fn inbox_from_home_returns_home() {
    let _serial = serial_lock();
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let session = format!("tripwire-inbox-home-{}", std::process::id());
    launch_to_home(&session, home_tmp.path());

    if poll_capture_resending(
        &session,
        "b",
        Instant::now() + Duration::from_secs(30),
        on_inbox_screen,
    )
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("inbox never opened after `b`; last:\n---\n{last}\n---");
    }

    send_key(&session, "Escape");
    let back = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("Stats") && c.contains("[i]") && !on_inbox_screen(c)
    });
    let final_cap = capture_pane(&session);
    kill_session(&session);
    assert!(
        back.is_some(),
        "Esc on inbox (opened from home) did not return to home within 25s. \
         Final capture:\n---\n{final_cap}\n---"
    );
}

#[test]
fn inbox_from_session_list_returns_to_session_list() {
    let _serial = serial_lock();
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let session = format!("tripwire-inbox-sessions-{}", std::process::id());
    launch_to_home(&session, home_tmp.path());

    // Hop to the session list (`del-sel` is unique to its legend).
    if poll_capture_resending(
        &session,
        "s",
        Instant::now() + Duration::from_secs(40),
        |c| c.contains("del-sel"),
    )
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("session list never rendered after `s`; last:\n---\n{last}\n---");
    }

    // Open the inbox from the session list (`b` is mirrored here).
    if poll_capture_resending(
        &session,
        "b",
        Instant::now() + Duration::from_secs(30),
        on_inbox_screen,
    )
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("inbox never opened after `b` from session list; last:\n---\n{last}\n---");
    }

    // Esc must return to the SESSION LIST (not home).
    send_key(&session, "Escape");
    let back = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("del-sel") && !on_inbox_screen(c)
    });
    let final_cap = capture_pane(&session);
    kill_session(&session);
    assert!(
        back.is_some(),
        "Esc on inbox (opened from session list) did not return to the session list \
         within 25s. Final capture:\n---\n{final_cap}\n---"
    );
}
