// ABOUTME: The terminal host's effect executor. The reducer in `ainb-app`
// queues `Effect`s; the run loop drains them once per iteration, after the
// step that queued them has finished writing state, and runs each one here.

use std::io::Stdout;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::app::{App, Effect, TerminalTarget, ToolTerminal};

/// Carry out one effect for the terminal host.
///
/// `Err` means the terminal itself could not be suspended or restored, which
/// the run loop treats as fatal; every failure the user can act on becomes a
/// notice instead.
pub async fn execute(
    effect: Effect,
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    match effect {
        Effect::AttachTerminal(target) => attach(app, terminal, target).await,
        Effect::OpenEditor(path) => {
            open_editor(app, &path);
            Ok(())
        }
    }
}

async fn attach(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    target: TerminalTarget,
) -> Result<()> {
    match target {
        TerminalTarget::Session(session_id) => attach_session(app, terminal, session_id).await,
        TerminalTarget::Tmux(session_name) => attach_tmux(app, terminal, session_name).await,
        TerminalTarget::Tool(ToolTerminal::Witr) => attach_witr(app, terminal).await,
        TerminalTarget::Tool(ToolTerminal::Abtop) => attach_abtop(app, terminal).await,
        TerminalTarget::Tool(ToolTerminal::AbtopWithSetup) => {
            attach_abtop_with_setup(app, terminal).await
        }
        TerminalTarget::WorkspaceShell {
            workspace_index,
            target_dir,
        } => attach_workspace_shell(app, terminal, workspace_index, target_dir).await,
    }
}

/// An ainb session's own tmux session.
async fn attach_session(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session_id: Uuid,
) -> Result<()> {
    use crate::app::AttachHandler;

    info!(
        "[ACTION] Handling AttachToTmuxSession for session {}",
        session_id
    );
    debug!(
        "[ACTION] Looking for session in {} workspaces",
        app.state.sessions.workspaces.len()
    );

    // Get session to find tmux session name
    let tmux_session_name = if let Some(session) = app
        .state
        .sessions
        .workspaces
        .iter()
        .flat_map(|w| &w.sessions)
        .find(|s| s.id == session_id)
    {
        debug!(
            "[ACTION] Found session: name='{}', status={:?}, tmux_name={:?}",
            session.name, session.status, session.tmux_session_name
        );
        if let Some(ref name) = session.tmux_session_name {
            info!("[ACTION] Using tmux session name: {}", name);
            Some(name.clone())
        } else {
            error!(
                "[ACTION] No tmux session name found for session {} (name={})",
                session_id, session.name
            );
            app.state
                .add_error_notification(format!("Session '{}' has no tmux session", session.name));
            app.state.shell.ui_needs_refresh = true;
            None
        }
    } else {
        error!("[ACTION] Session {} not found in workspaces", session_id);
        app.state.add_error_notification("Session not found".to_string());
        app.state.shell.ui_needs_refresh = true;
        None
    };

    if let Some(tmux_session_name) = tmux_session_name {
        // Fullscreen attach owns terminal size and input.
        // Preview reconnects after detach.
        app.state.release_interactive_pane();

        // Mark session as attached
        for workspace in &mut app.state.sessions.workspaces {
            for session in &mut workspace.sessions {
                if session.id == session_id {
                    session.mark_attached();
                    break;
                }
            }
        }

        // Create attach handler and attach directly
        info!(
            "[ACTION] Creating attach handler for tmux session '{}'",
            tmux_session_name
        );
        let mut attach_handler = AttachHandler::new_from_terminal(terminal)?;
        info!("[ACTION] Attach handler created, calling attach_to_session...");
        let mut target_missing = false;
        match attach_handler.attach_to_session(&tmux_session_name).await {
            Ok(()) => {
                info!(
                    "[ACTION] Successfully attached and detached from tmux session '{}'",
                    tmux_session_name
                );
            }
            Err(e) => {
                error!(
                    "[ACTION] Failed to attach to tmux session '{}': {}",
                    tmux_session_name, e
                );
                app.state.add_error_notification(attach_failure_notice(&tmux_session_name, &e));
                // An attach can fail because the terminal is nested even
                // though the target is alive. Probe the exact target before
                // changing lifecycle state, so only a terminally missing
                // tmux session becomes resumable Stopped.
                target_missing = matches!(
                    tmux_session_presence(&tmux_session_name).await,
                    TmuxSessionPresence::Missing
                );
            }
        }

        // Mark session as detached
        for workspace in &mut app.state.sessions.workspaces {
            for session in &mut workspace.sessions {
                if session.id == session_id {
                    session.mark_detached();
                    break;
                }
            }
        }

        if target_missing
            && app.state.mark_session_stopped_for_missing_tmux(session_id, &tmux_session_name)
        {
            // Rebuild from the persisted record too. This keeps the row
            // correctly filtered after an immediate refresh or TUI restart.
            app.state.load_real_workspaces().await;
        }

        app.state.shell.ui_needs_refresh = true;
    }
    Ok(())
}

