//! Shared fixture helpers for the `tripwire_new_session_*` integration tests.
//!
//! Finding #13: 15 tripwire files used to inline the same `ainb_bin`,
//! `tmux_available`, `seed_isolated_home`, `seed_new_session_fixtures`,
//! `capture`, `poll_capture`, `send_key`, `kill_session` helpers. Each is now
//! sourced from this module via:
//!
//! ```ignore
//! #[allow(dead_code)]
//! mod tripwire_new_session_common;
//! use tripwire_new_session_common::*;
//! ```
//!
//! Matches the existing `tripwire_helpers.rs` precedent already used by
//! `tripwire_crash_recovery.rs`, `tripwire_nonblocking.rs`, and
//! `tripwire_reproduced.rs`.
//!
//! `#[allow(dead_code)]` on the `mod` line is REQUIRED in each consumer
//! because Rust treats every integration-test file as its own crate root and
//! marks unused items dead per-file. Some tripwires use only a subset of
//! these helpers.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// Absolute path to the built `ainb` binary under test.
pub fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Write a fake `gh` onto an isolated bin dir that behaves like a
/// signed-in CLI: `gh auth status …` exits 0. Returns the bin dir to
/// prepend onto PATH.
///
/// Entering the seeded GitHub-shorthand favorite triggers the
/// `gh auth status` pre-check (`StartClone` → `CheckGitAuth`). Under an
/// isolated `$HOME` the box's real `gh` has no usable credentials there, so
/// the probe fails closed and parks the flow on the "GitHub auth required"
/// modal instead of advancing to Configure. A stub that exits 0 lets the
/// pre-check pass so the flow reaches the state each tripwire exercises.
/// Mirrors the logged-out `seed_logged_out_gh` in
/// `tripwire_new_session_github_auth_no_gh.rs`.
pub fn seed_logged_in_gh(home: &Path) -> PathBuf {
    let bin = home.join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\n\
         # Stub: simulates `gh` installed and signed in.\n\
         echo 'Logged in to github.com account trip (keyring)' 1>&2\n\
         exit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// Standard TUI launch command for the isolated-HOME tripwires, with a
/// signed-in `gh` stub prepended onto PATH so the GitHub auth pre-check
/// passes (see [`seed_logged_in_gh`]). `$PATH` expands in the launch shell
/// before `exec` replaces it, so keystrokes still hit `ainb` directly.
pub fn launch_cmd_gh_authed(home: &Path, bin: &Path) -> String {
    let fakebin = seed_logged_in_gh(home);
    format!(
        "HOME={home} PATH={fake}:$PATH AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        fake = fakebin.display(),
        bin = bin.display(),
    )
}

/// True when `tmux` is on PATH and responsive — gates the PTY tripwires
/// so CI hosts without tmux skip rather than fail.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Seed an isolated `$HOME` with the minimum config needed to bypass the
/// onboarding flow. Tripwires can layer additional fixtures on top
/// (favorites.yaml, session-defaults.yaml, etc.) via the `seed_*` helpers
/// or their own inline writes.
pub fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).unwrap();
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{}"
skipped_dependencies = []
git_directories = []
"#,
        env!("CARGO_PKG_VERSION")
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).unwrap();
    let install_record = r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");
}

/// Seed `favorites.yaml` with a single `ainb-tui` GithubShorthand entry so
/// the picker renders at least one ★ favorite row, AND seed the four shipped
/// default presets into `~/.agents-in-a-box/presets.toml` so the Configure
/// screen has the canonical preset ring to cycle through. The vast majority
/// of new-session tripwires need this exact fixture; bespoke tripwires can
/// override by writing the file themselves after calling `seed_isolated_home`.
pub fn seed_new_session_fixtures(home: &Path) {
    let root = home.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    let favorites_yaml = r#"version: 1
favorites:
  - alias: ainb-tui
    source_type: github_shorthand
    source: stevengonsalvez/ainb-tui
    stats:
      created_at: 2026-05-01T00:00:00Z
      last_used: 2026-05-01T00:00:00Z
      use_count: 0
settings:
  auto_promote_threshold: 5
"#;
    fs::write(root.join("favorites.yaml"), favorites_yaml).unwrap();
    seed_default_presets(home);
}

