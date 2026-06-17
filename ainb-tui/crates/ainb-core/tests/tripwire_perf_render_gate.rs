//! Tripwire: the dirty-gated render loop still repaints on input.
//!
//! Perf bead `wai` changed `run_tui_loop` from an unconditional ~30 fps
//! redraw to a dirty-gated one (draw only on input, a fresh plugin frame, or
//! the 250 ms app-tick animation floor). The regression risk is a frozen /
//! stale screen — a keystroke that does not trigger a repaint. This drives the
//! real `ainb tui` binary in tmux and asserts that navigating Home → Inbox →
//! Home actually changes the rendered pane each time. If the dirty-gate failed
//! to mark a repaint, a poll below would time out and the test would fail.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

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

fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).unwrap();
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-20T00:00:00+00:00"
version = "{}"
skipped_dependencies = []
git_directories = []
"#,
        env!("CARGO_PKG_VERSION")
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).unwrap();

    // Suppress the ainb-hooks first-run install dialog — it overlays the home
    // screen and swallows the navigation keystroke this tripwire sends.
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .unwrap();
}

struct TmuxSession {
    name: String,
}

impl TmuxSession {
    fn new(name: String) -> Self {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-x", "120", "-y", "40"])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed");
        Self { name }
    }

    fn send_keys(&self, keys: &[&str]) {
        let status = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&self.name)
            .args(keys)
            .status()
            .expect("tmux send-keys");
        assert!(status.success(), "tmux send-keys failed");
    }

    fn capture(&self) -> String {
        let out = Command::new("tmux")
            .args(["capture-pane", "-t", &self.name, "-p"])
            .output()
            .expect("tmux capture-pane");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn poll<F>(&self, deadline: Instant, mut ok: F) -> Option<String>
    where
        F: FnMut(&str) -> bool,
    {
        while Instant::now() < deadline {
            let capture = self.capture();
            if ok(&capture) {
                return Some(capture);
            }
            thread::sleep(Duration::from_millis(200));
        }
        None
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

#[test]
fn render_gate_repaints_on_input() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir");
    seed_isolated_home(home.path());

    let session = TmuxSession::new(format!("tripwire-rendergate-{}", std::process::id()));
    let cmd = format!(
        "HOME={} exec {} tui",
        home.path().display(),
        ainb_bin().display()
    );
    session.send_keys(&[&cmd, "Enter"]);

    // 1) Home must render (sidebar shows the Stats [i] tile).
    let home_ready = session.poll(Instant::now() + Duration::from_secs(20), |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    let home_cap = home_ready.unwrap_or_else(|| {
        panic!("HomeScreen never rendered:\n{}", session.capture());
    });
    assert!(
        home_cap.contains("Inbox") && home_cap.contains("[b]"),
        "Home sidebar missing Inbox [b] tile:\n{home_cap}"
    );

    // 2) Press `b` for Inbox. With the dirty-gate, the keystroke must mark a
    //    repaint; the pane should change to the Inbox screen (its title line
    //    `📥 Inbox · …` uses a single space, distinct from the sidebar tile).
    session.send_keys(&["b"]);
    let inbox = session.poll(Instant::now() + Duration::from_secs(12), |c| {
        c.contains("📥 Inbox ·")
    });
    assert!(
        inbox.is_some(),
        "Inbox did not render after pressing 'b' — dirty-gate may have dropped \
         the repaint. Last capture:\n{}",
        session.capture()
    );

    // 3) Press Esc to return Home. Again a keystroke must trigger a repaint
    //    back to the Home sidebar. (Generous deadline: this is a second paint,
    //    not a perf assertion.)
    session.send_keys(&["Escape"]);
    let back_home = session.poll(Instant::now() + Duration::from_secs(15), |c| {
        c.contains("Stats") && c.contains("[i]") && !c.contains("📥 Inbox ·")
    });
    assert!(
        back_home.is_some(),
        "Did not return to Home after Esc — dirty-gate may have dropped the \
         repaint. Last capture:\n{}",
        session.capture()
    );
}