/// A tmux session ainb did not create, by name.
async fn attach_tmux(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session_name: String,
) -> Result<()> {
    use crate::app::AttachHandler;

    // Fullscreen attach owns terminal size and input. Drop
    // preview client first so tmux has only one authority.
    app.state.release_interactive_pane();

    info!(
        "[ACTION] Handling AttachToOtherTmux for session '{}'",
        session_name
    );

    // Create attach handler and attach directly using the session name
    info!(
        "[ACTION] Creating attach handler for other tmux session '{}'",
        session_name
    );
    let mut attach_handler = AttachHandler::new_from_terminal(terminal)?;
    info!("[ACTION] Attach handler created, calling attach_to_session...");
    match attach_handler.attach_to_session(&session_name).await {
        Ok(()) => {
            info!(
                "[ACTION] Successfully attached and detached from other tmux session '{}'",
                session_name
            );
        }
        Err(e) => {
            error!(
                "[ACTION] Failed to attach to other tmux session '{}': {}",
                session_name, e
            );
            app.state.add_error_notification(attach_failure_notice(&session_name, &e));
        }
    }

    // Refresh other tmux sessions list after detach
    app.state.load_other_tmux_sessions().await;
    app.state.shell.ui_needs_refresh = true;
    Ok(())
}

/// `witr -i` in its own tmux session.
async fn attach_witr(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    use crate::app::AttachHandler;
    use tokio::process::Command;

    const WITR_SESSION: &str = "ainb-witr";
    info!(
        "[ACTION] Launching witr -i in tmux session '{}'",
        WITR_SESSION
    );

    // Atomic create-or-reuse: `-A` attaches if the session exists,
    // creates it otherwise; `-d` keeps it detached so we drive the
    // attach (with TUI suspend/resume) ourselves below. tmux runs
    // the command in its OWN pty, so `witr -i` gets a real TTY even
    // though ainb owns the alternate screen. The command is passed as
    // a single string so tmux doesn't parse `-i` as one of its flags.
    let created = Command::new("tmux")
        .args(["new-session", "-A", "-d", "-s", WITR_SESSION, "witr -i"])
        .status()
        .await;
    match created {
        Ok(s) if s.success() => {
            let mut attach_handler = AttachHandler::new_from_terminal(terminal)?;
            if let Err(e) = attach_handler.attach_to_session(WITR_SESSION).await {
                error!("[ACTION] witr attach failed: {}", e);
                app.state
                    .add_error_notification(format!("Failed to open the witr browser: {}", e));
            }
        }
        Ok(s) => {
            error!(
                "[ACTION] failed to create witr tmux session (exit {:?})",
                s.code()
            );
            app.state.add_error_notification(
                "Could not start the witr browser — is `witr` installed and on PATH?".to_string(),
            );
        }
        Err(e) => {
            error!("[ACTION] tmux new-session for witr errored: {}", e);
            app.state
                .add_error_notification(format!("Failed to open the witr browser: {}", e));
        }
    }
    app.state.shell.ui_needs_refresh = true;
    Ok(())
}

