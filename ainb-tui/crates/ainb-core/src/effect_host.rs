// ABOUTME: The terminal host's effect executor. The reducer in `ainb-app`
// queues `Effect`s; the run loop runs each one here once the step that queued
// it has finished writing state. The executor neither reads nor writes state:
// the effect carries what it needs, and what the work changed comes back as
// report intents the run loop dispatches.

use std::io::Stdout;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::app::reports::{self, AttachOutcome, AttachedTo, EditorOutcome, ShellCd, ShellOutcome};
use crate::app::ui_state::UiState;
use crate::app::{Effect, Intent, TerminalTarget, ToolTerminal};

/// Carry out one effect for the terminal host and return the reports to
/// dispatch, in order.
///
/// The executor holds no state: everything an effect needs rides on it, plus
/// the renderer's own `ui` layout, the live tmux client it keeps for the
/// preview pane, and the plugin runtime handle the host owns (`App`). The
/// reducer never reaches the runtime: plugin input and actions arrive here as
/// `Effect::ForwardToPlugin` and `Effect::RunPluginAction`.
///
/// `Err` means the terminal itself could not be suspended or restored, which
/// the run loop treats as fatal; every failure the user can act on is a report
/// the reducer turns into a notice.
pub fn execute<'t>(
    effect: Effect,
    terminal: &'t mut Terminal<CrosstermBackend<Stdout>>,
    ui: &UiState,
    clients: &mut crate::terminal_clients::TerminalClients,
    plugins: Option<&ainb_plugin_runtime::RuntimeHandle>,
) -> impl std::future::Future<Output = Result<Vec<Intent>>> + 't {
    let work = match effect {
        Effect::AttachTerminal(TerminalTarget::InPlace {
            tmux_session,
            show_menu_bar,
        }) => {
            let (rows, cols) = preview_size(terminal, ui, show_menu_bar);
            Work::Done(vec![clients.open_in_place(&tmux_session, rows, cols)])
        }
        Effect::AttachTerminal(TerminalTarget::Observe {
            tmux_session,
            show_menu_bar,
        }) => {
            let (rows, cols) = preview_size(terminal, ui, show_menu_bar);
            Work::Done(vec![clients.open_observer(&tmux_session, rows, cols)])
        }
        Effect::AttachTerminal(target) => Work::Attach(target),
        Effect::Detach => Work::Done(vec![reports::detached()]),
        Effect::OpenEditor {
            path,
            preferred_editor,
        } => Work::Done(vec![open_editor(
            path.as_path(),
            preferred_editor.as_deref(),
        )]),
        Effect::PasteClipboard => Work::Done(vec![paste_clipboard()]),
        Effect::RunDaemonAction {
            daemon,
            action,
            generation,
        } => {
            spawn_daemon_action(daemon, action, generation);
            Work::Done(Vec::new())
        }
        Effect::InboxMarkAllRead => {
            spawn_inbox_mark_all_read();
            Work::Done(Vec::new())
        }
        Effect::Persist(store) => Work::Done(match crate::config::persist::write(&store) {
            Ok(()) => Vec::new(),
            Err(error) => vec![reports::persist_failed(store.store_id(), &error)],
        }),
        Effect::ForwardToPlugin {
            plugin,
            screen,
            input,
            back,
        } => {
            let pid = ainb_plugin_runtime::PluginId::from(plugin.as_str());
            // A render past its budget holds the mutex the plugin's input
            // dispatch also needs, so a delivered key sits unserviced there,
            // which to the user is the same as a dead plugin.
            let serviced = plugins.is_some_and(|runtime| {
                let delivered = match input {
                    crate::app::PluginInput::Key(key) => {
                        runtime.send_key(&pid, screen.clone(), key)
                    }
                    crate::app::PluginInput::Mouse(mouse) => {
                        runtime.send_mouse(&pid, screen.clone(), mouse)
                    }
                };
                delivered && !runtime.render_wedged(&pid)
            });
            Work::Done(if back && !serviced {
                vec![reports::plugin_input_undelivered(&plugin, &screen)]
            } else {
                if !serviced {
                    tracing::debug!(%plugin, %screen, "plugin input dropped: not serviced");
                }
                Vec::new()
            })
        }
        Effect::RunPluginAction {
            plugin,
            action_id,
            payload,
        } => {
            let sent = plugins.is_some_and(|runtime| {
                runtime.send_action(
                    &ainb_plugin_runtime::types::PluginId::new(plugin.as_str()),
                    action_id.as_str(),
                    payload,
                )
            });
            Work::Done(if sent {
                Vec::new()
            } else {
                vec![reports::plugin_action_undelivered(&plugin, &action_id)]
            })
        }
    };
    async move {
        match work {
            Work::Done(reports) => Ok(reports),
            Work::Attach(target) => attach(terminal, target).await,
        }
    }
}

