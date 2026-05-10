// ABOUTME: Main entry point for Agents-in-a-Box with TUI and CLI support
//
// Binary: ainb
// Usage: ainb [COMMAND]
// - No command: launches TUI
// - run: spawn new AI coding session
// - list: show all sessions
// - attach: attach to session's tmux
// - logs: view session output
// - status: check session status
// - kill: terminate session
// - auth: set up authentication

#![allow(missing_docs)]

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::Backend, prelude::*};
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

mod agent_parsers;
mod app;
mod audit;
mod claude;
mod cli;
mod components;
mod config;
mod credentials;
mod docker;
mod editors;
mod git;
mod interactive;
mod models;
mod tmux;
mod usage_cache;
mod widgets;

#[cfg(any(test, feature = "test-support"))]
mod test_support;

use app::{App, EventHandler};
use components::LayoutComponent;

/// Terminal cleanup utility to ensure proper restoration
fn cleanup_terminal() {
    let _ = disable_raw_mode();
    // Use stdout for cleanup since that's where we enabled mouse capture
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    );
}

/// Unified terminal cleanup that works with a terminal instance
fn cleanup_terminal_with_instance<B: Backend + std::io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging();
    setup_panic_handler();

    let args = cli::Cli::parse();

    // Track whether we entered TUI mode so we only clean up terminal in that case.
    // CLI commands never touch the alternate screen; emitting LeaveAlternateScreen
    // would leak raw escape codes into the user's terminal.
    let mut entered_tui = false;

    let result = match args.command {
        // CLI commands
        Some(cli::Commands::Run(run_args)) => cli::run::execute(run_args).await,
        Some(cli::Commands::List(list_args)) => cli::list::execute(list_args, args.format).await,
        Some(cli::Commands::Logs(logs_args)) => cli::logs::execute(logs_args).await,
        Some(cli::Commands::Attach(attach_args)) => cli::attach::execute(attach_args).await,
        Some(cli::Commands::Status(status_args)) => {
            cli::status::execute(status_args, args.format).await
        }
        Some(cli::Commands::Kill(kill_args)) => cli::status::kill(kill_args).await,
        Some(cli::Commands::Auth) => run_auth_setup().await,
        Some(cli::Commands::Recover { command }) => {
            cli::recover::execute(command, args.format).await
        }
        Some(cli::Commands::Config { command }) => {
            cli::config_cmd::execute(command, args.format).await
        }
        Some(cli::Commands::Git { command }) => cli::git_cmd::execute(command, args.format).await,
        Some(cli::Commands::Favorites { command }) => {
            cli::favorites::execute(command, args.format).await
        }
        Some(cli::Commands::Init(init_args)) => cli::init::execute(init_args, args.format).await,
        Some(cli::Commands::Presets { command }) => {
            cli::presets::execute(command, args.format).await
        }
        Some(cli::Commands::Usage { command }) => cli::usage::execute(command, args.format).await,
        // New canonical namespace: `ainb claudecode statusline`. Legacy
        // top-level `Statusline` dispatches to the same handler so
        // existing `~/.claude/settings.json` entries keep working
        // unchanged.
        Some(cli::Commands::Claudecode {
            cmd: cli::ClaudeCodeCmd::Statusline { cache_only },
        }) => cli::statusline::execute(cache_only),
        // Legacy top-level alias has no flag — preserves the original
        // wire-format for `~/.claude/settings.json` entries written
        // before --cache-only existed.
        Some(cli::Commands::Statusline) => cli::statusline::execute(false),
        Some(cli::Commands::Completion { shell }) => {
            use clap::CommandFactory;
            let mut cmd = cli::Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }

        // TUI mode (explicit or default)
        Some(cli::Commands::Tui) | None => {
            entered_tui = true;
            let mut app = App::new();
            app.init().await;
            let mut layout = LayoutComponent::new();

            // Check if first-time setup is needed
            if app::state::AppState::needs_onboarding() {
                tracing::info!("First-time setup detected - starting onboarding wizard");
                app.state.start_onboarding(false, None);
            }

            // Always clear pending async actions after init to ensure clean startup
            app.state.pending_async_action = None;

            // Flush any pending terminal events to prevent stray keypresses
            // from interfering with onboarding or initial view
            while crossterm::event::poll(std::time::Duration::from_millis(10)).unwrap_or(false) {
                let _ = crossterm::event::read();
            }

            run_tui(&mut app, &mut layout).await
        }
    };

    // Only clean up terminal if we entered TUI mode. For CLI commands, calling
    // cleanup_terminal() would emit terminal escape sequences into the user's
    // shell, corrupting agent-captured output.
    if result.is_err() && entered_tui {
        cleanup_terminal();
    }

    result
}