/// `abtop --exit-on-jump` in its own tmux session.
async fn attach_abtop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    use crate::app::AttachHandler;
    use tokio::process::Command;

    const ABTOP_SESSION: &str = "ainb-abtop";
    info!(
        "[ACTION] Launching abtop in tmux session '{}'",
        ABTOP_SESSION
    );

    // Atomic create-or-reuse: `-A` attaches if the session exists,
    // creates it otherwise; `-d` keeps it detached so we drive the
    // attach (with TUI suspend/resume) ourselves below. tmux runs
    // the command in its OWN pty, so abtop gets a real TTY even
    // though ainb owns the alternate screen. `--exit-on-jump` makes
    // abtop quit (returning the terminal to ainb) after the user
    // jumps to an agent's pane with Enter. The command is passed as a
    // single string so tmux doesn't parse `--exit-on-jump` as a flag.
    let created = Command::new("tmux")
        .args([
            "new-session",
            "-A",
            "-d",
            "-s",
            ABTOP_SESSION,
            "abtop --exit-on-jump",
        ])
        .status()
        .await;
    match created {
        Ok(s) if s.success() => {
            let mut attach_handler = AttachHandler::new_from_terminal(terminal)?;
            if let Err(e) = attach_handler.attach_to_session(ABTOP_SESSION).await {
                error!("[ACTION] abtop attach failed: {}", e);
                app.state.add_error_notification(format!("Failed to open abtop: {}", e));
            }
        }
        Ok(s) => {
            error!(
                "[ACTION] failed to create abtop tmux session (exit {:?})",
                s.code()
            );
            app.state.add_error_notification(
                "Could not start abtop — is `abtop` installed and on PATH? Install: brew install graykode/tap/abtop · cargo install abtop"
                    .to_string(),
            );
        }
        Err(e) => {
            error!("[ACTION] tmux new-session for abtop errored: {}", e);
            app.state.add_error_notification(format!("Failed to open abtop: {}", e));
        }
    }
    app.state.shell.ui_needs_refresh = true;
    Ok(())
}

/// `abtop --setup` in a detached pane, then abtop itself.
async fn attach_abtop_with_setup(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    use tokio::process::Command;

    info!("[ACTION] Running abtop --setup (rate-limit StatusLine hook)");
    // `abtop --setup` writes a StatusLine hook into
    // ~/.claude/settings.json. Run it in its OWN detached
    // tmux pane so it gets a real TTY (abtop's CLI paths
    // expect one) without disturbing ainb's alternate
    // screen. We don't attach — it completes on its own.
    let setup = Command::new("tmux")
        .args([
            // `-A`: attach-or-create so a stale/slow
            // setup pane doesn't fail a retry with a
            // misleading "is abtop installed?" error.
            "new-session",
            "-A",
            "-d",
            "-s",
            "ainb-abtop-setup",
            "abtop --setup",
        ])
        .status()
        .await;
    match setup {
        Ok(s) if s.success() => {
            app.state.add_info_notification(
                "Enabling abtop rate-limit tracking (abtop --setup)…".to_string(),
            );
        }
        _ => {
            app.state.add_error_notification(
                "Could not run `abtop --setup` — is `abtop` on PATH? You can run it manually."
                    .to_string(),
            );
        }
    }
    app.state.shell.ui_needs_refresh = true;
    // Open abtop regardless of the setup outcome.
    attach_abtop(app, terminal).await
}

