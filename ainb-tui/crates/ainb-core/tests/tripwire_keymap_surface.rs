//! Tripwire for table-owned session-list attach and terminal-owned embed keys.
//!
//! Both journeys run against an isolated HOME and a private tmux server. The
//! target session is the only discoverable terminal because the TUI harness
//! session uses the ignored `ainb-sh-` prefix. That makes selection stable and
//! lets the captures prove the exact terminal the user reached.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const SESSION_LIST_DETAIL: &str = "Session Details";
const OTHER_TMUX_SECTION: &str = "Other tmux";
const FULLSCREEN_TARGET: &str = "KEYMAP_FULLSCREEN_TARGET";
const EMBED_READY: &str = "KEYMAP_EMBED_READY";
const CTRL_C_DELIVERED: &str = "KEYMAP_CTRL_C_DELIVERED";
static TRIPWIRE_LOCK: Mutex<()> = Mutex::new(());

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Private tmux server with exact-name cleanup for every session it creates.
struct TmuxHarness {
    socket_dir: tempfile::TempDir,
    sessions: Vec<String>,
}

impl TmuxHarness {
    fn new() -> Self {
        let socket_dir = tempfile::Builder::new()
            .prefix("ainb-keymap-")
            .tempdir_in("/tmp")
            .expect("create private tmux socket dir");
        Self {
            socket_dir,
            sessions: Vec::new(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", self.socket_dir.path())
            .env_remove("TMUX")
            .args(args)
            .output()
            .expect("run private tmux")
    }

    fn new_session(&mut self, name: String, command: &str) {
        let output = self.run(&[
            "new-session",
            "-d",
            "-s",
            &name,
            "-x",
            "200",
            "-y",
            "50",
            command,
        ]);
        assert!(
            output.status.success(),
            "tmux new-session {name:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.sessions.push(name);
    }

    fn send_key(&self, session: &str, key: &str) {
        let output = self.run(&["send-keys", "-t", session, key]);
        assert!(
            output.status.success(),
            "tmux send-keys {key:?} to {session:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn send_literal(&self, session: &str, text: &str) {
        let output = self.run(&["send-keys", "-t", session, "-l", "--", text]);
        assert!(
            output.status.success(),
            "tmux send-keys literal to {session:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn capture(&self, session: &str) -> String {
        let output = self.run(&["capture-pane", "-t", session, "-p"]);
        assert!(
            output.status.success(),
            "tmux capture-pane {session:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn has_session(&self, session: &str) -> bool {
        let exact = format!("={session}");
        self.run(&["has-session", "-t", &exact]).status.success()
    }

    fn poll_capture<F>(&self, session: &str, deadline: Instant, mut predicate: F) -> Option<String>
    where
        F: FnMut(&str) -> bool,
    {
        while Instant::now() < deadline {
            let capture = self.capture(session);
            if predicate(&capture) {
                return Some(capture);
            }
            thread::sleep(Duration::from_millis(250));
        }
        None
    }
}

impl Drop for TmuxHarness {
    fn drop(&mut self) {
        let sessions = std::mem::take(&mut self.sessions);
        for session in sessions {
            let exact = format!("={session}");
            let _ = self.run(&["kill-session", "-t", &exact]);
        }
    }
}

fn seed_isolated_home(home: &Path, keymap_override: bool) {
    let base = home.join(".agents-in-a-box");
    let config = base.join("config");
    fs::create_dir_all(&config).expect("create isolated config dir");
    fs::write(
        config.join("onboarding.toml"),
        format!(
            "completed = true\n\
             completed_at = \"2026-09-11T00:00:00+00:00\"\n\
             version = \"{version}\"\n\
             skipped_dependencies = []\n\
             git_directories = []\n",
            version = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed notify install record");
    if keymap_override {
        fs::write(base.join("keymap.toml"), "[session_list]\nattach = \"o\"\n")
            .expect("seed keymap override");
    }
}

fn session_list_visible(capture: &str) -> bool {
    capture.contains(SESSION_LIST_DETAIL) && capture.contains(OTHER_TMUX_SECTION)
}

fn tui_launch_command(home: &Path, tmux_dir: &Path) -> String {
    format!(
        "HOME={home} TMUX_TMPDIR={tmux_dir} AINB_DISABLE_PLUGINS=1 exec env -u TMUX {ainb} tui",
        home = home.display(),
        tmux_dir = tmux_dir.display(),
        ainb = ainb_bin().display(),
    )
}

fn launch_to_selected_target(
    harness: &mut TmuxHarness,
    home: &Path,
    target: &str,
    suffix: &str,
) -> String {
    // `ainb-sh-` is excluded by load_other_tmux_sessions. The seeded target is
    // therefore the sole row and selected with no order-dependent navigation.
    let tui = format!("ainb-sh-keymap-tui-{}-{suffix}", std::process::id());
    harness.new_session(tui.clone(), "exec sh");
    let launch = tui_launch_command(home, harness.socket_dir.path());
    harness.send_literal(&tui, &launch);
    harness.send_key(&tui, "Enter");

    let home_capture =
        harness.poll_capture(&tui, Instant::now() + Duration::from_secs(45), |capture| {
            capture.contains("Stats") && capture.contains("[i]")
        });
    assert!(
        home_capture.is_some(),
        "home screen never rendered:\n{}",
        harness.capture(&tui)
    );

    // Drive only from the captured screen. The first navigation key can land
    // before raw input is ready, while a retry after the list appears would
    // trigger the list's own `s` action.
    let deadline = Instant::now() + Duration::from_secs(30);
    let sessions = loop {
        let capture = harness.capture(&tui);
        if session_list_visible(&capture) && capture.contains(target) {
            break Some(capture);
        }
        if capture.contains(SESSION_LIST_DETAIL) {
            harness.send_key(&tui, "f");
        } else if capture.contains("Stats") && capture.contains("Getting Started") {
            harness.send_key(&tui, "s");
        }
        let poll_deadline = std::cmp::min(deadline, Instant::now() + Duration::from_secs(2));
        if let Some(capture) = harness.poll_capture(&tui, poll_deadline, |capture| {
            session_list_visible(capture) && capture.contains(target)
        }) {
            break Some(capture);
        }
        if Instant::now() >= deadline {
            break None;
        }
    };
    assert!(
        sessions.is_some(),
        "session list never rendered the seeded target {target:?}:\n{}",
        harness.capture(&tui)
    );
    tui
}

fn assert_stays_on_session_list(harness: &TmuxHarness, tui: &str, deadline: Instant) -> String {
    let mut last = String::new();
    while Instant::now() < deadline {
        last = harness.capture(tui);
        assert!(
            session_list_visible(&last),
            "Enter left the session list after the table moved attach to `o`:\n{last}"
        );
        thread::sleep(Duration::from_millis(250));
    }
    last
}

#[test]
fn table_override_attaches_only_on_o_then_returns_to_the_session_list() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let _lock = TRIPWIRE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home.path(), true);
    let mut harness = TmuxHarness::new();
    let target = format!("km-full-{}", std::process::id());
    harness.new_session(
        target.clone(),
        &format!(
            "printf '{FULLSCREEN_TARGET}\\n'; while IFS= read -r line; do [ \"$line\" = quit ] && exit 0; done"
        ),
    );
    assert!(
        harness
            .poll_capture(
                &target,
                Instant::now() + Duration::from_secs(10),
                |capture| capture.contains(FULLSCREEN_TARGET),
            )
            .is_some(),
        "seeded fullscreen target never printed its marker:\n{}",
        harness.capture(&target)
    );

    let tui = launch_to_selected_target(&mut harness, home.path(), &target, "override");
    let pre = harness.capture(&tui);
    assert!(
        session_list_visible(&pre),
        "missing session-list precondition:\n{pre}"
    );

    // The override replaces `attach = Enter`; Enter must now remain a no-op.
    harness.send_key(&tui, "Enter");
    let enter_capture =
        assert_stays_on_session_list(&harness, &tui, Instant::now() + Duration::from_secs(2));
    assert!(
        !enter_capture.contains(FULLSCREEN_TARGET) || session_list_visible(&enter_capture),
        "Enter attached the fullscreen target despite the override:\n{enter_capture}"
    );

    harness.send_key(&tui, "o");
    let attached =
        harness.poll_capture(&tui, Instant::now() + Duration::from_secs(20), |capture| {
            capture.contains(FULLSCREEN_TARGET) && !session_list_visible(capture)
        });
    assert!(
        attached.is_some(),
        "`o` did not hand the terminal to the selected target:\n{}",
        harness.capture(&tui)
    );
    let attached = attached.expect("attached pane captured");
    assert!(
        attached.contains(FULLSCREEN_TARGET),
        "`o` never attached the selected target terminal:\n{attached}"
    );
    assert!(
        !session_list_visible(&attached),
        "session-list chrome remained after `o` attached the target:\n{attached}"
    );

    // A real target exit returns control to the suspended TUI.
    harness.send_literal(&target, "quit");
    harness.send_key(&target, "Enter");
    assert!(
        harness
            .poll_capture(&tui, Instant::now() + Duration::from_secs(30), |capture| {
                capture.contains(SESSION_LIST_DETAIL) && !capture.contains(FULLSCREEN_TARGET)
            })
            .is_some(),
        "after target exit, ainb did not return to the session list:\n{}",
        harness.capture(&tui)
    );
    assert!(
        !harness.has_session(&target),
        "target session survived its explicit exit, return path was not exercised"
    );
}

#[test]
fn ctrl_c_reaches_the_interactive_embed_and_ctrl_q_returns_to_tui() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let _lock = TRIPWIRE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home.path(), false);
    let mut harness = TmuxHarness::new();
    let target = format!("km-embed-{}", std::process::id());
    harness.new_session(
        target.clone(),
        &format!(
            "printf '{EMBED_READY}\\n'; trap 'printf \"{CTRL_C_DELIVERED}\\n\"' INT; while :; do IFS= read -r _; done"
        ),
    );
    assert!(
        harness
            .poll_capture(
                &target,
                Instant::now() + Duration::from_secs(10),
                |capture| capture.contains(EMBED_READY),
            )
            .is_some(),
        "seeded embed target never printed its ready marker:\n{}",
        harness.capture(&target)
    );

    let tui = launch_to_selected_target(&mut harness, home.path(), &target, "embed");
    let before_embed = harness.capture(&tui);
    assert!(
        !before_embed.contains("INTERACTIVE"),
        "interactive badge was already present before the fixture launched:\n{before_embed}"
    );

    harness.send_key(&tui, "A");
    let interactive =
        harness.poll_capture(&tui, Instant::now() + Duration::from_secs(20), |capture| {
            capture.contains("INTERACTIVE") && capture.contains(EMBED_READY)
        });
    assert!(
        interactive.is_some(),
        "interactive embed never rendered target output and its focus badge:\n{}",
        harness.capture(&tui)
    );

    harness.send_key(&tui, "C-c");
    assert!(
        harness
            .poll_capture(
                &target,
                Instant::now() + Duration::from_secs(10),
                |capture| capture.contains(CTRL_C_DELIVERED),
            )
            .is_some(),
        "Ctrl-C never reached the embedded terminal program:\n{}",
        harness.capture(&target)
    );
    let still_interactive = harness.capture(&tui);
    assert!(
        still_interactive.contains("INTERACTIVE"),
        "Ctrl-C escaped the embed instead of reaching the target:\n{still_interactive}"
    );

    harness.send_key(&tui, "C-q");
    let released =
        harness.poll_capture(&tui, Instant::now() + Duration::from_secs(15), |capture| {
            session_list_visible(capture) && !capture.contains("INTERACTIVE")
        });
    let released = released.unwrap_or_else(|| harness.capture(&tui));
    assert!(
        session_list_visible(&released),
        "Ctrl-Q did not return from interactive embed to the TUI:\n{released}"
    );
    assert!(
        !released.contains("INTERACTIVE"),
        "Ctrl-Q left stale interactive chrome after release:\n{released}"
    );
    assert!(
        harness.has_session(&target),
        "Ctrl-Q killed the target session instead of detaching from it"
    );
    assert!(
        harness.capture(&target).contains(CTRL_C_DELIVERED),
        "target Ctrl-C marker disappeared before the detach assertion"
    );
}
