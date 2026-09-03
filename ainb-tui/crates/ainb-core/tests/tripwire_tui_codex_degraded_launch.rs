//! Tripwire: the TUI's Codex launch WITHOUT a shared remote thread reaches a
//! prompt. Real tmux, real `InteractiveSessionManager::create_session`, fake
//! `codex` on PATH.
//!
//! The CLI and the TUI build their Codex argv through different call sites, and
//! only the CLI's was covered. `start_cli_in_tmux` wrote the project-trust entry
//! and built Codex argv inside its `if let Some(remote)` arm only; everything
//! without a shared thread fell to the provider-generic builder and lost both
//! modal suppressors. That arm was rare while it took an ephemeral hangar home
//! to reach; degrading on an unreachable daemon makes it the ordinary path.
//!
//! A modal STALLS the pane instead of failing, so nothing above this notices:
//! `create_session` returns Ok, the session record is written, and the pane sits
//! on a question. The pane text is the only place the truth is written down,
//! which is why this drives the real thing rather than the builder.
//!
//! One test per binary ON PURPOSE: it sets `$HOME`, `$PATH` and `$TMUX_TMPDIR`
//! for the process, and cargo runs a binary's tests on parallel threads.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb::interactive::InteractiveSessionManager;
use ainb::models::session::SessionAgentType;

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

fn private_tmux_dir() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let dir = PathBuf::from("/tmp").join(format!(
        "ainb-tripwire-tui-codex-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create private tmux socket dir");
    dir
}

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux").args(args).output().expect("run tmux")
}

/// Fake Codex: reports its argv, then holds the pane like the real CLI.
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

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(repo).output().expect("run git");
    assert!(out.status.success(), "git {args:?} failed");
}

#[tokio::test]
async fn the_tui_launches_codex_without_a_shared_thread_and_reaches_a_prompt() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let tmux_dir = private_tmux_dir();
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");
    write_fake_codex(home.path()); // HOME doubles as the fake-bin dir

    let codex_home = home.path().join("codex-home");
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    // Present but empty: the trust write is a deliberate no-op when Codex has
    // never written a config, so an absent file would pass for the wrong reason.
    std::fs::write(codex_home.join("config.toml"), "").expect("seed codex config");

    // `$HOME` under the temp dir with no `$AINB_HANGAR_HOME` is the one shape
    // the autostart refuses to spawn a daemon for, so this degrades without
    // leaving a daemon running on a CI machine.
    std::env::remove_var("AINB_HANGAR_HOME");
    std::env::remove_var("TMUX");
    std::env::set_var("HOME", home.path());
    std::env::set_var("AINB_HOME", home.path());
    std::env::set_var("CODEX_HOME", &codex_home);
    std::env::set_var("TMUX_TMPDIR", &tmux_dir);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            home.path().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    git(repo.path(), &["init", "-q"]);
    git(
        repo.path(),
        &["config", "user.email", "tripwire@example.com"],
    );
    git(repo.path(), &["config", "user.name", "tripwire"]);
    std::fs::write(repo.path().join("README.md"), "tripwire\n").expect("seed repo file");
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-qm", "init", "--no-gpg-sign"]);

    let session_id = uuid::Uuid::new_v4();
    let branch = format!("tripwire-tui-codex-{}", &session_id.to_string()[..8]);
    let mut manager = InteractiveSessionManager::new().expect("interactive session manager");
    let session = manager
        .create_session(
            session_id,
            "tripwire".to_string(),
            repo.path().to_path_buf(),
            branch.clone(),
            None,
            false,
            SessionAgentType::Codex,
            None,
            false,
            false,
        )
        .await
        .expect("an unreachable Codex runtime must not fail the launch");

    let argv = pane_argv(&session.tmux_session_name);
    let _ = tmux(&[
        "kill-session",
        "-t",
        &format!("={}", session.tmux_session_name),
    ]);
    let _ = std::fs::remove_dir_all(&tmux_dir);

    let argv = argv.unwrap_or_else(|failure| panic!("{failure}"));
    for flag in [
        "-c check_for_update_on_startup=false",
        "--dangerously-bypass-hook-trust",
    ] {
        assert!(
            argv.contains(flag),
            "the TUI's degraded launch dropped {flag}, which parks the pane on a modal: {argv}"
        );
    }
    assert!(
        !argv.contains("--remote"),
        "a session with no shared thread has no endpoint to join: {argv}"
    );

    let codex_config =
        std::fs::read_to_string(codex_home.join("config.toml")).expect("read codex config");
    assert!(
        codex_config.contains("trust_level = \"trusted\""),
        "the worktree was not trusted, so the pane opens on the directory-trust modal: \
         {codex_config}"
    );
}

/// Poll capture-pane until the fake Codex reports the argv it was launched with.
fn pane_argv(session: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last_pane = String::new();
    loop {
        let cap = tmux(&["capture-pane", "-p", "-t", session]);
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
