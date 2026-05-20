//! Tripwire: HomeScreen sidebar mouse resize persists and restores.
//!
//! Drives the real `ainb tui` binary in tmux with xterm SGR mouse
//! sequences. This protects the visible mouse path, not only helper
//! state or config serialization.

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
            thread::sleep(Duration::from_millis(500));
        }
        None
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

fn send_sgr_mouse(session: &TmuxSession, code: u16, x: u16, y: u16, press_or_drag: bool) {
    let suffix = if press_or_drag { 'M' } else { 'm' };
    let sgr = format!("[<{};{};{}{}", code, x + 1, y + 1, suffix);
    session.send_keys(&["Escape", &sgr]);
}

fn config_text(home: &Path) -> String {
    fs::read_to_string(home.join(".agents-in-a-box/config/config.toml")).unwrap_or_default()
}

#[test]
fn tui_home_sidebar_mouse_resize_persists_and_restores() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let ainb = ainb_bin();
    let session_name = format!("tripwire-home-sidebar-mouse-{}", std::process::id());

    {
        let session = TmuxSession::new(session_name.clone());
        let cmd = format!(
            "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
            home_tmp.path().display(),
            ainb.display()
        );
        session.send_keys(&[&cmd, "Enter"]);

        let home = session.poll(Instant::now() + Duration::from_secs(45), |c| {
            c.contains("Agents") && c.contains("Sessions") && c.contains("[s]")
        });
        let Some(pre_cap) = home else {
            panic!(
                "HomeScreen never rendered before mouse resize:\n{}",
                session.capture()
            );
        };
        assert!(
            !pre_cap.contains("home_sidebar_width"),
            "pre-capture unexpectedly contains config text:\n{pre_cap}"
        );

        // Home full layout starts sidebar content below the 7-row header.
        // Edge x=25 is the default 26-column sidebar border; drag to x=39
        // requests a 40-column sidebar on this 120-column tmux pane.
        send_sgr_mouse(&session, 0, 25, 10, true);
        send_sgr_mouse(&session, 32, 39, 10, true);
        send_sgr_mouse(&session, 0, 39, 10, false);

        let persisted = session.poll(Instant::now() + Duration::from_secs(10), |_| {
            config_text(home_tmp.path()).contains("home_sidebar_width = 40")
        });
        assert!(
            persisted.is_some(),
            "sidebar width was not persisted after mouse drag; config:\n{}\npane:\n{}",
            config_text(home_tmp.path()),
            session.capture()
        );
    }

    {
        let session = TmuxSession::new(format!("{}-restore", session_name));
        let cmd = format!(
            "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
            home_tmp.path().display(),
            ainb.display()
        );
        session.send_keys(&[&cmd, "Enter"]);

        let restored = session.poll(Instant::now() + Duration::from_secs(45), |c| {
            c.contains("Catalog") && c.contains("[c]") && c.contains("Sessions")
        });
        assert!(
            restored.is_some(),
            "HomeScreen did not render after relaunch with persisted sidebar width:\n{}",
            session.capture()
        );
        assert!(
            config_text(home_tmp.path()).contains("home_sidebar_width = 40"),
            "relaunch lost persisted sidebar width:\n{}",
            config_text(home_tmp.path())
        );
    }
}