/// An effect's work, split into what finished before the future starts and
/// what attaches a terminal.
enum Work {
    Done(Vec<Intent>),
    Attach(TerminalTarget),
}

/// Reports from work that outlives the effect that started it, waiting for the
/// run loop to dispatch them.
///
/// ponytail: one process-wide queue, because this process runs one terminal
/// host; a host that ran two would give each its own.
fn deferred() -> &'static (
    std::sync::mpsc::Sender<Intent>,
    std::sync::Mutex<std::sync::mpsc::Receiver<Intent>>,
) {
    static DEFERRED: std::sync::OnceLock<(
        std::sync::mpsc::Sender<Intent>,
        std::sync::Mutex<std::sync::mpsc::Receiver<Intent>>,
    )> = std::sync::OnceLock::new();
    DEFERRED.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, std::sync::Mutex::new(rx))
    })
}

/// Reports that background work finished since the last call, oldest first.
/// The run loop dispatches them like any other intent.
#[must_use]
pub fn take_deferred_reports() -> Vec<Intent> {
    drain(&deferred().1)
}

/// Everything waiting on `queue`, oldest first.
fn drain(queue: &std::sync::Mutex<std::sync::mpsc::Receiver<Intent>>) -> Vec<Intent> {
    // A worker that panicked while holding the lock leaves the queue intact;
    // dropping every later report would pin rows on `working` forever.
    let rx = queue.lock().unwrap_or_else(|poisoned| {
        warn!("deferred report queue was poisoned; recovering it");
        poisoned.into_inner()
    });
    rx.try_iter().collect()
}

/// Run a daemon lifecycle verb on a worker and report it when it exits. A
/// worker that cannot start is reported as the failure, so the row never
/// stays on `working` waiting for a report that is not coming.
fn spawn_daemon_action(
    daemon: crate::fleet::daemons::probe::DaemonKind,
    action: crate::cli::daemon::Action,
    generation: u64,
) {
    let tx = deferred().0.clone();
    let spawned = std::thread::Builder::new().name("ainb-daemon-action".into()).spawn({
        let tx = tx.clone();
        move || {
            let report = run_daemon_action(daemon.id(), action.id()).sealed(generation);
            let _ = tx.send(reports::daemon_action_finished(&report));
        }
    });
    if let Err(error) = spawned {
        let report = reports::DaemonActionReport {
            daemon: daemon.id().to_string(),
            verb: action.id().to_string(),
            generation,
            ok: false,
            summary: format!("{} failed", action.id()),
            detail: format!("the worker that runs `ainb daemon` did not start: {error}"),
            local: None,
        };
        let _ = tx.send(reports::daemon_action_finished(&report));
    }
}

/// Sweep the inbox read on a worker thread, as the terminal, and report how
/// it ended. The op id is minted once inside and held across the retry
/// (`ainb_app::fleet::inbox_write`).
fn spawn_inbox_mark_all_read() {
    use ainb_hangar_proto::connections::SurfaceKind;
    let tx = deferred().0.clone();
    let spawned = std::thread::Builder::new().name("ainb-inbox-mark-read".into()).spawn({
        let tx = tx.clone();
        move || {
            let outcome = ainb_app::fleet::inbox_write::mark_all_read_blocking(|| {
                ainb_app::fleet::bridge::daemon::surface_client(SurfaceKind::Tui)
            });
            let _ = tx.send(reports::inbox_mark_all_read_finished(&outcome));
        }
    });
    if let Err(error) = spawned {
        let outcome = ainb_app::fleet::inbox_write::MarkAllReadOutcome {
            op_id: String::new(),
            ok: false,
            marked: 0,
            unread: 0,
            error: Some(format!("the worker did not start: {error}")),
            after: None,
        };
        let _ = tx.send(reports::inbox_mark_all_read_finished(&outcome));
    }
}

/// The system clipboard's text as a bracketed paste would deliver it.
fn paste_clipboard() -> Intent {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
        Ok(text) => Intent::Text(text),
        Err(e) => {
            warn!("Clipboard paste failed: {}", e);
            reports::clipboard_failed(&e.to_string())
        }
    }
}