async fn run_auth_setup() -> Result<()> {
    println!("🔐 Setting up Claude authentication for agents-in-a-box...");
    println!();

    // Create the auth directory structure
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let claude_box_dir = home_dir.join(".agents-in-a-box");
    let auth_dir = claude_box_dir.join("auth");

    std::fs::create_dir_all(&auth_dir)
        .map_err(|e| anyhow::anyhow!("Failed to create auth directory: {}", e))?;

    // Check if credentials already exist
    let credentials_path = auth_dir.join(".credentials.json");
    if credentials_path.exists() {
        println!("✅ Authentication already set up!");
        println!("   Credentials found at: {}", credentials_path.display());
        println!();
        println!("To re-authenticate, delete the credentials file and run this command again:");
        println!("   rm {}", credentials_path.display());
        return Ok(());
    }

    println!("📁 Creating auth directories...");
    println!("   Auth directory: {}", auth_dir.display());

    // Check if Docker is available
    let docker_version =
        std::process::Command::new("docker").args(["--version"]).output().map_err(|e| {
            anyhow::anyhow!(
                "Docker not found: {}. Please install Docker and try again.",
                e
            )
        })?;

    if !docker_version.status.success() {
        return Err(anyhow::anyhow!(
            "Docker is not running. Please start Docker and try again."
        ));
    }

    println!("🏗️  Building authentication container (agents-dev)...");
    let build_status = std::process::Command::new("docker")
        .args(["build", "-t", "agents-box:agents-dev", "docker/agents-dev"])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to build container: {}", e))?;

    if !build_status.success() {
        return Err(anyhow::anyhow!(
            "Container build failed. Please check Docker and try again."
        ));
    }

    // Execute the auth container
    println!();
    println!("🚀 Running authentication setup...");
    println!("   This will prompt you to enter your Anthropic API token.");
    println!();

    let status = std::process::Command::new("docker")
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
            "-w",
            "/home/claude-user",
            "--user",
            "claude-user",
            "--entrypoint",
            "bash",
            "agents-box:agents-dev",
            "-c",
            "/app/scripts/auth-setup.sh",
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run auth container: {}", e))?;

    if status.success() {
        println!();
        println!("🎉 Authentication setup complete!");
        println!("   Credentials saved to: {}", credentials_path.display());
        println!();
        println!("You can now create agents-box development sessions with:");
        println!("   agents-box");
    } else {
        println!();
        println!("❌ Authentication setup failed!");
        println!("   Please check the output above for errors and try again.");
        std::process::exit(1);
    }

    Ok(())
}

