// ABOUTME: Live tmux verification that resuming a stopped session launches the
// correct CLI command. Drives the real InteractiveSessionManager::start_cli_in_tmux
// against a real tmux pane and reads back the launched command, proving:
//   * Claude resume uses `--continue` (NOT the broken `--resume <path>` that the
//     current claude CLI treats as a picker search term)
//   * Codex resume uses the `resume --last` subcommand
//   * a fresh launch does neither
//
// It asserts on tmux's `#{pane_start_command}`, which reflects the exact argv
// tmux was asked to run — independent of whether the CLI binary exists or is
// authenticated, and whether the pane runs argv-directly or via `sh -c 'exec …'`
// (the API-key / OTEL injection path). Gated: skips cleanly when tmux is absent.
//
// ISOLATION — why this file pins `TMUX_TMPDIR`.
// These tests used to run against the machine's DEFAULT tmux server, which is
// also the server every other tmux-driving test (and the developer's own shell)
// uses. A tmux server exits as soon as its LAST session dies, so one test's
// `kill-session` teardown could tear the server down at the exact moment a
// sibling's `new-session` was connecting to it — surfacing as
// `failed to create tmux session ainb-verify-… / server exited unexpectedly`
// (seen on CI run 30209167091, job 89812377132).
//
// Cure, in two layers:
//   1. A PRIVATE server. `TMUX_TMPDIR` moves the default socket to a
//      pid-unique directory, so this process' sessions can never collide with
//      the ambient server, another test binary, or the user's own tmux. The
//      env var (not `tmux -L`) is the right lever because
//      `InteractiveSessionManager` shells out to `tmux` itself and targets the
//      DEFAULT socket — an env var is inherited by those children, a `-L` flag
//      on our own calls would not be, and the manager would then respawn into a
//      pane it cannot see.
//   2. A bounded retry on `new-session`. Under `cargo test` (threads in one
//      process) the four tests still share that one private server, so the
//      shutdown race is narrowed, not eliminated; the retry closes it.
// Under `cargo nextest` (one process per test) layer 1 alone already gives each
// test its own server.

use ainb::interactive::session_manager::InteractiveSessionManager;
use ainb::models::session::SessionAgentType;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// How many times `new_session` will re-attempt a failed `tmux new-session`.
const NEW_SESSION_ATTEMPTS: usize = 5;

struct TmuxIsolation {
    /// Value this process forces into `TMUX_TMPDIR`.
    dir: PathBuf,
    /// Whatever `TMUX_TMPDIR` was before we overrode it, so a probe can still
    /// reach the AMBIENT server on purpose.
    ambient: Option<String>,
}

/// Redirect every tmux call this process makes onto a private socket dir.
///
/// Runs exactly once and BEFORE the first tmux invocation (every helper below
/// goes through [`tmux`]), so the manager's own `tmux` children inherit it too.
/// The dir lives under `/tmp` rather than `TMPDIR`: a unix socket path is capped
/// at 104 bytes on macOS and `$TMPDIR` there is already ~50 of them.
fn isolation() -> &'static TmuxIsolation {
    static ISO: OnceLock<TmuxIsolation> = OnceLock::new();
    ISO.get_or_init(|| {
        let ambient = std::env::var("TMUX_TMPDIR").ok();
        let dir = PathBuf::from("/tmp").join(format!("ainb-verify-tmux-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create private tmux socket dir");
        std::env::set_var("TMUX_TMPDIR", &dir);
        TmuxIsolation { dir, ambient }
    })
}

/// A `tmux` command pinned to this file's private server.
fn tmux() -> Command {
    isolation();
    Command::new("tmux")
}

