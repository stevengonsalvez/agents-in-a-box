//! Tripwire: `ainb run --tool codex` with NO shared remote thread still puts a
//! usable Codex in the pane, and keeps the worktree it just created. Real
//! tmux, real `ainb` binary, fake `codex` on PATH.
//!
//! Two regressions meet here, both of which a unit test reports green on:
//!
//!   * The launch used to FAIL when the Hangar daemon was unreachable, and its
//!     failed-session cleanup then deleted the worktree the run had created
//!     seconds earlier. The daemon is load-bearing for the SHARED thread only.
//!   * The degraded argv used to come from the provider-generic builder, which
//!     emits neither `-c check_for_update_on_startup=false` nor
//!     `--dangerously-bypass-hook-trust`. Both suppress modals that STALL the
//!     pane rather than failing it, so `ainb run` reported a session created
//!     over a session parked on a prompt nobody is watching.
//!
//! What this pins vs what the unit tests pin: the degrade here comes from an
//! ephemeral hangar home (`$HOME` under the temp dir), which is the one cause
//! that reaches the same `Ok(None)` WITHOUT spawning a daemon into a CI runner.
//! The connect-class causes (`Connect`, `NoHome`, `Token`) are driven directly
//! in `session_manager`'s unit tests; from the argv down, every degraded launch
//! is this one.
//!
//! Harness notes follow `tripwire_cli_run_prompt.rs`: private tmux server via
//! `TMUX_TMPDIR` (the env var, not `-L`, because `ainb run` shells out to tmux
//! itself), `TMUX` removed so an ambient pane cannot reattach us to the shared
//! server, a tempdir `$HOME` with completed onboarding, and cleanup that kills
//! the session by EXACT name, never the server.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// Private tmux socket dir: pid + nanos, under /tmp (macOS caps unix socket
/// paths at 104 bytes and `$TMPDIR` there already burns ~50 of them).
fn private_tmux_dir() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let dir = PathBuf::from("/tmp").join(format!(
        "ainb-tripwire-codex-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create private tmux socket dir");
    dir
}

fn tmux(tmux_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .env("TMUX_TMPDIR", tmux_dir)
        .env_remove("TMUX")
        .args(args)
        .output()
        .expect("run tmux")
}

/// Fake Codex: prints the argv it was given, then holds the pane open the way
/// the real CLI does. The printed line is the assertion surface: a flag missing
/// from the launch cannot appear in it.
fn write_fake_codex(bin_dir: &Path) {
    let path = bin_dir.join("codex");
    let mut f = std::fs::File::create(&path).expect("create fake codex");
    f.write_all(
        b"#!/bin/sh\n\
          printf 'CODEX ARGV:%s\\n' \" $*\"\n\
          while :; do sleep 1; done\n",
    )
    .expect("write fake codex");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake codex");
}

fn seed_onboarding(home: &Path) {
    let cfg = home.join(".agents-in-a-box/config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    std::fs::write(
        cfg.join("onboarding.toml"),
        format!(
            r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
            ver = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
}

#[test]
fn a_codex_run_without_a_shared_thread_launches_and_keeps_its_worktree() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let tmux_dir = private_tmux_dir();
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");
    seed_onboarding(home.path());
    write_fake_codex(home.path()); // HOME doubles as the fake-bin dir

    // A Codex home of our own, with a config file present: the trust write is
    // deliberately a no-op when Codex has never written one, so an absent file
    // would make this assertion pass for the wrong reason. It also keeps the
    // write off the developer's real `~/.codex/config.toml`.
    let codex_home = home.path().join("codex-home");
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    std::fs::write(codex_home.join("config.toml"), "").expect("seed codex config");

    // A real repo with one commit: `--worktree` branches off it.
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "tripwire@example.com"],
        vec!["config", "user.name", "tripwire"],
    ] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(repo.path())
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?} failed");
    }
    std::fs::write(repo.path().join("README.md"), "tripwire\n").expect("seed repo file");
    for args in [
        vec!["add", "README.md"],
        vec!["commit", "-qm", "init", "--no-gpg-sign"],
    ] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(repo.path())
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?} failed");
    }

    let session = format!("tripwire-codex-degraded-{}", std::process::id());
    let tmux_session = ainb::tmux::sanitize_session_name(&session);
    let path_env = format!(
        "{}:{}",
        home.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::new(env!("CARGO_BIN_EXE_ainb"))
        .args([
            "run",
            "--tool",
            "codex",
            "--repo",
            &repo.path().display().to_string(),
            "--worktree",
            "--name",
            &session,
        ])
        .env("HOME", home.path())
        // `$HOME` under the temp dir, and no `$AINB_HANGAR_HOME`, is the one
        // shape the autostart refuses to spawn a daemon for. Without this a CI
        // runner ends up with a live daemon nobody stops.
        .env_remove("AINB_HANGAR_HOME")
        .env("AINB_HOME", home.path())
        .env("CODEX_HOME", &codex_home)
        .env("PATH", &path_env)
        .env("TMUX_TMPDIR", &tmux_dir)
        .env_remove("TMUX")
        .output()
        .expect("ainb run");

    let pane = pane_argv(&tmux_dir, &tmux_session);
    let _ = tmux(&tmux_dir, &["kill-session", "-t", &tmux_session]);
    let _ = std::fs::remove_dir_all(&tmux_dir);

    assert!(
        out.status.success(),
        "an unreachable Codex runtime must not fail the launch: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The worktree the run created is still there. This is the assertion the
    // incident is about: the old failure path ran failed-session cleanup, which
    // resolves `by-session/<uuid>` and deletes what it points at.
    let worktrees = home.path().join(".agents-in-a-box/worktrees/by-name");
    let created: Vec<_> = std::fs::read_dir(&worktrees)
        .expect("worktrees dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(
        created.len(),
        1,
        "the run must leave exactly the worktree it created, found {created:?}"
    );
    assert!(
        created[0].join("README.md").exists(),
        "the worktree is empty"
    );

    let argv = pane.unwrap_or_else(|failure| panic!("{failure}"));
    for flag in [
        "-c check_for_update_on_startup=false",
        "--dangerously-bypass-hook-trust",
    ] {
        assert!(
            argv.contains(flag),
            "a degraded launch dropped {flag}, which parks the pane on a modal: {argv}"
        );
    }
    assert!(
        !argv.contains("--remote"),
        "a session with no shared thread has no endpoint to join: {argv}"
    );
    assert!(
        !argv.contains("resume"),
        "a session with no shared thread has no thread to resume: {argv}"
    );

    // The other half of "reaches a prompt": Codex asks about a directory it has
    // not seen, and no flag suppresses that one.
    let codex_config =
        std::fs::read_to_string(codex_home.join("config.toml")).expect("read codex config");
    assert!(
        codex_config.contains("trust_level = \"trusted\""),
        "the worktree was not trusted, so the pane opens on the directory-trust modal: \
         {codex_config}"
    );
}

/// Poll capture-pane until the fake Codex reports the argv it was launched with.
fn pane_argv(tmux_dir: &Path, session: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_pane = String::new();
    loop {
        let cap = tmux(tmux_dir, &["capture-pane", "-p", "-t", session]);
        if cap.status.success() {
            last_pane = String::from_utf8_lossy(&cap.stdout).into_owned();
            if let Some(line) = last_pane.lines().find(|l| l.contains("CODEX ARGV:")) {
                return Ok(line.trim().to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the fake Codex never reported its argv; the pane never started it.\npane:\n{last_pane}"
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