/// Attach a terminal target full screen.
async fn attach(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    target: TerminalTarget,
) -> Result<Vec<Intent>> {
    Ok(match target {
        TerminalTarget::InPlace { .. } | TerminalTarget::Observe { .. } => {
            unreachable!("the preview client opens before the future starts")
        }
        TerminalTarget::Session { id, tmux_session } => {
            vec![attach_session(terminal, id, tmux_session.as_str()).await?]
        }
        TerminalTarget::Tmux(session_name) => {
            let outcome = attach_named(terminal, session_name.as_str()).await?;
            vec![reports::attach_finished(
                &AttachedTo::Tmux(session_name.as_str().to_string()),
                &outcome,
            )]
        }
        TerminalTarget::Tool(ToolTerminal::Witr) => vec![attach_witr(terminal).await?],
        TerminalTarget::Tool(ToolTerminal::Abtop) => vec![attach_abtop(terminal).await?],
        TerminalTarget::Tool(ToolTerminal::AbtopWithSetup) => {
            vec![run_abtop_setup().await, attach_abtop(terminal).await?]
        }
        TerminalTarget::WorkspaceShell {
            workspace_path,
            tmux_session,
            new_shell,
            target_dir,
        } => {
            attach_workspace_shell(
                terminal,
                workspace_path,
                tmux_session.as_str(),
                new_shell,
                target_dir,
            )
            .await?
        }
        TerminalTarget::ClaudeLogin { auth_dir, image } => {
            vec![claude_login(terminal, &auth_dir, &image)?]
        }
    })
}

/// Attach to `session_name` full screen until the user detaches.
async fn attach_named(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session_name: &str,
) -> Result<AttachOutcome> {
    let mut attach_handler = crate::app::AttachHandler::new_from_terminal(terminal)?;
    Ok(match attach_handler.attach_to_session(session_name).await {
        Ok(()) => {
            info!(
                "[ACTION] Attached and detached from tmux session '{}'",
                session_name
            );
            AttachOutcome::Detached
        }
        Err(e) => {
            error!(
                "[ACTION] Failed to attach to tmux session '{}': {}",
                session_name, e
            );
            AttachOutcome::Failed(e.to_string())
        }
    })
}

/// Leave every input mode the TUI set up at startup (raw mode, the alternate
/// screen, mouse capture, bracketed paste), so a child sees a plain tty.
fn release_terminal() -> std::io::Result<()> {
    use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
    use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
    disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )
}

/// Restore the input modes [`release_terminal`] left. Without mouse capture
/// and bracketed paste, mouse events stop arriving after the child returns.
fn reclaim_terminal() -> std::io::Result<()> {
    use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
    use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
    enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
}