/// A `tmux` command pointed back at the AMBIENT (default) server — used only to
/// PROVE our sessions are not on it.
fn ambient_tmux() -> Command {
    let iso = isolation();
    let mut cmd = Command::new("tmux");
    match &iso.ambient {
        Some(v) => cmd.env("TMUX_TMPDIR", v),
        None => cmd.env_remove("TMUX_TMPDIR"),
    };
    cmd
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Retry wrapper around a session spawn.
///
/// Extracted from [`new_session`] so the race it exists for can be driven
/// deterministically in a unit test instead of waited on. `backoff` is scaled by
/// the attempt number; pass `Duration::ZERO` to disable sleeping.
fn create_session_with_retry(
    name: &str,
    backoff: Duration,
    mut spawn: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=NEW_SESSION_ATTEMPTS {
        match spawn() {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        if attempt < NEW_SESSION_ATTEMPTS && !backoff.is_zero() {
            std::thread::sleep(backoff * attempt as u32);
        }
    }
    Err(format!(
        "failed to create tmux session {name} after {NEW_SESSION_ATTEMPTS} attempts: {last}"
    ))
}

fn new_session(name: &str) {
    let _ = tmux().args(["kill-session", "-t", name]).output();
    let result = create_session_with_retry(name, Duration::from_millis(120), || {
        let out = tmux()
            .args(["new-session", "-d", "-s", name, "-x", "80", "-y", "24"])
            .output()
            .map_err(|e| format!("spawn tmux: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    });
    result.unwrap_or_else(|e| panic!("{e}"));
}

fn pane_start_command(name: &str) -> String {
    let out = tmux()
        .args(["list-panes", "-t", name, "-F", "#{pane_start_command}"])
        .output()
        .expect("tmux list-panes");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn kill(name: &str) {
    let _ = tmux().args(["kill-session", "-t", name]).output();
}

/// Launch via the REAL manager and return the command tmux was asked to run.
async fn launched_command(
    name: &str,
    agent: SessionAgentType,
    model: Option<&str>,
    resume_transcript: Option<PathBuf>,
    resume_requested: bool,
) -> String {
    let mgr = InteractiveSessionManager::new().expect("manager");
    mgr.start_cli_in_tmux(
        name,
        true, // skip_permissions (yolo)
        model.map(str::to_string),
        agent,
        resume_transcript,
        resume_requested,
        false, // headroom_enabled (keep the env prefix off the critical path)
    )
    .await
    .expect("start_cli_in_tmux");
    std::thread::sleep(std::time::Duration::from_millis(250));
    pane_start_command(name)
}

#[tokio::test]
async fn claude_resume_launches_continue_not_resume_path() {
    if !tmux_available() {
        eprintln!("skip: tmux unavailable");
        return;
    }
    let name = format!("ainb-verify-claude-{}", std::process::id());
    new_session(&name);
    // Some(_) transcript == "prior history exists" == emit --continue.
    let cmd = launched_command(
        &name,
        SessionAgentType::Claude,
        None,
        Some(PathBuf::from("/tmp/whatever.jsonl")),
        true,
    )
    .await;
    kill(&name);

    assert!(cmd.contains("claude"), "expected claude binary, got: {cmd}");
    assert!(
        cmd.contains("--continue"),
        "claude resume must use --continue, got: {cmd}"
    );
    assert!(
        !cmd.contains("--resume"),
        "must NOT use the broken --resume <path>, got: {cmd}"
    );
    assert!(
        cmd.contains("--dangerously-skip-permissions"),
        "yolo flag must survive resume, got: {cmd}"
    );
}

#[tokio::test]
async fn codex_resume_launches_resume_last() {
    if !tmux_available() {
        eprintln!("skip: tmux unavailable");
        return;
    }
    let name = format!("ainb-verify-codex-{}", std::process::id());
    new_session(&name);
    // Codex gets no transcript path; resume_requested drives `resume --last`.
    let cmd = launched_command(
        &name,
        SessionAgentType::Codex,
        Some("gpt-5.6-luna"),
        None,
        true,
    )
    .await;
    kill(&name);

    assert!(
        cmd.contains("codex resume --last"),
        "codex resume must be `resume --last`, got: {cmd}"
    );
    assert!(
        cmd.contains("--dangerously-bypass-approvals-and-sandbox"),
        "codex yolo flag must survive resume, got: {cmd}"
    );
    assert!(
        cmd.contains("--model gpt-5.6-luna"),
        "raw Codex model must survive resume, got: {cmd}"
    );
}

#[tokio::test]
async fn copilot_resume_launches_continue() {
    if !tmux_available() {
        eprintln!("skip: tmux unavailable");
        return;
    }
    let name = format!("ainb-verify-copilot-{}", std::process::id());
    new_session(&name);
    let cmd = launched_command(&name, SessionAgentType::Copilot, None, None, true).await;
    kill(&name);

    assert!(
        cmd.contains("copilot") && cmd.contains("--continue"),
        "copilot resume must use --continue, got: {cmd}"
    );
    assert!(
        cmd.contains("--yolo"),
        "copilot yolo flag must survive, got: {cmd}"
    );
}

/// The isolation contract: a session this file creates must NOT exist on the
/// ambient/default tmux server. Before `TMUX_TMPDIR` was pinned it did, which is
/// how one test's teardown could kill the server another test was starting on.
#[test]
fn sessions_live_on_a_private_tmux_server_not_the_ambient_one() {
    if !tmux_available() {
        eprintln!("skip: tmux unavailable");
        return;
    }
    let name = format!("ainb-verify-isolation-{}", std::process::id());
    new_session(&name);

    let private = tmux()
        .args(["has-session", "-t", &name])
        .output()
        .expect("tmux has-session (private)");
    let ambient = ambient_tmux()
        .args(["has-session", "-t", &name])
        .output()
        .expect("tmux has-session (ambient)");
    kill(&name);

    assert!(
        private.status.success(),
        "session must exist on this file's private server"
    );
    assert!(
        !ambient.status.success(),
        "session leaked onto the AMBIENT tmux server — a sibling's teardown can \
         then kill the server out from under us"
    );
    assert!(isolation().dir.is_dir(), "private socket dir must exist");
}

/// A transient `server exited unexpectedly` must be retried, not fatal.
#[test]
fn new_session_retries_past_a_transient_server_shutdown() {
    let mut calls = 0;
    let out = create_session_with_retry("ainb-verify-unit", Duration::ZERO, || {
        calls += 1;
        if calls < 3 {
            Err("server exited unexpectedly".to_string())
        } else {
            Ok(())
        }
    });
    assert_eq!(out, Ok(()));
    assert_eq!(calls, 3, "must keep retrying until the spawn succeeds");
}

/// …but a genuinely broken tmux still fails loudly, with the real stderr.
#[test]
fn new_session_gives_up_after_the_attempt_budget() {
    let mut calls = 0;
    let out = create_session_with_retry("ainb-verify-unit", Duration::ZERO, || {
        calls += 1;
        Err("no such file or directory".to_string())
    });
    assert_eq!(calls, NEW_SESSION_ATTEMPTS);
    let err = out.expect_err("must not report success");
    assert!(err.contains("ainb-verify-unit"), "got: {err}");
    assert!(err.contains("no such file or directory"), "got: {err}");
}

#[tokio::test]
async fn claude_fresh_launch_has_no_resume() {
    if !tmux_available() {
        eprintln!("skip: tmux unavailable");
        return;
    }
    let name = format!("ainb-verify-fresh-{}", std::process::id());
    new_session(&name);
    let cmd = launched_command(&name, SessionAgentType::Claude, None, None, false).await;
    kill(&name);

    assert!(
        !cmd.contains("--continue") && !cmd.contains("--resume"),
        "fresh launch must not resume, got: {cmd}"
    );
}
