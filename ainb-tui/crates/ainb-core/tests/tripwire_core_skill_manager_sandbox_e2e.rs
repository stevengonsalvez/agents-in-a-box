//! Tripwire: spawn the real `ainb` binary against the sandbox
//! fixture, press `m`, capture the pane, assert the SkillManager
//! screen shows the seeded source name + Sources/Units/Detail panel
//! chrome.
//!
//! Proves end-to-end that the fixture's env-var set (HOME +
//! AINB_HOME + AINB_TOOL_HOME_*) is enough to redirect the binary
//! away from the real ~/.claude and onto the seeded sandbox.
//! Complements the in-process tripwire (`*_sandbox_loads.rs`).
//!
//! Bead `ai-e7t`. Skips when `tmux` is unavailable on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_skill_core::{build_skill_manager_sandbox, SandboxLayout, SandboxTier};

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

/// Seed the onboarding marker so the first-run wizard doesn't steal
/// our keystrokes. Same pattern as every other live tmux tripwire.
fn seed_isolated_home(layout: &SandboxLayout) {
    let cfg = layout.root.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
    // Suppress the first-run ainb-hooks install nudge (intercepts the
    // first key on the home screen). See build_skill_manager_sandbox.
    std::fs::write(cfg.parent().unwrap().join("install.json"), "{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n")
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
        thread::sleep(Duration::from_millis(400));
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
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

/// Quote a path for shell so spaces / special chars in the tempdir
/// path don't bork the `HOME=… exec ainb …` line the test sends.
fn sh_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    // Bash-safe — wrap in single quotes, double-up any single quote inside.
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[test]
fn pressing_m_in_real_binary_against_sandbox_renders_skill_manager() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("home tempdir");
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Full)
        .expect("sandbox full");
    seed_isolated_home(&layout);

    let session = format!("tripwire-sm-sandbox-{}", std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Construct the launch line: every env var from the fixture's
    // env_vars() shipped on the same command. This is the exact set
    // the bash launcher writes into env.sh — proves the fixture
    // contract end-to-end against the real binary.
    let mut launch = String::new();
    for (k, v) in layout.env_vars() {
        launch.push_str(k);
        launch.push('=');
        launch.push_str(&sh_quote(Path::new(&v)));
        launch.push(' ');
    }
    launch.push_str("exec ");
    launch.push_str(&sh_quote(&bin));
    launch.push_str(" 2>&1");

    Command::new("tmux")
        .args(["send-keys", "-t", &session, &launch, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to render.
    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    });
    if home_render.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    send_key(&session, "m");

    // After [m] we expect SkillManager chrome + the seeded `sandbox`
    // source name (from Full tier's pre-seeded manifest.yaml). All
    // three panes + the help bar must be alive.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources")
            && c.contains("Units")
            && c.contains("Detail")
            && c.contains("[i]")
            && c.contains("sandbox")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "SkillManager screen never rendered with sandbox source after pressing M.\n\
                 sandbox root: {}\n\
                 last capture:\n{dump}",
                layout.root.display()
            );
        }
    };
    kill_session(&session);

    assert!(post.contains("Sources"), "missing Sources panel: {post}");
    assert!(post.contains("Units"), "missing Units panel: {post}");
    assert!(post.contains("Detail"), "missing Detail pane: {post}");
    assert!(
        post.contains("sandbox"),
        "Sources panel missing seeded source name `sandbox`: {post}"
    );
    assert!(
        post.contains("initial-skill"),
        "Units panel missing seeded unit `initial-skill`: {post}"
    );
}
