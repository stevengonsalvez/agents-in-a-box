//! Tripwire: the SkillManager `[l]` Library view — bead ai-lgk.
//!
//! Drives the real `ainb` binary against the Full-tier sandbox via
//! tmux:
//!   1. `m` → SkillManager screen.
//!   2. `l` → the own-skill Library view renders, listing the
//!      `library.yaml`-registered own-skill the Full tier seeds
//!      (`my-own-skill`).
//!   3. `Enter` → the selected own-skill's Detail surface appears
//!      (scoped to the Library overlay region so a stale Units/Detail
//!      pane row can't satisfy it).
//!
//! Mirrors the EXACT launch + onboarding-seed + capture-pane pattern of
//! `tripwire_core_skill_manager_sandbox_e2e.rs` /
//! `tripwire_core_skill_manager_keys_wired.rs` /
//! `tripwire_core_skill_manager_nav_keys_live.rs`. Build the binary
//! under the real HOME; exec with HOME set to the sandbox tempdir.
//! Skips cleanly when `tmux` is unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::{build_skill_manager_sandbox, SandboxLayout, SandboxTier, OWN_SKILL_NAME};

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

/// Seed the onboarding marker so the first-run wizard doesn't steal our
/// keystrokes. Same pattern as every other live tmux tripwire.
fn seed_onboarding(layout: &SandboxLayout) {
    let cfg = layout.root.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll<F: FnMut(&str) -> bool>(session: &str, deadline: Instant, mut ok: F) -> Option<String> {
    while Instant::now() < deadline {
        let c = capture(session);
        if ok(&c) {
            return Some(c);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn send(session: &str, keys: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, keys])
        .status()
        .expect("tmux send-keys");
}

fn kill(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

fn sh_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn launch_line(layout: &SandboxLayout, bin: &Path) -> String {
    let mut s = String::new();
    for (k, v) in layout.env_vars() {
        s.push_str(k);
        s.push('=');
        s.push_str(&sh_quote(Path::new(&v)));
        s.push(' ');
    }
    s.push_str("exec ");
    s.push_str(&sh_quote(bin));
    s.push_str(" 2>&1");
    s
}

#[test]
fn library_view_renders_owned_skill_and_enter_shows_detail() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    // Full tier seeds library.yaml with one own-skill (`my-own-skill`)
    // plus its SKILL.md under .claude/skills/.
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("sandbox full");
    seed_onboarding(&layout);

    let session = format!("tripwire-sm-library-{}", std::process::id());
    let bin = ainb_bin();

    assert!(
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
            .status()
            .expect("tmux new-session")
            .success(),
        "tmux new-session failed"
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &launch_line(&layout, &bin), "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Home → SkillManager.
    if poll(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Welcome to AINB")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("home never rendered:\n{d}");
    }
    thread::sleep(Duration::from_millis(200));
    send(&session, "m");

    // SkillManager renders (Full tier → units present, no banner).
    if poll(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Units") && c.contains("Detail")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("SkillManager never rendered:\n{d}");
    }

    // ── [l] → Library view, listing the seeded own-skill.
    thread::sleep(Duration::from_millis(200));
    send(&session, "l");
    let lib_view = poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        c.contains("Library") && c.contains(OWN_SKILL_NAME)
    });
    if lib_view.is_none() {
        let d = capture(&session);
        kill(&session);
        panic!(
            "[l] did not open the Library view with the seeded own-skill `{OWN_SKILL_NAME}`.\n\
             last pane:\n{d}"
        );
    }

    // ── Enter → the own-skill's Detail surface inside the Library view.
    thread::sleep(Duration::from_millis(300));
    send(&session, "Enter");
    let detail = poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        // The Library overlay renders a "Library Detail" marker on
        // Enter; the own-skill name + its tool-home-relative path
        // (`.claude/skills/my-own-skill`) appear in that surface.
        c.contains("Library Detail")
            && c.contains(OWN_SKILL_NAME)
            && c.contains(".claude/skills/my-own-skill")
    });
    let dump = capture(&session);
    kill(&session);
    assert!(
        detail.is_some(),
        "Enter in the Library view did not surface the own-skill Detail (name + \
         `.claude/skills/my-own-skill`).\nlast pane:\n{dump}"
    );
}