async fn run_tui(app: &mut App, layout: &mut LayoutComponent) -> Result<()> {
    // Check if we have a proper TTY
    if !IsTerminal::is_terminal(&io::stdout()) {
        return Err(anyhow::anyhow!(
            "No TTY detected. This application requires a terminal.\n\
             Try running directly in a terminal instead of redirecting output."
        ));
    }

    // Check if we're in a proper terminal
    match crossterm::terminal::is_raw_mode_enabled() {
        Ok(false) => {
            // Raw mode is not enabled, which is normal - we'll enable it
        }
        Err(e) => {
            eprintln!("Cannot check terminal raw mode: {}", e);
            return Err(anyhow::anyhow!("Terminal not compatible: {}", e));
        }
        Ok(true) => {
            // Raw mode is already enabled, continue
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Ensure terminal cleanup happens even if there's an error
    let result = run_tui_loop(app, layout, &mut terminal).await;

    // Always clean up terminal using unified cleanup
    if let Err(e) = cleanup_terminal_with_instance(&mut terminal) {
        tracing::error!("Failed to cleanup terminal: {}", e);
        // Fallback to basic cleanup
        cleanup_terminal();
    }

    result
}

async fn run_tui_loop(
    app: &mut App,
    layout: &mut LayoutComponent,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();

    // Startup guard: Ignore key events for the first 100ms to prevent stray keypresses
    // from triggering actions (e.g., buffered 'n' key opening New Session dialog)
    let startup_time = Instant::now();
    const STARTUP_GUARD_MS: u64 = 100;

    loop {
        terminal.draw(|frame| {
            layout.render(frame, &mut app.state);
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            match event::read()? {
                Event::Key(key_event) => {
                    // Windows fires Press + Release for every key; macOS/Linux fire only Press.
                    // Drop Release so Enter doesn't immediately re-trigger and close popups.
                    if key_event.kind == KeyEventKind::Release {
                        continue;
                    }

                    // Startup guard: Ignore key events during startup period
                    if startup_time.elapsed() < Duration::from_millis(STARTUP_GUARD_MS) {
                        tracing::debug!(
                            "Ignoring key event {:?} during startup guard period",
                            key_event.code
                        );
                        continue;
                    }

                    // Intercept keys when tmux preview is in scroll mode
                    use crossterm::event::KeyCode;
                    let preview = layout.tmux_preview_mut();
                    if preview.is_scroll_mode() {
                        match key_event.code {
                            KeyCode::Esc => {
                                preview.exit_scroll_mode();
                                continue; // Don't process ESC as Quit
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                preview.scroll_up();
                                continue; // Don't let event handler navigate sessions
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                preview.scroll_down();
                                continue; // Don't let event handler navigate sessions
                            }
                            KeyCode::PageUp => {
                                preview.scroll_page_up();
                                continue;
                            }
                            KeyCode::PageDown => {
                                preview.scroll_page_down();
                                continue;
                            }
                            _ => {} // Let other keys pass through to event handler
                        }
                    }

                    if let Some(app_event) =
                        EventHandler::handle_key_event(key_event, &mut app.state)
                    {
                        // Handle scroll events for live logs and tmux preview
                        use crate::app::events::AppEvent;
                        match app_event {
                            AppEvent::ScrollLogsUp => {
                                layout.live_logs_mut().scroll_up();
                            }
                            AppEvent::ScrollLogsDown => {
                                let total_logs =
                                    app.state.live_logs.values().map(|v| v.len()).sum::<usize>();
                                layout.live_logs_mut().scroll_down(total_logs);
                            }
                            AppEvent::ScrollLogsToTop => {
                                layout.live_logs_mut().scroll_to_top();
                            }
                            AppEvent::ScrollLogsToBottom => {
                                let total_logs =
                                    app.state.live_logs.values().map(|v| v.len()).sum::<usize>();
                                layout.live_logs_mut().scroll_to_bottom(total_logs);
                            }
                            AppEvent::ToggleAutoScroll => {
                                layout.live_logs_mut().toggle_auto_scroll();
                            }
                            // Tmux preview scroll events
                            AppEvent::ScrollPreviewUp => {
                                let preview = layout.tmux_preview_mut();
                                if !preview.is_scroll_mode() {
                                    preview.enter_scroll_mode();
                                }
                                preview.scroll_up();
                            }
                            AppEvent::ScrollPreviewDown => {
                                let preview = layout.tmux_preview_mut();
                                if !preview.is_scroll_mode() {
                                    preview.enter_scroll_mode();
                                }
                                preview.scroll_down();
                            }
                            AppEvent::EnterScrollMode => {
                                layout.tmux_preview_mut().enter_scroll_mode();
                            }
                            AppEvent::ExitScrollMode => {
                                layout.tmux_preview_mut().exit_scroll_mode();
                            }
                            AppEvent::NewSession
                            | AppEvent::SearchWorkspace
                            | AppEvent::NewSessionCreate
                            | AppEvent::ConfirmationConfirm => {
                                // Process the event to queue the async action
                                EventHandler::process_event(app_event, &mut app.state);

                                // IMMEDIATELY process the async action for responsive UI
                                // This ensures dialogs appear without delay and session creation/deletion starts immediately
                                use tracing::{error, info};
                                info!(">>> Immediately processing async action for responsive UI");
                                match app.tick().await {
                                    Ok(()) => {
                                        info!(">>> Immediate tick completed successfully");
                                        last_tick = Instant::now();
                                        // Force UI refresh
                                        terminal.draw(|frame| {
                                            layout.render(frame, &mut app.state);
                                        })?;
                                    }
                                    Err(e) => {
                                        error!(">>> Error during immediate tick: {}", e);
                                    }
                                }
                            }
                            _ => {
                                // Process other events normally
                                EventHandler::process_event(app_event, &mut app.state);
                            }
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    use crate::app::events::AppEvent;
                    use crossterm::event::{MouseButton, MouseEventKind};

                    match mouse_event.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            // Convert coordinates to pane focus
                            let (col, row) = (mouse_event.column, mouse_event.row);

                            // Handle log history view clicks directly
                            if app.state.current_view == crate::app::state::View::LogHistory {
                                // Log history viewer takes full screen, starts at (0, 0)
                                app.state.log_history_state.handle_click(col, row, 0, 0);
                            } else if let Some(app_event) = EventHandler::handle_mouse_event(
                                AppEvent::MouseClick { x: col, y: row },
                                &mut app.state,
                            ) {
                                EventHandler::process_event(app_event, &mut app.state);
                            }
                        }
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            // Handle mouse scroll based on current view
                            use crate::app::state::View;
                            const SCROLL_LINES: usize = 3; // Lines per mouse wheel tick
                            let is_down = matches!(mouse_event.kind, MouseEventKind::ScrollDown);

                            if app.state.current_view == View::HomeScreen {
                                // Scroll welcome panel on home screen (right side only)
                                // Sidebar is ~26 chars wide, so anything beyond that is the welcome panel
                                let sidebar_width = 26u16;
                                if mouse_event.column > sidebar_width {
                                    for _ in 0..SCROLL_LINES {
                                        if is_down {
                                            app.state.home_screen_v2_state.welcome.scroll_down();
                                        } else {
                                            app.state.home_screen_v2_state.welcome.scroll_up();
                                        }
                                    }
                                }
                            } else if app.state.current_view == View::GitView {
                                // Scroll git view content (markdown or diff)
                                if let Some(ref mut git_state) = app.state.git_view_state {
                                    match git_state.active_tab {
                                        crate::components::git_view::GitTab::Diff => {
                                            if is_down {
                                                git_state.scroll_diff_down_by(SCROLL_LINES);
                                            } else {
                                                git_state.scroll_diff_up_by(SCROLL_LINES);
                                            }
                                        }
                                        crate::components::git_view::GitTab::Markdown => {
                                            if is_down {
                                                git_state.scroll_markdown_down_by(SCROLL_LINES);
                                            } else {
                                                git_state.scroll_markdown_up_by(SCROLL_LINES);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            } else if app.state.current_view == View::LogHistory {
                                // Scroll log history viewer
                                // Shift+Scroll = horizontal, normal scroll = vertical
                                if mouse_event
                                    .modifiers
                                    .contains(crossterm::event::KeyModifiers::SHIFT)
                                {
                                    // Horizontal scroll
                                    if is_down {
                                        app.state.log_history_state.scroll_right(SCROLL_LINES * 4);
                                    } else {
                                        app.state.log_history_state.scroll_left(SCROLL_LINES * 4);
                                    }
                                } else {
                                    // Vertical scroll
                                    if is_down {
                                        app.state.log_history_state.scroll_down_by(SCROLL_LINES);
                                    } else {
                                        app.state.log_history_state.scroll_up_by(SCROLL_LINES);
                                    }
                                }
                            } else {
                                // Default: scroll live logs
                                if is_down {
                                    let total_logs = app
                                        .state
                                        .live_logs
                                        .values()
                                        .map(|v| v.len())
                                        .sum::<usize>();
                                    layout.live_logs_mut().scroll_down(total_logs);
                                } else {
                                    layout.live_logs_mut().scroll_up();
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            let (col, row) = (mouse_event.column, mouse_event.row);

                            // Handle log history text selection drag
                            if app.state.current_view == crate::app::state::View::LogHistory {
                                app.state.log_history_state.update_selection(col, row);
                            } else if let Some(app_event) = EventHandler::handle_mouse_event(
                                AppEvent::MouseDragging { x: col, y: row },
                                &mut app.state,
                            ) {
                                EventHandler::process_event(app_event, &mut app.state);
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            let (col, row) = (mouse_event.column, mouse_event.row);

                            // Handle log history text selection end
                            if app.state.current_view == crate::app::state::View::LogHistory {
                                app.state.log_history_state.end_selection();
                            } else if let Some(app_event) = EventHandler::handle_mouse_event(
                                AppEvent::MouseDragEnd { x: col, y: row },
                                &mut app.state,
                            ) {
                                EventHandler::process_event(app_event, &mut app.state);
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    // Clear terminal buffer on resize to prevent ghost/duplicate UI elements
                    // The old frame buffer contains data for the previous terminal size,
                    // which can cause stale content to appear without this clear
                    terminal.clear()?;
                }
                Event::FocusGained => {}
                Event::FocusLost => {}
                Event::Paste(text) => {
                    if let Some(app_event) = EventHandler::handle_paste_event(text, &app.state) {
                        EventHandler::process_event(app_event, &mut app.state);
                    }
                }
            }
        }

        // Process any pending events
        if let Some(pending_event) = app.state.pending_event.take() {
            EventHandler::process_event(pending_event, &mut app.state);
        }

        if last_tick.elapsed() >= tick_rate {
            // Update mascot animation on home screen
            app.state.home_screen_v2_state.tick_mascot();

            // Handle tmux-related async actions BEFORE app.tick() to get terminal access
            // IMPORTANT: Use match instead of multiple if-let with .take() to avoid dropping unmatched actions
            if let Some(action) = app.state.pending_async_action.take() {
                use crate::app::state::AsyncAction;
                use tracing::{debug, error, info, warn};

                match action {
                    AsyncAction::AttachToOtherTmux(session_name) => {
                        use crate::app::AttachHandler;

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
                                app.state
                                    .add_error_notification(format!("Failed to attach: {}", e));
                            }
                        }

                        // Refresh other tmux sessions list after detach
                        app.state.load_other_tmux_sessions().await;
                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::KillOtherTmux(session_name) => {
                        use tokio::process::Command;

                        info!("Killing other tmux session '{}'", session_name);

                        let output = Command::new("tmux")
                            .args(["kill-session", "-t", &session_name])
                            .output()
                            .await;

                        match output {
                            Ok(o) if o.status.success() => {
                                info!("Successfully killed tmux session '{}'", session_name);
                                app.state.add_success_notification(format!(
                                    "Killed tmux session '{}'",
                                    session_name
                                ));
                                // Clear selection if we just killed the selected session
                                if app.state.selected_other_tmux_session().map(|s| s.name.as_str())
                                    == Some(&session_name)
                                {
                                    app.state.selected_other_tmux_index = None;
                                }
                            }
                            Ok(o) => {
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                warn!("Failed to kill tmux session '{}': {}", session_name, stderr);
                                app.state.add_error_notification(format!(
                                    "Failed to kill session: {}",
                                    stderr
                                ));
                            }
                            Err(e) => {
                                warn!("Failed to kill tmux session '{}': {}", session_name, e);
                                app.state.add_error_notification(format!(
                                    "Failed to kill session: {}",
                                    e
                                ));
                            }
                        }

                        // Refresh other tmux sessions list
                        app.state.load_other_tmux_sessions().await;
                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::OpenInEditor(workspace_path) => {
                        info!("[ACTION] Opening workspace in editor: {:?}", workspace_path);

                        // Resolve editor using fallback chain
                        let editor = resolve_editor(&app.state.app_config);

                        match editor {
                            Some(cmd) => {
                                info!("Opening {} in {}", workspace_path.display(), cmd);

                                let result =
                                    std::process::Command::new(&cmd).arg(&workspace_path).spawn();

                                match result {
                                    Ok(_) => {
                                        app.state.add_success_notification(format!(
                                            "📝 Opened in {}",
                                            cmd
                                        ));
                                    }
                                    Err(e) => {
                                        error!("Failed to open editor: {}", e);
                                        app.state.add_error_notification(format!(
                                            "❌ Failed to open editor: {}",
                                            e
                                        ));
                                    }
                                }
                            }
                            None => {
                                warn!("No editor found in fallback chain");
                                app.state.add_error_notification(
                                    "❌ No editor found. Set preferred editor in settings or install VS Code.".to_string()
                                );
                            }
                        }
                    }

                    // Workspace shell handling (one shell per workspace, cd to switch directories)
                    AsyncAction::OpenWorkspaceShell {
                        workspace_index,
                        target_dir,
                    } => {
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
                            if let Some(workspace) = app.state.workspaces.get(workspace_index) {
                                (
                                    workspace.path.clone(),
                                    workspace.name.clone(),
                                    workspace
                                        .shell_session
                                        .as_ref()
                                        .map(|s| s.tmux_session_name.clone()),
                                )
                            } else {
                                app.state.add_error_notification("Workspace not found".to_string());
                                app.state.ui_needs_refresh = true;
                                continue;
                            }
                        };

                        // Determine tmux session name - use existing or create new
                        let (tmux_name, is_new_shell) = if let Some(existing) = existing_shell {
                            (existing, false)
                        } else {
                            let shell = ShellSession::new_workspace_shell(
                                workspace_path.clone(),
                                &workspace_name,
                            );
                            let name = shell.tmux_session_name.clone();
                            // Store the new shell in workspace
                            if let Some(workspace) = app.state.workspaces.get_mut(workspace_index) {
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
                                app.state.add_error_notification(format!(
                                    "Failed to create shell: {}",
                                    stderr
                                ));
                                app.state.ui_needs_refresh = true;
                                continue;
                            }
                            Err(e) => {
                                error!("[ACTION] Failed to create tmux session: {}", e);
                                app.state.add_error_notification(format!(
                                    "Failed to create shell: {}",
                                    e
                                ));
                                app.state.ui_needs_refresh = true;
                                continue;
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
                                    if let Some(workspace) =
                                        app.state.workspaces.get_mut(workspace_index)
                                    {
                                        if let Some(shell) = workspace.get_shell_session_mut() {
                                            shell.set_working_dir(dir.clone());
                                        }
                                    }
                                }
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    warn!("[ACTION] tmux send-keys may have failed: {}", stderr);
                                    app.state.add_warning_notification(format!(
                                        "May have failed to cd to: {}",
                                        dir_str
                                    ));
                                }
                                Err(e) => {
                                    error!("[ACTION] tmux send-keys error: {}", e);
                                    app.state.add_error_notification(format!(
                                        "Shell command error: {}",
                                        e
                                    ));
                                }
                            }
                        }

                        // Update shell's last accessed time
                        if let Some(workspace) = app.state.workspaces.get_mut(workspace_index) {
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
                                app.state
                                    .add_error_notification(format!("Failed to attach: {}", e));
                            }
                        }

                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::OpenShellAtPath(repo_path) => {
                        use crate::app::AttachHandler;
                        use tokio::process::Command;

                        info!("[ACTION] Opening shell at path: {:?}", repo_path);

                        // Generate a simple tmux session name based on repo directory
                        // Sanitize repo name: periods are tmux session.window delimiters
                        let repo_name = repo_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("shell")
                            .replace('.', "-") // Periods break tmux (session.window delimiter)
                            .replace(':', "-") // Colons are special in tmux
                            .replace('/', "-"); // Slashes for safety
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let tmux_name = format!("shell-{}-{}", repo_name, timestamp % 10000);

                        let repo_path_str = repo_path.to_str().unwrap_or(".");

                        // Check if session already exists
                        let has_session = Command::new("tmux")
                            .args(["has-session", "-t", &tmux_name])
                            .output()
                            .await
                            .map(|o| o.status.success())
                            .unwrap_or(false);

                        if !has_session {
                            // Create new tmux session (detached so we can attach via AttachHandler)
                            let create_result = Command::new("tmux")
                                .args([
                                    "new-session",
                                    "-d", // Start detached
                                    "-s",
                                    &tmux_name,
                                    "-c",
                                    repo_path_str, // Set working directory
                                ])
                                .output()
                                .await;

                            match create_result {
                                Ok(output) if output.status.success() => {
                                    // Configure clipboard for the tmux session
                                    if let Err(e) =
                                        crate::tmux::configure_clipboard(&tmux_name).await
                                    {
                                        warn!("[ACTION] Failed to configure clipboard: {}", e);
                                    }
                                    info!("[ACTION] Created tmux session: {}", tmux_name);
                                }
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    error!("[ACTION] tmux session creation failed: {}", stderr);
                                    app.state.add_error_notification(format!(
                                        "Shell creation failed: {}",
                                        stderr
                                    ));
                                    app.state.ui_needs_refresh = true;
                                    continue;
                                }
                                Err(e) => {
                                    error!("[ACTION] tmux command error: {}", e);
                                    app.state.add_error_notification(format!("Shell error: {}", e));
                                    app.state.ui_needs_refresh = true;
                                    continue;
                                }
                            }
                        } else {
                            // Ensure clipboard is configured even for existing sessions
                            if let Err(e) = crate::tmux::configure_clipboard(&tmux_name).await {
                                warn!("[ACTION] Failed to configure clipboard: {}", e);
                            }
                            info!("[ACTION] Reusing existing tmux session: {}", tmux_name);
                        }

                        // Attach to the shell
                        let mut attach_handler = AttachHandler::new_from_terminal(terminal)?;
                        match attach_handler.attach_to_session(&tmux_name).await {
                            Ok(()) => {
                                info!("[ACTION] Successfully attached to shell at {:?}", repo_path);
                                app.state.add_success_notification(format!(
                                    "Shell opened at: {}",
                                    repo_name
                                ));
                            }
                            Err(e) => {
                                error!("[ACTION] Failed to attach to shell: {}", e);
                                app.state
                                    .add_error_notification(format!("Failed to attach: {}", e));
                            }
                        }

                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::CreateSshSession => {
                        use crate::app::AttachHandler;
                        use tokio::process::Command;

                        info!("[ACTION] Creating SSH session");

                        // Get SSH target from state
                        let ssh_target = app.state.build_ssh_target();
                        let repo_path = app
                            .state
                            .new_session_state
                            .as_ref()
                            .and_then(|s| s.get_selected_repo_path());

                        // Clear new session state
                        app.state.new_session_state = None;
                        app.state.current_view = crate::app::state::View::SessionList;

                        if let Some(target) = ssh_target {
                            let ssh_cmd = target.to_ssh_command();
                            let display_name = target.display_name();

                            // Generate tmux session name for SSH
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let sanitized_host = target.host.replace('.', "-").replace(':', "-");
                            let tmux_name = format!("ssh-{}-{}", sanitized_host, timestamp % 10000);

                            // Determine working directory (use repo path if available, else home)
                            let work_dir =
                                repo_path.as_ref().and_then(|p| p.to_str()).unwrap_or("~");

                            // Create tmux session first (detached)
                            let create_result = Command::new("tmux")
                                .args(["new-session", "-d", "-s", &tmux_name, "-c", work_dir])
                                .output()
                                .await;

                            match create_result {
                                Ok(output) if output.status.success() => {
                                    info!("[ACTION] Created tmux session for SSH: {}", tmux_name);

                                    // Configure clipboard
                                    if let Err(e) =
                                        crate::tmux::configure_clipboard(&tmux_name).await
                                    {
                                        warn!("[ACTION] Failed to configure clipboard: {}", e);
                                    }

                                    // Send SSH command to the tmux session
                                    let send_result = Command::new("tmux")
                                        .args(["send-keys", "-t", &tmux_name, &ssh_cmd, "Enter"])
                                        .output()
                                        .await;

                                    if let Err(e) = send_result {
                                        warn!("[ACTION] Failed to send SSH command: {}", e);
                                    }

                                    // Attach to the SSH session
                                    let mut attach_handler =
                                        AttachHandler::new_from_terminal(terminal)?;
                                    match attach_handler.attach_to_session(&tmux_name).await {
                                        Ok(()) => {
                                            info!(
                                                "[ACTION] Successfully attached to SSH session: {}",
                                                display_name
                                            );
                                            app.state.add_success_notification(format!(
                                                "SSH: {}",
                                                display_name
                                            ));
                                        }
                                        Err(e) => {
                                            error!(
                                                "[ACTION] Failed to attach to SSH session: {}",
                                                e
                                            );
                                            app.state.add_error_notification(format!(
                                                "SSH attach failed: {}",
                                                e
                                            ));
                                        }
                                    }
                                }
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    error!("[ACTION] SSH tmux session creation failed: {}", stderr);
                                    app.state.add_error_notification(format!(
                                        "SSH session failed: {}",
                                        stderr
                                    ));
                                }
                                Err(e) => {
                                    error!("[ACTION] tmux command error for SSH: {}", e);
                                    app.state.add_error_notification(format!("SSH error: {}", e));
                                }
                            }
                        } else {
                            app.state
                                .add_error_notification("SSH target not configured".to_string());
                        }

                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::KillWorkspaceShell(workspace_index) => {
                        use tokio::process::Command;

                        info!(
                            "[ACTION] Killing workspace shell, index: {}",
                            workspace_index
                        );

                        // Extract info first to avoid borrow issues
                        let shell_info = if let Some(workspace) =
                            app.state.workspaces.get_mut(workspace_index)
                        {
                            if let Some(shell) = workspace.shell_session.take() {
                                Some((shell.tmux_session_name.clone(), workspace.name.clone()))
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some((tmux_name, workspace_name)) = shell_info {
                            // Kill the tmux session
                            let _ = Command::new("tmux")
                                .args(["kill-session", "-t", &tmux_name])
                                .output()
                                .await;

                            app.state.add_success_notification(format!(
                                "Killed workspace shell: {}",
                                workspace_name
                            ));
                        }

                        // Refresh workspace list to ensure UI reflects the actual state
                        app.state.load_real_workspaces().await;
                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::AttachToTmuxSession(session_id) => {
                        use crate::app::AttachHandler;

                        info!(
                            "[ACTION] Handling AttachToTmuxSession for session {}",
                            session_id
                        );
                        debug!(
                            "[ACTION] Looking for session in {} workspaces",
                            app.state.workspaces.len()
                        );

                        // Get session to find tmux session name
                        let tmux_session_name = if let Some(session) = app
                            .state
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
                                app.state.add_error_notification(format!(
                                    "Session '{}' has no tmux session",
                                    session.name
                                ));
                                app.state.ui_needs_refresh = true;
                                None
                            }
                        } else {
                            error!("[ACTION] Session {} not found in workspaces", session_id);
                            app.state.add_error_notification("Session not found".to_string());
                            app.state.ui_needs_refresh = true;
                            None
                        };

                        if let Some(tmux_session_name) = tmux_session_name {
                            // Mark session as attached
                            for workspace in &mut app.state.workspaces {
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
                                    app.state
                                        .add_error_notification(format!("Failed to attach: {}", e));
                                }
                            }

                            // Mark session as detached
                            for workspace in &mut app.state.workspaces {
                                for session in &mut workspace.sessions {
                                    if session.id == session_id {
                                        session.mark_detached();
                                        break;
                                    }
                                }
                            }

                            app.state.ui_needs_refresh = true;
                        }
                    }

                    // Put back any other actions we don't handle here
                    other => {
                        debug!(
                            "[ACTION] Passing through unhandled action in main loop: {:?}",
                            std::any::type_name_of_val(&other)
                        );
                        app.state.pending_async_action = Some(other);
                    }
                }
            }

            match app.tick().await {
                Ok(()) => {
                    last_tick = Instant::now();

                    // Check if UI needs immediate refresh after async operations
                    if app.needs_ui_refresh() {
                        // Force immediate redraw by skipping the timeout
                        terminal.draw(|frame| {
                            layout.render(frame, &mut app.state);
                        })?;
                    }
                }
                Err(e) => {
                    use tracing::error;
                    error!("Error during app tick: {}", e);
                    // Continue running instead of crashing
                    last_tick = Instant::now();
                }
            }
        }

        if app.state.should_quit {
            break;
        }
    }

    Ok(())
}

fn setup_logging() {
    use std::fs::OpenOptions;
    use std::path::PathBuf;
    use tracing_subscriber::prelude::*;

    // Create log directory if it doesn't exist
    let log_dir = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".agents-in-a-box").join("logs"))
        .unwrap_or_else(|_| PathBuf::from(".agents-in-a-box/logs"));

    let _ = std::fs::create_dir_all(&log_dir);

    // Create JSONL log file with timestamp
    let log_file = log_dir.join(format!(
        "agents-in-a-box-{}.jsonl",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));

    // Open file for writing
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .expect("Failed to create log file");

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json() // Output in JSON Lines format
                .with_target(true) // Include target module in JSON
                .with_writer(file)
                .with_ansi(false),
        )
        .with(
            // Comprehensive default: `ainb` (this crate) at debug so
            // every traced event from our own code lands in the JSONL,
            // and `warn` globally so noisy dependencies (bollard, hyper,
            // tokio, etc.) only surface real problems. Override at any
            // time with `RUST_LOG`. The previous default,
            // `agents_box=info`, never matched — the crate was renamed
            // to `ainb` years ago — so log files were empty by
            // construction.
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ainb=debug,warn".into()),
        )
        .init();
}

fn setup_panic_handler() {
    use tracing::error;

    std::panic::set_hook(Box::new(|panic_info| {
        // Ensure terminal is restored before logging the panic
        cleanup_terminal();

        error!("Application panicked: {}", panic_info);
        eprintln!("Application panicked: {}", panic_info);
        eprintln!("Please check the logs for more details.");
    }));
}

/// Resolve which editor to use via fallback chain:
/// 1. preferred_editor from config
/// 2. 'code' (VS Code)
/// 3. $EDITOR env var
/// 4. None (error)
fn resolve_editor(config: &crate::config::AppConfig) -> Option<String> {
    // 1. Check preferred_editor from config
    if let Some(ref editor) = config.ui_preferences.preferred_editor {
        if command_exists(editor) {
            return Some(editor.clone());
        }
    }

    // 2. Fallback to 'code' (VS Code)
    if command_exists("code") {
        return Some("code".to_string());
    }

    // 3. Fallback to $EDITOR env var
    if let Ok(editor) = std::env::var("EDITOR") {
        if command_exists(&editor) {
            return Some(editor);
        }
    }

    // 4. No editor found
    None
}

/// Check if a command exists on the system
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