/// The Claude OAuth login in `image`, on the plain tty.
fn claude_login(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    auth_dir: &std::path::Path,
    image: &str,
) -> Result<Intent> {
    info!("Exiting TUI to run interactive authentication");
    release_terminal()?;

    println!("\n🔐 Claude Authentication Setup\n");
    println!("This will guide you through the OAuth authentication process.");
    println!("You'll be prompted to open a URL in your browser to complete authentication.\n");

    // Inherit stdin/stdout/stderr so the container gets the real TTY.
    let exited_ok = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "-it",
            "-v",
            &format!("{}:/home/claude-user/.claude", auth_dir.display()),
            "-e",
            "PATH=/home/claude-user/.npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "-e",
            "HOME=/home/claude-user",
            "-e",
            "AUTH_METHOD=oauth",
            "-w",
            "/home/claude-user",
            "--user",
            "claude-user",
            "--entrypoint",
            "bash",
            image,
            "-c",
            "/app/scripts/auth-setup.sh",
        ])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| error!("Failed to start the authentication container: {}", e))
        .is_ok_and(|status| status.success());

    if reports::oauth_credentials_written(auth_dir, exited_ok) {
        println!("\n✅ Authentication successful!");
        println!("Press Enter to continue...");
    } else {
        println!("\n❌ Authentication failed!");
        println!("Press Enter to return to the authentication menu...");
    }
    let _ = std::io::stdin().read_line(&mut String::new());

    // The login ran on the primary screen. Clear it and its scrollback
    // (ESC[3J) before taking the terminal back, so nothing the child printed,
    // the OAuth URL included, stays readable after the TUI returns.
    {
        use std::io::Write;
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[H\x1b[2J\x1b[3J");
        let _ = stdout.flush();
    }
    reclaim_terminal()?;
    terminal.clear()?;
    Ok(reports::login_finished(auth_dir, exited_ok))
}

/// The preview pane's interior under the session list's current layout (the
/// user's sidebar and chrome), so a client opens at the size it is drawn and
/// tmux reflows once. The render path still resizes it for terminal resizes.
fn preview_size(
    terminal: &Terminal<CrosstermBackend<Stdout>>,
    ui: &UiState,
    show_menu_bar: bool,
) -> (u16, u16) {
    let size = terminal.size().unwrap_or(ratatui::layout::Size {
        width: 80,
        height: 24,
    });
    let sidebar = ui.sessions_pane.effective_width(size.width);
    crate::components::layout::interactive_embed_size(
        size.width,
        size.height,
        sidebar,
        show_menu_bar,
    )
}

/// An ainb session's own tmux session, `tmux_session_name` as state named it
/// when the attach was queued.
async fn attach_session(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session_id: Uuid,
    tmux_session_name: &str,
) -> Result<Intent> {
    let target = AttachedTo::Session(session_id);
    info!(
        "[ACTION] Attaching session {} to tmux session '{}'",
        session_id, tmux_session_name
    );
    let outcome = match attach_named(terminal, tmux_session_name).await? {
        // An attach can fail because the terminal is nested even though the
        // target is alive. Probe the exact target, so only a terminally
        // missing tmux session is reported as gone.
        AttachOutcome::Failed(error)
            if tmux_session_presence(tmux_session_name).await == TmuxSessionPresence::Missing =>
        {
            AttachOutcome::TargetMissing(error)
        }
        outcome => outcome,
    };
    Ok(reports::attach_finished(&target, &outcome))
}

/// Create-or-reuse `session` running `command` in its own tmux session, then
/// attach to it. `-A` attaches if the session exists and creates it
/// otherwise; `-d` keeps it detached so the attach below suspends and
/// resumes the TUI. tmux runs the command in its own pty, so the tool gets a
/// real TTY even though ainb owns the alternate screen. The command is one
/// string so tmux does not parse its flags as tmux's own.
async fn attach_tool(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session: &str,
    command: &str,
) -> Result<AttachOutcome> {
    info!(
        "[ACTION] Launching `{}` in tmux session '{}'",
        command, session
    );
    let created = tokio::process::Command::new("tmux")
        .args(["new-session", "-A", "-d", "-s", session, command])
        .status()
        .await;
    match created {
        Ok(s) if s.success() => attach_named(terminal, session).await,
        Ok(s) => {
            error!(
                "[ACTION] failed to create tmux session '{}' (exit {:?})",
                session,
                s.code()
            );
            Ok(AttachOutcome::NotInstalled)
        }
        Err(e) => {
            error!("[ACTION] tmux new-session for '{}' errored: {}", session, e);
            Ok(AttachOutcome::Failed(e.to_string()))
        }
    }
}

/// `witr -i`, the process-causality browser.
async fn attach_witr(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<Intent> {
    let outcome = attach_tool(terminal, "ainb-witr", "witr -i").await?;
    Ok(reports::attach_finished(&AttachedTo::Witr, &outcome))
}

/// `abtop --exit-on-jump`: abtop quits, returning the terminal to ainb, after
/// the user jumps to an agent's pane with Enter.
async fn attach_abtop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<Intent> {
    let outcome = attach_tool(terminal, "ainb-abtop", "abtop --exit-on-jump").await?;
    Ok(reports::attach_finished(&AttachedTo::Abtop, &outcome))
}

/// `abtop --setup`, which writes a `StatusLine` hook into
/// `~/.claude/settings.json`, in its own detached tmux pane so it gets the
/// real TTY abtop's CLI paths expect. Not attached: it completes on its own.
async fn run_abtop_setup() -> Intent {
    info!("[ACTION] Running abtop --setup (rate-limit StatusLine hook)");
    // `-A`: attach-or-create, so a stale or slow setup pane does not fail a
    // retry with a misleading "is abtop installed?" error.
    let setup = tokio::process::Command::new("tmux")
        .args([
            "new-session",
            "-A",
            "-d",
            "-s",
            "ainb-abtop-setup",
            "abtop --setup",
        ])
        .status()
        .await;
    reports::abtop_setup_finished(setup.is_ok_and(|status| status.success()))
}

/// The workspace's shell, created on first use and `cd`'d to `target_dir`.
async fn attach_workspace_shell(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    workspace_path: std::path::PathBuf,
    tmux_name: &str,
    new_shell: bool,
    target_dir: Option<std::path::PathBuf>,
) -> Result<Vec<Intent>> {
    use shell_escape::escape;
    use std::borrow::Cow;
    use tokio::process::Command;

    info!(
        "[ACTION] Opening workspace shell '{}', target_dir: {:?}",
        tmux_name, target_dir
    );

    // Atomic create-or-reuse: -A attaches if the session exists, creates it
    // otherwise, so there is no check-then-create race.
    let create_result = Command::new("tmux")
        .arg("new-session")
        .arg("-A")
        .arg("-d") // Detached; the attach below handles the TUI.
        .arg("-s")
        .arg(tmux_name)
        .arg("-c")
        .arg(workspace_path.to_str().unwrap_or("."))
        .output()
        .await;

    let failed = match create_result {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(String::from_utf8_lossy(&output.stderr).into_owned()),
        Err(e) => Some(e.to_string()),
    };
    if let Some(error) = failed {
        error!("[ACTION] Failed to create/attach tmux session: {}", error);
        return Ok(vec![reports::shell_prepared(
            &workspace_path,
            &ShellOutcome::Failed(error),
        )]);
    }
    if let Err(e) = crate::tmux::configure_clipboard(tmux_name).await {
        warn!("[ACTION] Failed to configure clipboard: {}", e);
    }

    let cd = match target_dir {
        None => ShellCd::Stayed,
        Some(dir) => {
            let dir_str = dir.to_str().unwrap_or(".");
            info!("[ACTION] Sending cd command to shell: {}", dir_str);
            // Escaped, so a path with spaces, quotes or shell syntax is one
            // argument to cd and never a second command.
            let cd_cmd = format!("cd {} && clear", escape(Cow::Borrowed(dir_str)));
            // `=` makes tmux match the session name exactly, never a prefix
            // of another session's name.
            let exact_target = format!("={tmux_name}:");
            match Command::new("tmux")
                .args(["send-keys", "-t", &exact_target, &cd_cmd, "Enter"])
                .output()
                .await
            {
                Ok(output) if output.status.success() => ShellCd::Moved(dir),
                Ok(output) => {
                    warn!(
                        "[ACTION] tmux send-keys may have failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    ShellCd::MaybeFailed(dir)
                }
                Err(e) => {
                    error!("[ACTION] tmux send-keys error: {}", e);
                    ShellCd::Failed(e.to_string())
                }
            }
        }
    };
    let prepared = reports::shell_prepared(
        &workspace_path,
        &ShellOutcome::Ready {
            created: new_shell,
            cd,
        },
    );

    let outcome = attach_named(terminal, tmux_name).await?;
    Ok(vec![
        prepared,
        reports::attach_finished(&AttachedTo::WorkspaceShell(workspace_path), &outcome),
    ])
}

/// Shell one `ainb daemon <kind> <action>` and capture everything it said.
///
/// Runs on a worker thread. The captured argv, exit status, and output are
/// what the row's error view shows verbatim: the operator sees the actual
/// failure, not our summary of it.
fn run_daemon_action(kind_id: &str, verb: &str) -> reports::DaemonActionReport {
    let argv = format!("ainb daemon {kind_id} {verb}");
    // Never self-exec a test harness: under `cargo test` current_exe() is the
    // test binary, and libtest treats the trailing argv as name filters, so
    // this would re-run the suite instead of running a subcommand. See
    // `crate::self_exec_guard` and issue #715.
    if crate::self_exec_guard::running_under_cargo_test() {
        return reports::DaemonActionReport {
            daemon: kind_id.to_string(),
            verb: verb.to_string(),
            generation: 0,
            ok: false,
            summary: format!("{verb} unavailable"),
            detail: format!(
                "cmd: {argv}\nrefusing to self-exec a cargo test binary \
                 (current_exe is a test harness, not `ainb`)"
            ),
            local: None,
        };
    }
    let bin = match std::env::current_exe() {
        Ok(bin) => bin,
        Err(e) => {
            return reports::DaemonActionReport {
                daemon: kind_id.to_string(),
                verb: verb.to_string(),
                generation: 0,
                ok: false,
                summary: format!("{verb} failed"),
                detail: format!("cmd: {argv}\ncould not resolve the running ainb binary: {e}"),
                local: None,
            };
        }
    };
    match std::process::Command::new(bin).args(["daemon", kind_id, verb]).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let ok = out.status.success();
            // The LAST non-empty line, not the first: it was named `first_line`
            // for long enough that a CLI ending its output with a help block
            // badged the row with the help's closing line. Every verb reachable
            // from this menu therefore has to end its stdout with the sentence
            // worth badging; `fleet atc mode --set` prints its notes first for
            // exactly this reason.
            let badge_line = |s: &str| {
                s.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let summary = if ok {
                let line = badge_line(&stdout);
                if line.is_empty() {
                    format!("{verb} ok")
                } else {
                    line
                }
            } else {
                format!("{verb} failed")
            };
            reports::DaemonActionReport {
                daemon: kind_id.to_string(),
                verb: verb.to_string(),
                generation: 0,
                ok,
                summary,
                detail: format!(
                    "cmd: {argv}\nexit: {}\n\nstdout:\n{}\n\nstderr:\n{}",
                    out.status,
                    if stdout.is_empty() { "(none)" } else { &stdout },
                    if stderr.is_empty() { "(none)" } else { &stderr },
                ),
                local: None,
            }
        }
        Err(e) => reports::DaemonActionReport {
            daemon: kind_id.to_string(),
            verb: verb.to_string(),
            generation: 0,
            ok: false,
            summary: format!("{verb} failed"),
            detail: format!("cmd: {argv}\ncould not run it: {e}"),
            local: None,
        },
    }
}

fn open_editor(path: &std::path::Path, preferred_editor: Option<&str>) -> Intent {
    info!("[EFFECT] Opening in editor: {:?}", path);
    let Some(editor) = resolve_editor(preferred_editor) else {
        warn!("No editor found in fallback chain");
        return reports::editor_finished(&EditorOutcome::NoneFound);
    };
    info!("Opening {} in {}", path.display(), editor);
    // `--` ends option parsing, so a path that starts with `-` is opened, not
    // read as an editor flag.
    let outcome = match std::process::Command::new(&editor).arg("--").arg(path).spawn() {
        Ok(_) => EditorOutcome::Opened(editor),
        Err(e) => {
            error!("Failed to open editor: {}", e);
            EditorOutcome::Failed(e.to_string())
        }
    };
    reports::editor_finished(&outcome)
}

/// The editor to run: the configured preference, then `code`, then `$EDITOR`,
/// whichever is on `PATH` first.
fn resolve_editor(preferred_editor: Option<&str>) -> Option<String> {
    if let Some(editor) = preferred_editor {
        if command_exists(editor) {
            return Some(editor.to_string());
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
mod daemon_action_tests {
    use super::{spawn_daemon_action, take_deferred_reports};
    use crate::app::{CommandId, Intent};

    /// The verb runs on a worker, and its report reaches the run loop's queue
    /// once it exits. Under `cargo test` the runner refuses to self-exec, which
    /// is itself the reported failure.
    #[test]
    fn a_daemon_verb_reports_through_the_deferred_queue() {
        spawn_daemon_action(
            crate::fleet::daemons::probe::DaemonKind::McpPool,
            crate::cli::daemon::Action::Stop,
            7,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut reports = Vec::new();
        while reports.is_empty() && std::time::Instant::now() < deadline {
            reports = take_deferred_reports();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let [Intent::Command(id, args)] = reports.as_slice() else {
            panic!("expected one report, got {reports:?}");
        };
        assert_eq!(
            id,
            &CommandId::new(crate::app::reports::ids::DAEMON_ACTION_FINISHED)
        );
        assert_eq!(args["report"]["daemon"], "mcp-pool");
        assert_eq!(args["report"]["verb"], "stop");
        assert_eq!(
            args["report"]["generation"], 7,
            "the report answers its request"
        );
        assert_eq!(args["report"]["ok"], false);
        assert!(
            take_deferred_reports().is_empty(),
            "a report is handed over once"
        );
    }

    /// A worker that panics holding the queue lock must not cost later
    /// reports: the next drain recovers the queue and hands them over.
    #[test]
    fn a_poisoned_report_queue_still_hands_over_its_reports() {
        // Its own queue, so the process-wide one other tests drain is untouched.
        let (tx, rx) = std::sync::mpsc::channel();
        let queue = std::sync::Arc::new(std::sync::Mutex::new(rx));
        let held = std::sync::Arc::clone(&queue);
        let _ = std::thread::spawn(move || {
            let _held = held.lock();
            panic!("worker dies holding the queue");
        })
        .join();
        assert!(queue.is_poisoned());

        let report = crate::app::reports::detached();
        tx.send(report.clone()).expect("queue open");

        assert_eq!(super::drain(&queue), vec![report]);
    }
}

#[cfg(test)]
mod tmux_presence_tests {
    use super::is_explicitly_missing_tmux_target;

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