/// The workspace's shell, created on first use and `cd`'d to `target_dir`.
async fn attach_workspace_shell(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    workspace_index: usize,
    target_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    use crate::app::AttachHandler;
    use crate::models::ShellSession;
    use shell_escape::escape;
    use std::borrow::Cow;
    use tokio::process::Command;

    info!(
        "[ACTION] Opening workspace shell, index: {}, target_dir: {:?}",
        workspace_index, target_dir
    );

    // Get workspace info
    let (workspace_path, workspace_name, existing_shell) = {
        if let Some(workspace) = app.state.sessions.workspaces.get(workspace_index) {
            (
                workspace.path.clone(),
                workspace.name.clone(),
                workspace.shell_session.as_ref().map(|s| s.tmux_session_name.clone()),
            )
        } else {
            app.state.add_error_notification("Workspace not found".to_string());
            app.state.shell.ui_needs_refresh = true;
            return Ok(());
        }
    };

    // Determine tmux session name - use existing or create new
    let (tmux_name, is_new_shell) = if let Some(existing) = existing_shell {
        (existing, false)
    } else {
        let shell = ShellSession::new_workspace_shell(workspace_path.clone(), &workspace_name);
        let name = shell.tmux_session_name.clone();
        // Store the new shell in workspace
        if let Some(workspace) = app.state.sessions.workspaces.get_mut(workspace_index) {
            workspace.set_shell_session(shell);
        }
        (name, true)
    };

    // Use atomic session creation: -A flag attaches if exists, creates if not
    // This eliminates the TOCTOU race condition
    let workspace_path_str = workspace_path.to_str().unwrap_or(".");
    let create_result = Command::new("tmux")
        .arg("new-session")
        .arg("-A") // Atomic: attach if exists, create if not
        .arg("-d") // Detached (we'll attach separately for TUI handling)
        .arg("-s")
        .arg(&tmux_name)
        .arg("-c")
        .arg(workspace_path_str)
        .output()
        .await;

    match create_result {
        Ok(output) if output.status.success() => {
            // Configure clipboard for the tmux session
            if let Err(e) = crate::tmux::configure_clipboard(&tmux_name).await {
                warn!("[ACTION] Failed to configure clipboard: {}", e);
            }

            if is_new_shell {
                info!("[ACTION] Created new workspace shell: {}", tmux_name);
                app.state.add_success_notification(format!(
                    "$ Created workspace shell: {}",
                    workspace_name
                ));
            } else {
                info!("[ACTION] Reusing workspace shell: {}", tmux_name);
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("[ACTION] Failed to create/attach tmux session: {}", stderr);
            app.state.add_error_notification(format!("Failed to create shell: {}", stderr));
            app.state.shell.ui_needs_refresh = true;
            return Ok(());
        }
        Err(e) => {
            error!("[ACTION] Failed to create tmux session: {}", e);
            app.state.add_error_notification(format!("Failed to create shell: {}", e));
            app.state.shell.ui_needs_refresh = true;
            return Ok(());
        }
    }

    // If target_dir specified, cd to it before attaching
    if let Some(ref dir) = target_dir {
        let dir_str = dir.to_str().unwrap_or(".");
        info!("[ACTION] Sending cd command to shell: {}", dir_str);

        // Use proper shell escaping to prevent command injection
        // This handles paths with spaces, quotes, and special characters
        let escaped_path = escape(Cow::Borrowed(dir_str));
        let cd_cmd = format!("cd {} && clear", escaped_path);

        let cd_result = Command::new("tmux")
            .args(["send-keys", "-t", &tmux_name, &cd_cmd, "Enter"])
            .output()
            .await;

        match cd_result {
            Ok(output) if output.status.success() => {
                // Update stored working_dir for state consistency
                if let Some(workspace) = app.state.sessions.workspaces.get_mut(workspace_index) {
                    if let Some(shell) = workspace.get_shell_session_mut() {
                        shell.set_working_dir(dir.clone());
                    }
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("[ACTION] tmux send-keys may have failed: {}", stderr);
                app.state
                    .add_warning_notification(format!("May have failed to cd to: {}", dir_str));
            }
            Err(e) => {
                error!("[ACTION] tmux send-keys error: {}", e);
                app.state.add_error_notification(format!("Shell command error: {}", e));
            }
        }
    }

    // Update shell's last accessed time
    if let Some(workspace) = app.state.sessions.workspaces.get_mut(workspace_index) {
        if let Some(shell) = workspace.get_shell_session_mut() {
            shell.touch();
        }
    }

    // Attach to the shell
    let mut attach_handler = AttachHandler::new_from_terminal(terminal)?;
    match attach_handler.attach_to_session(&tmux_name).await {
        Ok(()) => {
            info!("[ACTION] Successfully attached to workspace shell");
        }
        Err(e) => {
            error!("[ACTION] Failed to attach to shell: {}", e);
            app.state.add_error_notification(format!("Failed to attach: {}", e));
        }
    }

    app.state.shell.ui_needs_refresh = true;
    Ok(())
}

fn open_editor(app: &mut App, path: &std::path::Path) {
    info!("[EFFECT] Opening in editor: {:?}", path);
    let Some(editor) = resolve_editor(&app.state.config.app_config) else {
        warn!("No editor found in fallback chain");
        app.state.add_error_notification(
            "❌ No editor found. Set preferred editor in settings or install VS Code.".to_string(),
        );
        return;
    };
    info!("Opening {} in {}", path.display(), editor);
    match std::process::Command::new(&editor).arg(path).spawn() {
        Ok(_) => app.state.add_success_notification(format!("📝 Opened in {}", editor)),
        Err(e) => {
            error!("Failed to open editor: {}", e);
            app.state.add_error_notification(format!("❌ Failed to open editor: {}", e));
        }
    }
}

/// The editor to run: the configured preference, then `code`, then `$EDITOR`,
/// whichever is on `PATH` first.
fn resolve_editor(config: &crate::config::AppConfig) -> Option<String> {
    if let Some(ref editor) = config.ui_preferences.preferred_editor {
        if command_exists(editor) {
            return Some(editor.clone());
        }
    }
    if command_exists("code") {
        return Some("code".to_string());
    }
    if let Ok(editor) = std::env::var("EDITOR") {
        if command_exists(&editor) {
            return Some(editor);
        }
    }
    None
}

fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A tmux attach failure, naming the target and the two causes that produce it.
///
/// `Failed to attach: tmux attach-session failed with exit code: Some(1)` named
/// neither the session nor anything the operator could act on. It is not
/// laziness on the error's part: tmux prints its real reason to the terminal
/// the TUI is about to repaint over, so an exit code is genuinely all that
/// survives the round trip. What the caller knows and never said is the target
/// and the two things that actually produce a bare exit 1 here.
///
/// Room for this is what `[ui] notice_error_secs` and `Ctrl+X` bought: at five
/// seconds and one clipped line, a sentence like this would have been worse
/// than the stub.
fn attach_failure_notice(session_name: &str, error: &impl std::fmt::Display) -> String {
    format!(
        "Failed to attach to '{session_name}': {error}. Either the session ended after \
         the list was drawn — press f to refresh — or it is the tmux session ainb is \
         itself running in, which tmux refuses to nest."
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TmuxSessionPresence {
    Exists,
    Missing,
    Uncertain,
}

/// Probe an exact tmux target without converting a transport failure into a
/// lifecycle fact. A bare `-t name` can prefix-match a different live session;
/// `=name` cannot.
async fn tmux_session_presence(session_name: &str) -> TmuxSessionPresence {
    let output = match tokio::process::Command::new("tmux")
        .args(["has-session", "-t", &format!("={session_name}")])
        .output()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(%error, %session_name, "could not verify tmux target after attach failure");
            return TmuxSessionPresence::Uncertain;
        }
    };
    if output.status.success() {
        return TmuxSessionPresence::Exists;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_explicitly_missing_tmux_target(&stderr) {
        TmuxSessionPresence::Missing
    } else {
        tracing::warn!(%session_name, %stderr, "tmux target probe was inconclusive after attach failure");
        TmuxSessionPresence::Uncertain
    }
}

fn is_explicitly_missing_tmux_target(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("can't find session")
        || stderr.contains("no server running")
        || (stderr.contains("error connecting to") && stderr.contains("no such file or directory"))
}

#[cfg(test)]
mod attach_failure_notice_tests {
    use super::{attach_failure_notice, is_explicitly_missing_tmux_target};

    /// The three things the old one-liner never said.
    #[test]
    fn the_notice_names_the_target_the_error_and_what_to_do() {
        let notice = attach_failure_notice(
            "tmux_myrepo_main",
            &"tmux attach-session failed with exit code: Some(1)",
        );
        assert!(notice.contains("tmux_myrepo_main"), "the target: {notice}");
        assert!(notice.contains("exit code: Some(1)"), "the error: {notice}");
        assert!(
            notice.contains("press f to refresh"),
            "the remedy: {notice}"
        );
        assert!(notice.contains("nest"), "the other cause: {notice}");
    }

    #[test]
    fn only_definitive_tmux_diagnostics_mean_target_missing() {
        assert!(is_explicitly_missing_tmux_target(
            "can't find session: tmux_dead"
        ));
        assert!(is_explicitly_missing_tmux_target(
            "no server running on /tmp/tmux-1/default"
        ));
        assert!(is_explicitly_missing_tmux_target(
            "error connecting to /tmp/tmux-1/default (No such file or directory)"
        ));
        assert!(!is_explicitly_missing_tmux_target("permission denied"));
        assert!(!is_explicitly_missing_tmux_target(
            "protocol version mismatch"
        ));
    }
}
