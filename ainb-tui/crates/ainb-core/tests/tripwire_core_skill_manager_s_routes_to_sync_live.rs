//! Tripwire: pressing `[s]` on the SkillManager screen routes to
//! `SkillManagerSync` when the selected unit has NO `shadowed_by`
//! peer — observable in the live binary via the
//! `sync: <unit-name>` info notification.
//!
//! The conflict-flip path already has live coverage via
//! tripwire_core_skill_manager_conflict_flip.rs. This tripwire pairs
//! with that one so BOTH `[s]` branches (Sync vs ConflictFlip) have
//! end-to-end live coverage in tmux.
//!
//! Bead v12.1.T3 / agents-in-a-box-03u. Pattern mirrors sibling live
//! tmux tripwires.
//!
//! Skips when `tmux` is unavailable on PATH.

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
    // Suppress the first-run ainb-hooks install nudge (intercepts the
    // first key on the home screen). See build_skill_manager_sandbox.
    fs::write(
        cfg.parent().unwrap().join("install.json"),
        "{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n",
    )
    .expect("seed install.json");
}

/// Seed manifest + lockfile with a single unit that has NO
/// `shadowed_by` peer — the `[s]` keybind handler in
/// app/events.rs at the SkillManager match arm chooses
/// `SkillManagerConflictFlip` when a peer exists and
/// `SkillManagerSync` otherwise. The single-unit fixture here pins
/// the no-peer branch.
fn seed_manifest_and_lockfile(home: &Path) {
    let ainb_home = home.join(".agents-in-a-box");
    fs::create_dir_all(&ainb_home).expect("create ainb_home");

    let manifest_yaml = r#"schema_version: 1
sources:
  - name: local-skills
    type: local
    uri: local:~/.claude/skills
    ref: head
    enabled: true
units:
  - uri: local:~/.claude/skills@head/commit
    targets: [claude]
"#;
    fs::write(ainb_home.join("manifest.yaml"), manifest_yaml).expect("seed manifest.yaml");

    let lock_yaml = r#"schema_version: 1
generated_at: "2026-05-26T00:00:00+00:00"
sources: []
units:
  - uri: local:~/.claude/skills@head/commit
    declared_uri: local:~/.claude/skills@head/commit
    kind: skill
    sha: abc123
    deployed:
      claude:
        status: deployed
        path: /tmp/tripwire-sync-deployed/claude/skills/commit
        file_hashes:
          SKILL.md: deadbeef
"#;
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
fn pressing_s_routes_to_sync_in_live_binary_when_no_shadow_peer() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_manifest_and_lockfile(home_tmp.path());

    let session = format!("tripwire-sm-sync-{}", std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let ainb_home = home_tmp.path().join(".agents-in-a-box");
    let cmd = format!(
        "unset XDG_CONFIG_HOME; HOME={} AINB_HOME={} exec {} 2>&1",
        home_tmp.path().display(),
        ainb_home.display(),
        bin.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen to fully render before sending M.
    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    });
    if home_render.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    send_key(&session, "z");

    // Wait for SkillManager to be fully painted (seeded source +
    // unit name both rendered) before pressing [s], else the
    // keystroke may arrive before the unit-table has a selection.
    let after_m = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources") && c.contains("Units") && c.contains("Detail") && c.contains("commit")
    });
    if after_m.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("SkillManager screen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    // Press 's' (lowercase, no Enter — see tripwire-skill hard rule
    // #3). With NO shadowed_by peer on the selected unit, the
    // handler routes to SkillManagerSync, which surfaces an
    // `already in sync` info notification for this local fixture.
    send_key(&session, "s");

    let post = poll_capture(&session, Instant::now() + Duration::from_secs(45), |c| {
        c.contains("already in sync") && c.contains("commit")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "Live binary did not surface `already in sync` notification after [s]. \
                 last capture:\n{dump}"
            );
        }
    };
    kill_session(&session);

    // Positive — the sync routing fired and produced the
    // user-visible notification we assert on.
    assert!(
        post.contains("already in sync"),
        "missing `already in sync` notification: {post}"
    );
    assert!(
        post.contains("commit"),
        "notification missing seeded unit name: {post}"
    );

    // Negative — pressing [s] with no shadow peer must NOT toggle
    // a conflict-flip status. The conflict-flip user-visible
    // signal is the deployment status flipping; in this single-
    // unit fixture there's no peer to flip TO, so the deployed
    // path the Detail pane shows must still be the seeded one.
    assert!(
        post.contains("/tmp/tripwire-sync-deployed/claude/skills/commit"),
        "Detail pane lost its deployed path after [s] press — conflict-flip path may have been taken: {post}"
    );
}
