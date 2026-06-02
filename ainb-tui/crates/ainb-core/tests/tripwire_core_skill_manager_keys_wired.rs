//! Tripwire: the SkillManager help-bar keys are actually wired —
//! before this work they were advertised in the help bar but dropped
//! on the floor. Drives the real `ainb` binary against the sandbox
//! fixture via tmux.
//!
//! Coverage in THIS file (two assertions that prove wiring
//! end-to-end):
//!   1. `[i]` opens the add-source prompt; typing a `git:file://` URI
//!      (which contains `:` — the char that used to hijack the global
//!      slash-command palette) and pressing Enter actually writes the
//!      source into manifest.yaml.
//!   2. `[/]` opens the search prompt and filters the Units table.
//!
//! The other help-bar keys have their own live tripwires so each file
//! stays focused:
//!   * `[c] check`  → `tripwire_core_skill_manager_check_live.rs`
//!     (asserts the "drift check running" info toast surfaces).
//!   * `[m] refresh` → `tripwire_core_skill_manager_sandbox_e2e.rs`
//!     (presses `m` and asserts the SkillManager screen renders).
//!
//! Bead: skill-manager key wiring (follow-up to ai-e7t). Skips when
//! tmux is unavailable.

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

fn seed_onboarding(layout: &SandboxLayout) {
    let cfg = layout.root.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&cfg).expect("config dir");
    let onboarding = format!(
        "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(cfg.join("onboarding.toml"), onboarding).expect("onboarding");
}

fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("capture-pane");
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
        .expect("send-keys");
}

/// Send a literal string (no key-name interpretation) — needed for
/// URIs containing `:` and `/`.
fn send_literal(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("send-keys -l");
}

fn kill(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

fn launch_line(layout: &SandboxLayout, bin: &Path) -> String {
    let mut s = String::new();
    for (k, v) in layout.env_vars() {
        s.push_str(k);
        s.push('=');
        s.push('\'');
        s.push_str(&Path::new(&v).to_string_lossy());
        s.push('\'');
        s.push(' ');
    }
    s.push_str("exec '");
    s.push_str(&bin.to_string_lossy());
    s.push_str("' 2>&1");
    s
}

#[test]
fn add_source_key_writes_manifest_in_live_binary() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal)
        .expect("sandbox");
    seed_onboarding(&layout);

    let session = format!("tripwire-sm-keys-add-{}", std::process::id());
    let bin = ainb_bin();
    assert!(
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
            .status()
            .expect("new-session")
            .success()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &launch_line(&layout, &bin), "Enter"])
        .status()
        .expect("launch");

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

    // Minimal tier → empty manifest → discovery banner pops. Skip it
    // so we're on the units screen with the empty-state help.
    if poll(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("import them?") || c.contains("No units installed")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("skill manager never rendered:\n{d}");
    }
    thread::sleep(Duration::from_millis(200));
    send(&session, "s"); // skip banner if present
    thread::sleep(Duration::from_millis(600));

    // [i] → add-source prompt.
    send(&session, "i");
    if poll(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("Add source")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("[i] did not open add-source prompt:\n{d}");
    }

    // Type the bare-remote URI (contains `:` and `/`) + Enter.
    let uri = format!("git:file://{}", layout.bare_remote.display());
    send_literal(&session, &uri);
    thread::sleep(Duration::from_millis(400));
    send(&session, "Enter");

    // The manifest must gain the source. Poll the file directly.
    let manifest = layout.ainb_home.join("manifest.yaml");
    let mut wrote = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if text.contains("sandbox-remote.git") && text.contains("sources:") {
                wrote = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(400));
    }
    let dump = capture(&session);
    kill(&session);
    assert!(
        wrote,
        "[i] add-source did not write the source into manifest.yaml.\n\
         (regression: the `:` in the URI likely hijacked the global \
         slash-command palette again).\nlast pane:\n{dump}"
    );
}

#[test]
fn search_key_filters_units_in_live_binary() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    // Full tier pre-seeds a manifest with units, so the Units table
    // has rows to filter and no banner intercepts.
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Full)
        .expect("sandbox full");
    seed_onboarding(&layout);

    let session = format!("tripwire-sm-keys-search-{}", std::process::id());
    let bin = ainb_bin();
    assert!(
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
            .status()
            .expect("new-session")
            .success()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &launch_line(&layout, &bin), "Enter"])
        .status()
        .expect("launch");

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
    if poll(&session, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("initial-skill")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("units table with initial-skill never rendered:\n{d}");
    }

    // [/] → search prompt → type a query that matches nothing → Enter.
    thread::sleep(Duration::from_millis(200));
    send(&session, "/");
    if poll(&session, Instant::now() + Duration::from_secs(10), |c| {
        c.contains("Search units")
    })
    .is_none()
    {
        let d = capture(&session);
        kill(&session);
        panic!("[/] did not open search prompt:\n{d}");
    }
    send_literal(&session, "zzzznomatch");
    thread::sleep(Duration::from_millis(500));
    send(&session, "Enter");

    // The Detail pane (bottom of the screen) keeps showing the
    // selected unit's name even when the Units TABLE is filtered, so
    // we must scope the assertion to the Units-table region only.
    // The top ~60% of the screen is the Sources|Units row; the
    // Detail pane is the lower band. Check the first 20 rows (well
    // within the Units table, clear of the Detail pane on a 50-row
    // terminal).
    let units_region = |full: &str| -> String {
        full.lines().take(20).collect::<Vec<_>>().join("\n")
    };
    let cleared = poll(&session, Instant::now() + Duration::from_secs(20), |c| {
        !c.contains("Search units") && !units_region(c).contains("initial-skill")
    });
    let dump = capture(&session);
    kill(&session);
    assert!(
        cleared.is_some(),
        "[/] search did not filter the Units table — `initial-skill` still in the \
         Units region after a non-matching query.\nlast pane:\n{dump}"
    );
}
