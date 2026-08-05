//! Tripwire: `ainb run --prompt` delivers a `-`-prefixed initial prompt to the
//! tmux pane VERBATIM and SUBMITS it. Real tmux, real `ainb` binary.
//!
//! Pins the Phase 0 hardening of the run-command send path: the old local
//! `send_prompt_to_tmux` shelled out as `send-keys -t <s> <prompt> C-m` with no
//! `-l` and no `--` terminator, so a prompt beginning with `-` (e.g. `-y …`)
//! was parsed by tmux as a flag and silently corrupted. The call now routes
//! through fleet-core's hardened `tmux_send` (`-l --` literal + Enter). If
//! anyone reintroduces a bare send-keys path, this test goes red.
//!
//! Harness notes (repo tripwire lore):
//!   * PRIVATE tmux server via `TMUX_TMPDIR` (see `resume_command_tmux.rs`):
//!     env var, not `-L`, because `ainb run` shells out to `tmux` itself and
//!     must land on the same socket; children inherit the env var. `TMUX` is
//!     REMOVED on every spawn: when the test itself runs inside a tmux pane
//!     (dev shells, agent sessions) that variable overrides `TMUX_TMPDIR` and
//!     silently reattaches everything to the ambient shared server.
//!   * A tempdir `$HOME` seeded with a completed onboarding.toml so no
//!     first-run wizard can intercept anything.
//!   * A fake `gemini` agent on PATH (gemini: no MCP-pool side quests, and its
//!     ready marker is in `input_box_ready`'s `READY_MARKERS`). It prints the
//!     ready marker, then echoes each SUBMITTED line as `RECEIVED:<line>`.
//!     `read` only completes on Enter, so the `RECEIVED:` assertion proves
//!     both verbatim delivery AND submission, no substring-OR that could
//!     pass while broken.
//!   * Cleanup kills the session by exact name only, never the server.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The exact prompt under test. MUST start with `-`: that is the regression
/// this tripwire exists to catch (tmux parsing the payload as a flag).
const PROMPT: &str = "-y run the tripwire payload";

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
    let dir =
        PathBuf::from("/tmp").join(format!("ainb-tripwire-run-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create private tmux socket dir");
    dir
}

/// tmux pinned to this test's private server.
fn tmux(tmux_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .env("TMUX_TMPDIR", tmux_dir)
        .env_remove("TMUX") // an inherited $TMUX would override TMUX_TMPDIR
        .args(args)
        .output()
        .expect("run tmux")
}

/// Fake agent CLI: prints a marker `input_box_ready` recognises, then echoes
/// every submitted line. Named `gemini` so `ainb run --tool gemini` finds it.
fn write_fake_gemini(bin_dir: &Path) {
    let path = bin_dir.join("gemini");
    let mut f = std::fs::File::create(&path).expect("create fake gemini");
    f.write_all(
        b"#!/bin/sh\n\
          echo 'fake agent ready. Ctrl+C to exit'\n\
          while IFS= read -r line; do\n\
            printf 'RECEIVED:%s\\n' \"$line\"\n\
          done\n",
    )
    .expect("write fake gemini");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake gemini");
}

/// Seed a completed onboarding.toml so no first-run wizard can steal input.
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
fn run_delivers_dash_prefixed_prompt_verbatim_and_submits() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let tmux_dir = private_tmux_dir();
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");
    seed_onboarding(home.path());
    write_fake_gemini(home.path()); // HOME doubles as the fake-bin dir

    // A real (empty) git repo: `ainb run` records the current branch from it.
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");

    let session = format!("tripwire-run-prompt-{}", std::process::id());
    // `TmuxSession` registers the session under its SANITIZED name (a `tmux_`
    // prefix, see `tmux::sanitize_session_name`); capture/kill must target it.
    let tmux_session = ainb::tmux::sanitize_session_name(&session);
    // Fake bin dir FIRST so our `gemini` wins `which` in the child, then the
    // ambient PATH so `tmux`/`git`/`sh` still resolve.
    let path_env = format!(
        "{}:{}",
        home.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::new(env!("CARGO_BIN_EXE_ainb"))
        .args([
            "run",
            "--tool",
            "gemini",
            "--repo",
            &repo.path().display().to_string(),
            "--name",
            &session,
            // `--prompt=` (glued form): clap rejects a separate `-y …` value as
            // an unexpected flag, exactly like tmux used to.
            &format!("--prompt={PROMPT}"),
        ])
        .env("HOME", home.path())
        .env("PATH", &path_env)
        .env("TMUX_TMPDIR", &tmux_dir)
        .env_remove("TMUX") // an inherited $TMUX would override TMUX_TMPDIR
        .output()
        .expect("ainb run");

    // Best-effort teardown runs even on assertion failure paths below only if
    // we get there, so assert with a helper that kills first.
    let result = verify_pane(&tmux_dir, &tmux_session);
    // Exact session name only, never the server (other tests share machines).
    let _ = tmux(&tmux_dir, &["kill-session", "-t", &tmux_session]);
    let _ = std::fs::remove_dir_all(&tmux_dir);

    assert!(
        out.status.success(),
        "ainb run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if let Err(msg) = result {
        panic!("{msg}");
    }
}

/// Poll capture-pane until the fake agent has echoed the SUBMITTED prompt.
/// `RECEIVED:<PROMPT>` can only appear if the payload survived verbatim
/// (`-y` not eaten as a tmux flag) AND Enter actually submitted it.
fn verify_pane(tmux_dir: &Path, session: &str) -> Result<(), String> {
    let expected = format!("RECEIVED:{PROMPT}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_pane = String::new();
    loop {
        let cap = tmux(tmux_dir, &["capture-pane", "-p", "-t", session]);
        if cap.status.success() {
            last_pane = String::from_utf8_lossy(&cap.stdout).into_owned();
            if last_pane.lines().any(|l| l.trim_end() == expected) {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "fake agent never echoed the submitted prompt.\nexpected line: {expected:?}\npane:\n{last_pane}"
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