/// Seed the four shipped default presets into the single-file
/// `~/.agents-in-a-box/presets.toml`. Idempotent — overwrites any existing
/// file under the isolated test HOME.
pub fn seed_default_presets(home: &Path) {
    let root = home.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    let presets_toml = r#"# Shipped default presets (seeded by tripwire fixtures).

[[preset]]
name = "claude-interactive-yolo"
description = "Claude Code, interactive REPL with bypass permissions"
agent_provider = "claude"
agent_model = "default"
mode = "interactive"
[preset.permissions]
skip_all = true

[[preset]]
name = "codex-interactive-yolo"
description = "Codex CLI, interactive REPL with bypass permissions"
agent_provider = "codex"
agent_model = "default"
mode = "interactive"
[preset.permissions]
skip_all = true

[[preset]]
name = "opusplan"
description = "Claude Code with opusplan hybrid (Opus plans, Sonnet executes), Boss mode, bypass permissions"
agent_provider = "claude"
agent_model = "opusplan"
mode = "boss"
[preset.permissions]
skip_all = true

[[preset]]
name = "shell"
description = "Plain shell in the worktree; no agent"
agent_provider = "shell"
agent_model = "default"
mode = "interactive"
[preset.permissions]
"#;
    fs::write(root.join("presets.toml"), presets_toml).unwrap();
}

/// Capture the current tmux pane contents as a UTF-8 String.
pub fn capture(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Poll `capture` at ~2.5Hz until the predicate fires or `deadline` passes.
pub fn poll_capture<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let c = capture(session);
        if ok(&c) {
            return Some(c);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

/// Send a single key (or key-name like `"Escape"`, `"BSpace"`) to the tmux
/// session. Wraps `tmux send-keys -t <session> <key>`.
pub fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

/// Idempotent cleanup — never panics on a missing session.
pub fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

/// Send a multi-character string by issuing a single `send-keys -l` (literal)
/// — avoids tmux interpreting key names embedded in the string.
pub fn send_text(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux send-keys text");
}

/// True iff `git` is on PATH (gates tripwires that seed a real local repo).
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Seed an empty git repository under `<home>/projects/seeded-repo` and
/// return the absolute path. Used by tripwires that exercise the LocalPath
/// dispatch arm.
pub fn seed_local_git_repo(home: &Path) -> PathBuf {
    let repo = home.join("projects").join("seeded-repo");
    fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    // Make a first commit so HEAD resolves cleanly for branch_namer / git2.
    fs::write(repo.join("README.md"), "seeded\n").unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=trip@example.com",
            "-c",
            "user.name=trip",
            "add",
            "README.md",
        ])
        .current_dir(&repo)
        .status();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=trip@example.com",
            "-c",
            "user.name=trip",
            "commit",
            "-m",
            "seed",
        ])
        .current_dir(&repo)
        .status();
    repo
}

/// Write a `favorites.yaml` whose single entry points at the on-disk
/// `seeded-repo`. Replaces the GithubShorthand default used by
/// `seed_new_session_fixtures` for tripwires that need a real LocalPath.
pub fn seed_favorites_pointing_at(home: &Path, repo: &Path) {
    let root = home.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    let yaml = format!(
        r#"version: 1
favorites:
  - alias: seeded-repo
    source_type: local_path
    source: {}
    stats:
      created_at: 2026-05-01T00:00:00Z
      last_used: 2026-05-01T00:00:00Z
      use_count: 0
settings:
  auto_promote_threshold: 5
"#,
        repo.display()
    );
    fs::write(root.join("favorites.yaml"), yaml).unwrap();
}
