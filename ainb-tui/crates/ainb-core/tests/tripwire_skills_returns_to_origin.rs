//! Tripwire (overlay-panels): the Skills panel opens from BOTH the home
//! menu and the session list, and Esc closes it back to whichever screen
//! it was opened from.
//!
//! Skills is a built-in host screen (not a wire plugin), so it needs no
//! staged plugins — we launch with `AINB_DISABLE_PLUGINS=1` to keep the
//! test fast and to prove the host's own return-to-origin wiring
//! (`GoToSkills` saves `previous_screen`; `SkillsBack` routes through
//! `PanelBack`) works independently of the plugin runtime.
//!
//! The pairing here is the whole point (see the skill's rule 6): a
//! forward-only "press k → skills rendered" test passes even when the
//! return path silently breaks. Each test drives the full round trip and
//! asserts the ORIGIN screen — home for the home-origin test, the
//! session list for the session-list-origin test — proving the host pops
//! `previous_screen` rather than hardcoding a destination.
//!
//! Skips gracefully if `tmux` isn't on `$PATH`.

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
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
    // Suppress the ainb-hooks first-run install dialog — it overlays the
    // home screen and swallows the first keystroke this tripwire sends.
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");
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

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

/// Launch ainb (plugins disabled) in a detached tmux session, returning
/// once the HomeScreen has rendered. Panics with the last capture if it
/// never appears.
fn launch_to_home(session: &str, home: &Path) {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.display(),
        ainb_bin().display()
    );
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

/// `Provider:` + `Associations` are painted only by the Skills SCREEN's
/// provider strip + tab row — neither appears on the home sidebar (which
/// merely lists a `Skills [k]` tile) nor on the session list. So this
/// pair uniquely identifies "we are on the skills screen".
fn on_skills_screen(c: &str) -> bool {
    c.contains("Provider:") && c.contains("Associations")
}

#[test]
fn skills_from_home_returns_home() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let session = format!("tripwire-skills-home-{}", std::process::id());
    launch_to_home(&session, home_tmp.path());

    // Pre-press: not already on skills.
    let pre = capture_pane(&session);
    assert!(
        !on_skills_screen(&pre),
        "pre-key capture already on skills — state leaked:\n{pre}"
    );

    // Open skills from the home menu.
    send_key(&session, "k");
    if poll_capture(
        &session,
        Instant::now() + Duration::from_secs(40),
        on_skills_screen,
    )
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("skills screen never rendered after `k`; last:\n---\n{last}\n---");
    }

    // Esc at the skills root closes back to home.
    send_key(&session, "Escape");
    let back = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("Stats") && c.contains("[i]") && !on_skills_screen(c)
    });
    let final_cap = capture_pane(&session);
    kill_session(&session);
    assert!(
        back.is_some(),
        "Esc on skills (opened from home) did not return to home within 10s. \
         Final capture:\n---\n{final_cap}\n---"
    );
}

#[test]
fn skills_from_session_list_returns_to_session_list() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let session = format!("tripwire-skills-sessions-{}", std::process::id());
    launch_to_home(&session, home_tmp.path());

    // Hop to the session list first. `del-sel` is on the session-list
    // legend (line 2) and appears on no other screen.
    send_key(&session, "s");
    if poll_capture(&session, Instant::now() + Duration::from_secs(40), |c| {
        c.contains("del-sel")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("session list never rendered after `s`; last:\n---\n{last}\n---");
    }

    // Open skills from the session list (the `k` shortcut is mirrored
    // here, same as the home menu).
    send_key(&session, "k");
    if poll_capture(
        &session,
        Instant::now() + Duration::from_secs(40),
        on_skills_screen,
    )
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("skills screen never rendered after `k` from session list; last:\n---\n{last}\n---");
    }

    // Esc must return to the SESSION LIST (not home) — proving the host
    // pops the saved `previous_screen`.
    send_key(&session, "Escape");
    let back = poll_capture(&session, Instant::now() + Duration::from_secs(25), |c| {
        c.contains("del-sel") && !on_skills_screen(c)
    });
    let final_cap = capture_pane(&session);
    kill_session(&session);
    assert!(
        back.is_some(),
        "Esc on skills (opened from session list) did not return to the session list \
         within 10s. Final capture:\n---\n{final_cap}\n---"
    );
}
