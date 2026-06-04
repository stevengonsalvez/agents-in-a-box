//! Tripwire: pressing `M` on the HomeScreen and landing on the
//! SkillManager Detail pane renders the usage telemetry line — the
//! literal "<N> invocations" must appear in the captured tmux pane.
//!
//! Catches: SkillsScreenData::reload_from_disk regression that drops
//! the `usage` projection from LockedUnit, format_time_ago panic on
//! a malformed RFC 3339 input, Detail-pane render branch deleted, or
//! the line_kv("Usage", …) call being short-circuited away. The
//! existing TestBackend-only tripwire (tripwire_core_skill_manager_
//! usage_renders.rs) proves the render works in isolation; this
//! tripwire proves the LIVE binary actually surfaces the line on
//! screen open.
//!
//! Bead v12.1.T1 / agents-in-a-box-2ss. Pattern mirrors sibling
//! tripwire_core_skill_manager_screen_opens.rs.
//!
//! Skips when `tmux` isn't on PATH.

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

/// Seed an isolated `$HOME` so the first-run onboarding wizard does
/// not intercept our keystrokes.
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
}

/// Seed manifest + lock with a single unit carrying
/// `usage: { last_used_at: 3h-ago, invocations: 12 }`. The literal
/// `12 invocations` is what the test asserts on, derived from the
/// `LockedUnit::usage.invocations` projection feeding the Detail pane.
fn seed_manifest_and_lockfile(home: &Path) {
    let ainb_home = home.join(".agents-in-a-box");
    fs::create_dir_all(&ainb_home).expect("create ainb_home");

    let manifest_yaml = r#"schema_version: 1
sources:
  - name: stevie-skills
    type: gh
    uri: gh:stevie/skills
    ref: main
    enabled: true
units:
  - uri: gh:stevie/skills@main/skills/commit
    targets: [claude]
"#;
    fs::write(ainb_home.join("manifest.yaml"), manifest_yaml).expect("seed manifest.yaml");

    // 3h-ago RFC 3339 timestamp. chrono is already on the workspace
    // (used by ainb-core); test-only use is free.
    let three_hours_ago = chrono::Utc::now()
        - chrono::Duration::try_hours(3).expect("3h fits in Duration");
    let last_used = three_hours_ago.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let lock_yaml = format!(
        r#"schema_version: 1
generated_at: "2026-05-26T00:00:00+00:00"
sources: []
units:
  - uri: gh:stevie/skills@abc123/skills/commit
    declared_uri: gh:stevie/skills@main/skills/commit
    kind: skill
    sha: abc123
    deployed:
      claude:
        status: deployed
        path: /tmp/tripwire-deployed/claude/skills/commit
        file_hashes:
          SKILL.md: deadbeef
    usage:
      last_used_at: "{last_used}"
      invocations: 12
"#
    );
    fs::write(ainb_home.join("lock.yaml"), lock_yaml).expect("seed lock.yaml");
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
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn pressing_m_renders_usage_invocations_in_live_binary() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_manifest_and_lockfile(home_tmp.path());

    let session = format!("tripwire-sm-usage-{}", std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} exec {} 2>&1",
        home_tmp.path().display(),
        bin.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to fully render before sending M.
    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Welcome to AINB")
    });
    if home_render.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    send_key(&session, "m");

    // Wait for the SkillManager screen + Detail pane to surface the
    // "12 invocations" literal. The Detail pane auto-binds to the
    // first unit when no row is explicitly selected, so we don't
    // need to navigate.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources")
            && c.contains("Units")
            && c.contains("Detail")
            && c.contains("12 invocations")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "SkillManager Detail pane never showed '12 invocations' in live binary.\n\
                 last capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    // Direct positive assertions on the live render — substring not
    // OR'd with anything else so the literal is the load-bearing
    // signal.
    assert!(
        post.contains("12 invocations"),
        "Detail pane did not render the usage invocation count: {post}"
    );
    assert!(
        post.contains("Usage"),
        "Detail pane usage label missing: {post}"
    );
    // The "last used" prefix is rendered next to the time-ago string;
    // we don't pin the exact "3h ago" / "4h ago" boundary because the
    // formatter rounds, but the label must be there.
    assert!(
        post.contains("last used"),
        "Detail pane usage line missing 'last used' segment: {post}"
    );
}
