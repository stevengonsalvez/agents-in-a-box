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

use ainb::interactive::session_manager::InteractiveSessionManager;
use ainb::models::session::SessionAgentType;
use std::path::PathBuf;
use std::process::Command;

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn new_session(name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", name]).output();
    let ok = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-x", "80", "-y", "24"])
        .status()
        .expect("spawn tmux")
        .success();
    assert!(ok, "failed to create tmux session {name}");
}

fn pane_start_command(name: &str) -> String {
    let out = Command::new("tmux")
        .args(["list-panes", "-t", name, "-F", "#{pane_start_command}"])
        .output()
        .expect("tmux list-panes");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn kill(name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", name]).output();
}

/// Launch via the REAL manager and return the command tmux was asked to run.
async fn launched_command(
    name: &str,
    agent: SessionAgentType,
    resume_transcript: Option<PathBuf>,
    resume_requested: bool,
) -> String {
    let mgr = InteractiveSessionManager::new().expect("manager");
    mgr.start_cli_in_tmux(
        name,
        true, // skip_permissions (yolo)
        None, // claude_model
        None, // codex_model
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
    let cmd = launched_command(&name, SessionAgentType::Codex, None, true).await;
    kill(&name);

    assert!(
        cmd.contains("codex resume --last"),
        "codex resume must be `resume --last`, got: {cmd}"
    );
    assert!(
        cmd.contains("--dangerously-bypass-approvals-and-sandbox"),
        "codex yolo flag must survive resume, got: {cmd}"
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
    let cmd = launched_command(&name, SessionAgentType::Copilot, None, true).await;
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

#[tokio::test]
async fn claude_fresh_launch_has_no_resume() {
    if !tmux_available() {
        eprintln!("skip: tmux unavailable");
        return;
    }
    let name = format!("ainb-verify-fresh-{}", std::process::id());
    new_session(&name);
    let cmd = launched_command(&name, SessionAgentType::Claude, None, false).await;
    kill(&name);

    assert!(
        !cmd.contains("--continue") && !cmd.contains("--resume"),
        "fresh launch must not resume, got: {cmd}"
    );
}
