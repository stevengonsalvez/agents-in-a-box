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
mod agents;
mod app;
mod audit;
mod claude;
mod cli;
mod components;
mod config;
mod credentials;
mod docker;
mod editors;
mod fleet;
mod git;
mod interactive;
mod models;
mod plugins;
mod providers;
mod tmux;
mod usage_cache;
mod widgets;

#[cfg(any(test, feature = "test-support"))]
mod test_support;

use app::{App, EventHandler};
use components::LayoutComponent;
use components::slash::{SlashAction, SlashCommandRegistry, SlashPalette};

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

    // Build the clap surface from the CommandRegistry. The base `ainb` command
    // (--format, after-help, etc.) lives in cli::root_clap_command(); each
    // built-in subcommand registers itself via CommandRegistry::built_ins().
    // Adding plugin-supplied subcommands later = registering an extra
    // CliCommand impl (Phase 4); no changes here required.
    let registry = cli::registry::CommandRegistry::built_ins();
    let mut app = cli::root_clap_command();
    // `tui` is handled inline in this function (it owns the alternate-screen
    // setup + cleanup), so it sits outside the registry. Declare it on the
    // base command so help/completion still list it.
    app = app.subcommand(
        clap::Command::new("tui").about("Launch the TUI (default if no command given)"),
    );
    // `diff-review` is also handled inline (owns the alternate screen, like `tui`).
    app = app.subcommand(
        clap::Command::new("diff-review")
            .about("Review a repository's uncommitted changes in the Code Review surface")
            .arg(
                clap::Arg::new("path")
                    .help("Repository path (default: current directory)")
                    .default_value("."),
            ),
    );
    app = registry.build_clap(app);
    let matches = app.get_matches();
    let format = matches.get_one::<cli::OutputFormat>("format").copied().unwrap_or_default();
    let ctx = cli::registry::CliContext { format };

    // Track whether we entered TUI mode so we only clean up terminal in that case.
    // CLI commands never touch the alternate screen; emitting LeaveAlternateScreen
    // would leak raw escape codes into the user's terminal.
    let mut entered_tui = false;

    let result = match matches.subcommand() {
        // TUI: explicit `tui` subcommand or no subcommand at all.
        Some(("tui", _)) | None => {
            entered_tui = true;

            // Best-effort: drop shipped default presets into
            // ~/.agents-in-a-box/presets.toml on first run. Never overwrites
            // user-edited files (see `install_default_presets`). Also migrates
            // away from the legacy per-file `presets/` directory layout when
            // present. Failure here is non-fatal — the TUI still launches;
            // the user just won't see the defaults until they fix the
            // underlying issue (e.g., unwriteable HOME).
            if let Some(home) = dirs::home_dir() {
                let presets_file = home.join(".agents-in-a-box").join("presets.toml");
                if let Err(e) = config::presets::install_default_presets(&presets_file) {
                    tracing::warn!(
                        error = %e,
                        file = %presets_file.display(),
                        "failed to install default presets",
                    );
                }
            }

            let mut app_state = App::new();
            app_state.init().await;

            // Migrate legacy local-path favorites to their remote indicator. A
            // star is always a remote pointer now; local-path entries from
            // older versions are rewritten to their `origin` remote, or dropped
            // when there is none. One-time per launch; idempotent once all
            // entries are remote. Non-fatal — failure just leaves the store as-is.
            {
                let mut favorites = config::FavoritesStore::load();
                let report = favorites.migrate_local_to_remote();
                if !report.is_empty() {
                    // Back up the original (pre-migration) file once before the
                    // destructive overwrite so dropped favorites are recoverable.
                    if let Err(e) = config::FavoritesStore::write_migration_backup() {
                        tracing::warn!(error = %e, "failed to back up favorites before migration");
                    }
                    match favorites.save() {
                        Ok(()) => {
                            // Only claim success once the migrated store is on disk;
                            // otherwise the migration would silently re-run next launch.
                            if !report.migrated.is_empty() {
                                app_state.state.add_info_notification(format!(
                                    "⭐ Migrated {} favorite(s) to remote",
                                    report.migrated.len()
                                ));
                            }
                            if !report.dropped.is_empty() {
                                app_state.state.add_error_notification(format!(
                                    "★ Removed {} local-only favorite(s) with no remote: {}",
                                    report.dropped.len(),
                                    report.dropped.join(", ")
                                ));
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "failed to persist migrated favorites");
                            app_state.state.add_error_notification(format!(
                                "Could not migrate favorites to remote: {e}"
                            ));
                        }
                    }
                }
            }

            let mut layout = LayoutComponent::new();

            // Check if first-time setup is needed
            if app::state::AppState::needs_onboarding() {
                tracing::info!("First-time setup detected - starting onboarding wizard");
                app_state.state.start_onboarding(false, None);
            } else {
                // Existing user (no onboarding to run): offer to install /
                // update the ainb-hooks notification plugin if it's absent
                // or stale. New users get this same prompt at the end of
                // onboarding instead (see `complete_onboarding`). No-op
                // when already up to date or previously declined.
                app_state.state.maybe_prompt_notify_install();
            }

            // Always clear pending async actions after init to ensure clean startup
            app_state.state.pending_async_action = None;

            // Flush any pending terminal events to prevent stray keypresses
            // from interfering with onboarding or initial view
            while crossterm::event::poll(std::time::Duration::from_millis(10)).unwrap_or(false) {
                let _ = crossterm::event::read();
            }

            let tui_result = run_tui(&mut app_state, &mut layout).await;

            // Explicitly tear down the plugin runtime before `app_state`
            // drops. Without this, `AppState.plugin_runtime_owner: Option<Runtime>`
            // drops inside `#[tokio::main]`'s active runtime context and
            // tokio panics: "Cannot drop a runtime in a context where
            // blocking is not allowed". See `Runtime::shutdown` for why
            // `shutdown_background` is the right call here.
            if let Some(rt) = app_state.take_plugin_runtime() {
                rt.shutdown();
            }

            tui_result
        }

        // diff-review: interactive Code Review surface for a repo path (owns the
        // alternate screen, so it is handled inline rather than via the registry).
        Some(("diff-review", sub)) => {
            entered_tui = true;
            let path = sub
                .get_one::<String>("path")
                .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
            cli::diff_review::run(path)
        }

        // Every other subcommand routes through the registry.
        Some((name, sub)) => registry.dispatch(name, sub, ctx).await,
    };

    // Only clean up terminal if we entered TUI mode. For CLI commands, calling
    // cleanup_terminal() would emit terminal escape sequences into the user's
    // shell, corrupting agent-captured output.
    if result.is_err() && entered_tui {
        cleanup_terminal();
    }

    result
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
    // Event-poll cadence: how often we wake up to check for a keystroke
    // or paste event. Drives the "time-to-first-response" for any input
    // the user generates — including keystrokes routed to plugin
    // screens via `App::tick_plugin_renders` and its `take_render_dirty`
    // gate. Set to ~30 fps so a keystroke lands in the next iter
    // (< 33 ms) rather than the next 250 ms window.
    let tick_rate = Duration::from_millis(33);
    // App-tick cadence: how often we run the heavy host-side periodic
    // work (mascot animation, OAuth refresh check, tmux preview
    // capture, async action dispatch, log streaming refresh,
    // workspace/skills load checks). These are coarse-grained and
    // expensive — running them every 33 ms would starve the event
    // loop, stall key processing, and burn CPU. 250 ms is the
    // pre-perf-PR cadence; keeping it isolates the tick-rate cut
    // to only the event-poll path that actually affects perceived
    // latency. See `last_app_tick` below.
    let app_tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();
    let mut last_app_tick = Instant::now();

    // Startup guard: Ignore key events for the first 100ms to prevent stray keypresses
    // from triggering actions (e.g., buffered 'n' key opening New Session dialog)
    let startup_time = Instant::now();
    const STARTUP_GUARD_MS: u64 = 100;

    let mut slash_palette = SlashPalette::new(SlashCommandRegistry::built_ins());

    loop {
        // Drive plugin-owned screens before every paint. Pushes any
        // host-side state into each plugin and drains its painted
        // WireBuffer into `state.pending_plugin_renders`, so layout's
        // `PluginScreen` can paint without touching the plugin host
        // directly.
        app.tick_plugin_renders();

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

                    use crossterm::event::KeyCode;

                    // Slash-command palette: `:` opens it; while open, all
                    // keypresses go to the palette. Plugin-contributed slash
                    // commands hook in here in Phase 4.
                    //
                    // Don't let the palette steal `:` when the key belongs
                    // to someone else's text input. Two cases:
                    //  - A focused plugin screen owns every non-reserved key
                    //    (the `forward_key_to_focused_plugin` contract);
                    //    witr addresses targets as `port:5432` / `pid:4242`
                    //    / `file:/x` / `container:abc`, all needing a
                    //    literal `:`.
                    //  - Host text-input contexts (new-session URL/prompt
                    //    fields, filter prompts) must keep `:` too — e.g.
                    //    typing an `ssh://...` URL.
                    // An already-open palette still consumes keys, so it can
                    // always be closed.
                    let colon = matches!(key_event.code, KeyCode::Char(':'));
                    let palette_open_suppressed = colon
                        && !slash_palette.is_open()
                        && (crate::app::screens::builtin::plugin_id_for_screen(
                            &app.state.current_screen,
                        )
                        .is_some()
                            || crate::app::events::EventHandler::is_in_text_input_context(
                                &app.state,
                            ));
                    if !palette_open_suppressed && (slash_palette.is_open() || colon) {
                        match slash_palette.handle_key(key_event) {
                            SlashAction::Execute(cmd) => {
                                tracing::info!(
                                    "slash command requested (stub, no dispatch yet): /{}",
                                    cmd
                                );
                            }
                            SlashAction::Opened | SlashAction::Closed | SlashAction::None => {}
                        }
                        continue;
                    }

                    // Intercept keys when tmux preview is in scroll mode
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
                                        last_app_tick = Instant::now();
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
                            if app.state.current_screen == crate::app::screens::ids::LOG_HISTORY {
                                // Log history viewer takes full screen, starts at (0, 0)
                                app.state.log_history_state.handle_click(col, row, 0, 0);
                            } else if app.state.current_screen == crate::app::screens::ids::GIT_VIEW
                                && app.state.git_view_state.as_ref().is_some_and(|g| {
                                    g.active_tab == crate::components::git_view::GitTab::Review
                                })
                            {
                                // Code Review sidebar: click a file/folder row to select/toggle.
                                if let Some(ref mut git_state) = app.state.git_view_state {
                                    git_state.review_sidebar_click(col, row);
                                }
                            } else if let Some(app_event) = EventHandler::handle_mouse_event(
                                AppEvent::MouseClick { x: col, y: row },
                                &mut app.state,
                            ) {
                                EventHandler::process_event(app_event, &mut app.state);
                            }
                        }
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            // Handle mouse scroll based on current view
                            use crate::app::screens::ids as screen_ids;
                            const SCROLL_LINES: usize = 3; // Lines per mouse wheel tick
                            let is_down = matches!(mouse_event.kind, MouseEventKind::ScrollDown);

                            if app.state.current_screen == screen_ids::HOME {
                                // Scroll welcome panel on home screen (right side only)
                                let sidebar_width = app
                                    .state
                                    .home_screen_v2_state
                                    .rendered_sidebar_width()
                                    .unwrap_or_else(|| {
                                        app.state.home_screen_v2_state.sidebar.effective_width(
                                            crossterm::terminal::size().unwrap_or((80, 24)).0,
                                        )
                                    });
                                if mouse_event.column >= sidebar_width {
                                    for _ in 0..SCROLL_LINES {
                                        if is_down {
                                            app.state.home_screen_v2_state.welcome.scroll_down();
                                        } else {
                                            app.state.home_screen_v2_state.welcome.scroll_up();
                                        }
                                    }
                                }
                            } else if app.state.current_screen == screen_ids::GIT_VIEW {
                                // Scroll git view content (markdown or diff)
                                if let Some(ref mut git_state) = app.state.git_view_state {
                                    match git_state.active_tab {
                                        crate::components::git_view::GitTab::Review => {
                                            if is_down {
                                                git_state.review_scroll_down(SCROLL_LINES);
                                            } else {
                                                git_state.review_scroll_up(SCROLL_LINES);
                                            }
                                        }
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
                            } else if app.state.current_screen == screen_ids::LOG_HISTORY {
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
                            } else if app.state.current_screen == screen_ids::SESSION_LIST
                                && app.state.scroll_session_list_by_mouse(
                                    mouse_event.column,
                                    mouse_event.row,
                                    is_down,
                                    SCROLL_LINES,
                                )
                            {
                                // Session-list scrolling was handled in-memory.
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
                            if app.state.current_screen == crate::app::screens::ids::LOG_HISTORY {
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
                            if app.state.current_screen == crate::app::screens::ids::LOG_HISTORY {
                                app.state.log_history_state.end_selection();
                            } else if let Some(app_event) = EventHandler::handle_mouse_event(
                                AppEvent::MouseDragEnd { x: col, y: row },
                                &mut app.state,
                            ) {
                                EventHandler::process_event(app_event, &mut app.state);
                            }
                        }
                        MouseEventKind::Moved => {
                            let (col, row) = (mouse_event.column, mouse_event.row);
                            if let Some(app_event) = EventHandler::handle_mouse_event(
                                AppEvent::MouseMove { x: col, y: row },
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

        // Update last_tick on every iteration so the event-poll timeout
        // stays accurate. The heavy work below is gated on a SEPARATE
        // `last_app_tick` so it runs at app_tick_rate (250 ms) even
        // though the poll cadence is much faster (33 ms).
        last_tick = Instant::now();

        if last_app_tick.elapsed() >= app_tick_rate {
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

                    AsyncAction::AttachWitr => {
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
                                let mut attach_handler =
                                    AttachHandler::new_from_terminal(terminal)?;
                                if let Err(e) = attach_handler.attach_to_session(WITR_SESSION).await
                                {
                                    error!("[ACTION] witr attach failed: {}", e);
                                    app.state.add_error_notification(format!(
                                        "Failed to open the witr browser: {}",
                                        e
                                    ));
                                }
                            }
                            Ok(s) => {
                                error!(
                                    "[ACTION] failed to create witr tmux session (exit {:?})",
                                    s.code()
                                );
                                app.state.add_error_notification(
                                    "Could not start the witr browser — is `witr` installed and on PATH?"
                                        .to_string(),
                                );
                            }
                            Err(e) => {
                                error!("[ACTION] tmux new-session for witr errored: {}", e);
                                app.state.add_error_notification(format!(
                                    "Failed to open the witr browser: {}",
                                    e
                                ));
                            }
                        }
                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::AttachAbtop => {
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
                                let mut attach_handler =
                                    AttachHandler::new_from_terminal(terminal)?;
                                if let Err(e) =
                                    attach_handler.attach_to_session(ABTOP_SESSION).await
                                {
                                    error!("[ACTION] abtop attach failed: {}", e);
                                    app.state.add_error_notification(format!(
                                        "Failed to open abtop: {}",
                                        e
                                    ));
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
                                app.state.add_error_notification(format!(
                                    "Failed to open abtop: {}",
                                    e
                                ));
                            }
                        }
                        app.state.ui_needs_refresh = true;
                    }

                    AsyncAction::SetupAbtopRateLimits => {
                        use tokio::process::Command;

                        info!("[ACTION] Running abtop --setup (rate-limit StatusLine hook)");
                        // `abtop --setup` writes a StatusLine hook into
                        // ~/.claude/settings.json. Run it in its OWN detached
                        // tmux pane so it gets a real TTY (abtop's CLI paths
                        // expect one) without disturbing ainb's alternate
                        // screen. We don't attach — it completes on its own.
                        let setup = Command::new("tmux")
                            .args([
                                "new-session",
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
                                    "Enabling abtop rate-limit tracking (abtop --setup)…"
                                        .to_string(),
                                );
                            }
                            _ => {
                                app.state.add_error_notification(
                                    "Could not run `abtop --setup` — is `abtop` on PATH? You can run it manually."
                                        .to_string(),
                                );
                            }
                        }
                        // Open abtop regardless of the setup outcome.
                        app.state.pending_async_action = Some(AsyncAction::AttachAbtop);
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

                    AsyncAction::KillOtherTmuxSessions(session_names) => {
                        use tokio::process::Command;

                        let total = session_names.len();
                        let mut killed = 0usize;
                        let mut failed = 0usize;
                        let selected_name = app
                            .state
                            .selected_other_tmux_session()
                            .map(|session| session.name.clone());

                        for session_name in &session_names {
                            info!("Killing other tmux session '{}'", session_name);

                            let output = Command::new("tmux")
                                .args(["kill-session", "-t", session_name])
                                .output()
                                .await;

                            match output {
                                Ok(o) if o.status.success() => {
                                    info!("Successfully killed tmux session '{}'", session_name);
                                    killed += 1;
                                }
                                Ok(o) => {
                                    let stderr = String::from_utf8_lossy(&o.stderr);
                                    warn!(
                                        "Failed to kill tmux session '{}': {}",
                                        session_name, stderr
                                    );
                                    failed += 1;
                                }
                                Err(e) => {
                                    warn!("Failed to kill tmux session '{}': {}", session_name, e);
                                    failed += 1;
                                }
                            }
                        }

                        if let Some(selected_name) = selected_name {
                            if session_names.iter().any(|name| name == &selected_name) {
                                app.state.selected_other_tmux_index = None;
                            }
                        }

                        if failed > 0 {
                            app.state.add_warning_notification(format!(
                                "Killed {}/{} tmux session(s) ({} failed)",
                                killed, total, failed
                            ));
                        } else {
                            app.state.add_success_notification(format!(
                                "Killed {} tmux session(s)",
                                killed
                            ));
                        }

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
                    last_app_tick = Instant::now();

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
                    last_app_tick = Instant::now();
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

    // Short-lived CLI subcommands (one-shot utilities, the statusline
    // hook that fires on every Claude Code prompt render, completion
    // generation, `--help`, etc.) must NOT open a JSONL file — it'd
    // litter `~/.agents-in-a-box/logs/` with one empty file per
    // invocation. For these commands, install a stderr-only subscriber
    // so explicit warns/errors still surface when invoked synchronously
    // from a shell. Long-running commands (TUI, `run`, `attach`,
    // `auth`, `recover`) fall through to the JSONL file path.
    let first_arg = std::env::args().nth(1);
    let is_short_lived_cli = matches!(
        first_arg.as_deref(),
        Some(
            "list"
                | "logs"
                | "status"
                | "kill"
                | "config"
                | "git"
                | "favorites"
                | "init"
                | "presets"
                | "usage"
                | "claudecode"
                | "statusline"
                | "completion"
                | "--help"
                | "-h"
                | "--version"
                | "-V"
                | "help"
        )
    );
    if is_short_lived_cli {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_ansi(false)
                    .compact(),
            )
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "ainb=warn".into()),
            )
            .init();
        return;
    }

    // Create log directory if it doesn't exist
    let log_dir = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".agents-in-a-box").join("logs"))
        .unwrap_or_else(|_| PathBuf::from(".agents-in-a-box/logs"));

    let _ = std::fs::create_dir_all(&log_dir);

    // Best-effort janitor: every TUI startup, prune zero-byte JSONL
    // files older than 24h. The bug that produced 2,999 empty files
    // (filter mismatch + per-invocation file open) is fixed, but old
    // installs accumulate cruft and even now a crash before the first
    // log event leaves an empty file behind.
    purge_empty_log_files(&log_dir);

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
            // Comprehensive default: `ainb`/`ainb_core` (this crate and
            // its lib) at info so every traced event from our own code
            // lands in the JSONL, plugin runtime + first-party plugins
            // at debug for visibility, and global `warn` so noisy
            // dependencies (bollard, hyper, tokio, etc.) only surface
            // real problems. Override at any time with `RUST_LOG`.
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,ainb=info,ainb_core=info,ainb_plugin_runtime=debug,\
                 ainb_plugin_session_reader=debug,ainb_plugin_burndown=debug"
                    .into()
            }),
        )
        .init();
}

/// Remove zero-byte `agents-in-a-box-*.jsonl` files older than 24h.
/// Best-effort — any IO error is silently swallowed so log
/// initialisation always proceeds.
fn purge_empty_log_files(log_dir: &std::path::Path) {
    use std::time::{Duration, SystemTime};

    const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() != 0 {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("agents-in-a-box-") || !name.ends_with(".jsonl") {
            continue;
        }
        let age = meta.modified().ok().and_then(|m| now.duration_since(m).ok());
        if age.is_some_and(|a| a >= STALE_AFTER) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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
