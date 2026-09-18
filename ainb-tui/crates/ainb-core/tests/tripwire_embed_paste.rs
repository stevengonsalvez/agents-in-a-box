//! Tripwire for #1003 on the terminal host: a clipboard carrying its own
//! bracketed-paste terminator, pasted into a focused in-place embed, lands in
//! the embedded pane as text and never runs as typed keys.
//!
//! The payload goes through the real path: bytes into the TUI's pane, parsed
//! by crossterm (which ends a paste at the first `ESC[201~` it sees), then the
//! host's embed forwarding, then the embedded tmux session running bash with
//! readline's bracketed paste on. A paste that ended early would run
//! `echo pwned-$((6*7))` there and print `pwned-42`.
//!
//! Isolated HOME, private tmux server, exact-name session cleanup.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

const SESSION_LIST_DETAIL: &str = "Session Details";
const OTHER_TMUX_SECTION: &str = "Other tmux";
const PROMPT: &str = "pasteprompt>";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn require_tmux() {
    let output = Command::new("tmux").arg("-V").output().expect("tripwire requires tmux on PATH");
    assert!(
        output.status.success(),
        "tripwire requires working tmux: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Private tmux server with exact-name cleanup for every session it creates.
struct TmuxHarness {
    socket_dir: tempfile::TempDir,
    sessions: Vec<String>,
}

impl TmuxHarness {
    fn new() -> Self {
        let socket_dir = tempfile::Builder::new()
            .prefix("ainb-paste-")
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

    /// Type raw bytes into `session`'s pane, as a terminal would deliver
    /// them: `send-keys -H` writes each hex byte to the pane unchanged.
    fn send_bytes(&self, session: &str, bytes: &[u8]) {
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut args = vec!["send-keys", "-t", session, "-H"];
        args.extend(hex.iter().map(String::as_str));
        let output = self.run(&args);
        assert!(
            output.status.success(),
            "tmux send-keys -H to {session:?} failed: {}",
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
    let tui = format!("ainb-sh-paste-tui-{}-{suffix}", std::process::id());
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

#[test]
fn a_paste_carrying_the_terminator_lands_in_the_embed_as_text() {
    require_tmux();
    let home = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home.path(), false);
    let mut harness = TmuxHarness::new();
    let target = format!("paste-embed-{}", std::process::id());
    harness.new_session(
        target.clone(),
        &format!(
            "env -i PATH=/usr/bin:/bin TERM=xterm-256color PS1='{PROMPT} ' bash --norc --noprofile"
        ),
    );
    assert!(
        harness
            .poll_capture(&target, Instant::now() + Duration::from_secs(10), |c| {
                c.contains(PROMPT)
            })
            .is_some(),
        "the embedded bash never showed its prompt:\n{}",
        harness.capture(&target)
    );

    let tui = launch_to_selected_target(&mut harness, home.path(), &target, "embed");
    harness.send_key(&tui, "A");
    assert!(
        harness
            .poll_capture(&tui, Instant::now() + Duration::from_secs(20), |c| {
                c.contains("INTERACTIVE") && c.contains(PROMPT)
            })
            .is_some(),
        "the in-place embed never went interactive on the bash prompt:\n{}",
        harness.capture(&tui)
    );

    // The #1003 payload, as a terminal delivers a paste: wrapped in the
    // markers, with its own terminator, a return and a command inside.
    let mut paste = b"\x1b[200~".to_vec();
    paste.extend_from_slice(b"echo safe\x1b[201~\recho pwned-$((6*7))\r");
    paste.extend_from_slice(b"\x1b[201~");
    harness.send_bytes(&tui, &paste);

    assert!(
        harness
            .poll_capture(&target, Instant::now() + Duration::from_secs(10), |c| {
                c.contains("pwned-$((6*7))")
            })
            .is_some(),
        "the pasted text never reached the embedded prompt:\n{}",
        harness.capture(&target)
    );
    // Long enough for a leaked return to have run the command.
    thread::sleep(Duration::from_millis(750));
    let pane = harness.capture(&target);
    assert!(
        !pane.contains("pwned-42"),
        "the pasted command ran in the embedded pane:\n{pane}"
    );
    assert!(
        harness.capture(&tui).contains("INTERACTIVE"),
        "the paste knocked the TUI out of the embed:\n{}",
        harness.capture(&tui)
    );
}
