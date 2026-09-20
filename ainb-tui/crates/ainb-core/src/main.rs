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
use clap::{FromArgMatches, Subcommand};
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

// The binary links the `ainb` library instead of compiling the module tree a
// second time, so every module has one home and one set of visibility rules.
use ainb::{app, cli, components, config, fleet, headroom, perf, plugins, tmux};

use ainb::App;
use app::keymap::{KeyAction, KeyContext, Keymap, ScrollAction, UiAction};
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
) -> Result<()>
where
    <B as ratatui::backend::Backend>::Error: std::error::Error + Send + Sync + 'static,
{
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

/// argv[1] values that route to the `ainb-cli` crate before tokio
/// spins up. These commands are pure CLI (no Docker, no tmux, no
/// alt-screen) so they short-circuit the TUI bootstrap entirely.
/// Keep in sync with the safety-tag's CLI_COMMANDS list — adding a
/// new top-level command means adding it here AND wiring an arm in
/// ainb-cli's Command enum.
const SKILL_MANAGER_CLI_COMMANDS: &[&str] = &["source", "search", "skill"];

fn is_skill_manager_cli_invocation() -> bool {
    std::env::args()
        .nth(1)
        .is_some_and(|arg| SKILL_MANAGER_CLI_COMMANDS.contains(&arg.as_str()))
}

fn main() -> Result<()> {
    // BEFORE the `ainb-cli` short-circuit and before any runtime exists.
    //
    // Two reasons, both bugs when this lived in `tokio_main`:
    //
    // 1. `skill` / `source` / `search` return through `ainb_cli::run()` above
    //    without ever entering `tokio_main`, and `ainb-cli` does not depend on
    //    this crate. So `general.skill_install_real_homes` never reached
    //    `ainb skill install`, the one command it exists to govern.
    // 2. `export_env_bridge` calls `std::env::set_var`. Inside `#[tokio::main]`
    //    the worker threads and the tracing subscriber already exist, and a
    //    `setenv` racing another thread's `getenv` is a data race (which is why
    //    Rust 2024 made it `unsafe`). Here, `main` is still the only thread.
    //
    // `migrate_legacy_paths` moves with it rather than staying behind: it folds
    // a stray older config.toml into the canonical one, and the bridge has to
    // read the merged result, not the pre-migration file.
    // Skipped for the pure-informational invocations. Otherwise `ainb --help`
    // and `ainb --version` would each do a four-path config load and could
    // WRITE config.toml through the legacy-path migration — surprising for a
    // command that is meant to print and exit, and any warning raised during
    // that load is discarded because the tracing subscriber does not exist yet.
    //
    // `!args.is_empty()` matters: `all()` on an EMPTY iterator is `true`, so
    // bare `ainb` — the normal way to open the TUI — would classify itself as
    // informational and skip both the migration and the bridge, leaving every
    // promoted key inert in the one invocation that most needs them.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let informational_only = !args.is_empty()
        && args
            .iter()
            .all(|arg| matches!(arg.as_str(), "-h" | "--help" | "-V" | "--version" | "help"));
    if !informational_only {
        config::AppConfig::migrate_legacy_paths();
        config::tunables::export_env_bridge(&config::tunables::snapshot());
    }

    if is_skill_manager_cli_invocation() {
        return ainb_cli::run();
    }
    tokio_main()
}

#[tokio::main]
async fn tokio_main() -> Result<()> {
    crate::perf::init();
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
            )
            .after_help(
                "EXAMPLES:\n  \
                 ainb diff-review                 Review uncommitted changes in the current repo\n  \
                 ainb diff-review ~/code/proj     Review a specific repo\n  \
                 ainb diff-review --format json   Emit the structured diff as JSON (headless)",
            ),
    );
    app = app.subcommand(
        <cli::keymap::KeymapCommands as Subcommand>::augment_subcommands(
            clap::Command::new("keymap").subcommand_required(true),
        )
        .about("List effective terminal shortcuts")
        .after_help(
            "EXAMPLES:\n  \\
             ainb keymap list\n  \\
             ainb keymap list --format json",
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

            // #963: the TUI's one presence connection, held from here until
            // quit on every screen, plugins or not. It is how
            // `hangar connections list` knows a TUI is running; it dials once
            // the daemon is reachable and reconnects after a daemon restart.
            // Spawned before the first daemon call below, and the process is
            // marked a surface for good, so no call made during startup or
            // teardown lists as a second row.
            let presence = spawn_tui_presence();
            // Section 20 (T0-section, #1015): one joined daemon read per Fleet
            // revision, folded into the app state by its reducer. Held for the
            // TUI's lifetime; dropping it stops the task. It is the process's
            // only agent-status reader: the Fleet panel renders what it
            // publishes (#1031), and `[fleet.status] legacy_panel` is honoured
            // here rather than in the plugin.
            let mut agent_status = ainb::agent_status_host::AgentStatusHost::spawn(
                Box::new(fleet::bridge::daemon::tui_client),
                config::tunables::legacy_panel(),
            );
            // Section 16 (D3-prime): read for the TUI's whole life, slowly
            // while the inbox screen is closed (the legend's unread badge)
            // and at the normal cadence while it is open.
            let mut inbox_host =
                ainb::inbox_host::InboxHost::new(Box::new(fleet::bridge::daemon::tui_client));

            // A plugin-disabled TUI is a diagnostic fallback with no Hangar
            // consumer. Do not leave a background daemon behind for it.
            if !plugins::plugins_disabled() {
                // Best-effort: bring the Hangar daemon up before the TUI connects, so
                // a fresh home shows a runtime + a seeded agent (the boot seed runs in
                // the daemon) instead of an offline panel. Idempotent + non-fatal — a
                // spawn failure is logged and the TUI still launches.
                // Persistent: the TUI is the daemon's consumer and outlives it.
                cli::hangar::ensure_hangar_daemon(cli::hangar::LauncherLifetime::Persistent);
                // Same contract for the rest: clear heartbeats left by daemons
                // that are already gone, and hand a drifted notifyd to this
                // binary. Without it an upgraded ainb kept talking to the
                // pre-upgrade notifyd until someone restarted it by hand.
                fleet::daemons::ensure_daemons_current();
                if let Err(error) = cli::update::ensure_schedule() {
                    tracing::warn!(error = %error, "failed to install daily release checker");
                }
            }

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
            if let Some(state) = cli::update::cached_state() {
                if let Some(version) = state.available_version {
                    app_state.state.add_info_notification(format!(
                        "ainb {version} available. Run `ainb update --yes`, then restart."
                    ));
                }
            }

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
            app_state.state.shell.pending_async_action = None;

            // Flush any pending terminal events to prevent stray keypresses
            // from interfering with onboarding or initial view
            while crossterm::event::poll(std::time::Duration::from_millis(10)).unwrap_or(false) {
                let _ = crossterm::event::read();
            }

            // Lease the shared Headroom proxy for this TUI's lifetime, so a
            // second TUI quitting does not stop a proxy this one still uses.
            if let Err(e) = headroom::register_user() {
                tracing::warn!("could not register as a headroom proxy user: {e}");
            }

            let tui_result = run_tui(
                &mut app_state,
                &mut layout,
                &mut agent_status,
                &mut inbox_host,
            )
            .await;
            drop(inbox_host);
            drop(agent_status);

            // Explicitly tear down the plugin runtime before `app_state`
            // drops. Without this, `AppState.plugin_runtime_owner: Option<Runtime>`
            // drops inside `#[tokio::main]`'s active runtime context and
            // tokio panics: "Cannot drop a runtime in a context where
            // blocking is not allowed". See `Runtime::shutdown` for why
            // `shutdown_background` is the right call here.
            if let Some(rt) = app_state.take_plugin_runtime() {
                rt.shutdown();
            }

            // Best-effort: stop the shared Headroom proxy so it does not
            // orphan after the TUI exits, unless another live TUI still holds
            // a lease on it.
            headroom::release_user_and_stop_if_unused();

            // Last, so the row is listed for as long as this TUI can still act.
            presence.close().await;

            tui_result
        }

        // diff-review: interactive Code Review surface for a repo path (owns the
        // alternate screen, so it is handled inline rather than via the registry).
        Some(("diff-review", sub)) => {
            let path = sub
                .get_one::<String>("path")
                .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
            // `--format text` (default) opens the interactive surface; any
            // machine format emits the diff as JSON headless.
            if matches!(format, cli::OutputFormat::Text) {
                entered_tui = true;
                cli::diff_review::run(path)
            } else {
                cli::diff_review::run_headless(path)
            }
        }

        Some(("keymap", sub)) => {
            let command = cli::keymap::KeymapCommands::from_arg_matches(sub)?;
            cli::keymap::execute(command, format)
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

/// Hold this TUI's presence row in the daemon's connection registry.
///
/// The lease owns the connection lifecycle; this only names the surface and
/// logs transitions, never toasts: a TUI with no daemon is a normal state.
fn spawn_tui_presence() -> fleet::bridge::daemon::PresenceLease {
    use ainb_hangar_proto::connections::{SurfaceInfo, SurfaceKind};
    use fleet::bridge::daemon::{PresenceLease, PresenceState, mark_process_as_surface};

    mark_process_as_surface();
    let lease = PresenceLease::spawn(SurfaceInfo {
        kind: SurfaceKind::Tui,
        pid: std::process::id(),
    });
    let mut state = lease.state();
    // Panic-free on purpose: the global panic handler tears the terminal down,
    // so nothing here unwraps, and the lease task's own failures arrive as
    // states, not panics.
    tokio::spawn(async move {
        while state.changed().await.is_ok() {
            match &*state.borrow_and_update() {
                PresenceState::Connected => {
                    tracing::info!("tui presence connected to hangar daemon")
                }
                PresenceState::Waiting { error } => {
                    tracing::debug!(error = ?error, "tui presence waiting for hangar daemon");
                }
                PresenceState::Closed => tracing::debug!("tui presence closed"),
            }
        }
        // The sender is gone. Only `close()` publishes `Closed` first; any other
        // end (the task panicked or was aborted) leaves this TUI unlisted in
        // `hangar connections list` until restart, so say why.
        let last = state.borrow().clone();
        match last {
            PresenceState::Closed => {}
            PresenceState::Connected => tracing::warn!(
                "tui presence task ended without close while connected (panicked or \
                 aborted); this TUI is no longer listed in hangar connections"
            ),
            PresenceState::Waiting { error } => tracing::warn!(
                last_error = ?error,
                "tui presence task ended without close while waiting for the daemon \
                 (panicked or aborted); this TUI will not be listed in hangar connections"
            ),
        }
    });
    lease
}

async fn run_tui(
    app: &mut App,
    layout: &mut LayoutComponent,
    agent_status: &mut ainb::agent_status_host::AgentStatusHost,
    inbox_host: &mut ainb::inbox_host::InboxHost,
) -> Result<()> {
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
    let result = run_tui_loop(app, layout, &mut terminal, agent_status, inbox_host).await;

    // Always clean up terminal using unified cleanup
    if let Err(e) = cleanup_terminal_with_instance(&mut terminal) {
        tracing::error!("Failed to cleanup terminal: {}", e);
        // Fallback to basic cleanup
        cleanup_terminal();
    }

    // P6e: a queued session-store write outlives the loop, so it is waited for
    // before the process goes. Quitting mid-write would drop the operator's
    // last change. After the terminal is back, never before: a wait inside the
    // alternate screen is a frozen screen to the operator. Bounded, and for
    // the whole queue, because a write can be sitting on a daemon that stopped
    // answering; what the bound leaves behind is said on the real stderr.
    let dropped =
        ainb::effect_host::finish_session_store_writes(ainb::cli::util::SESSION_STORE_FLUSH_BOUND);
    if dropped > 0 {
        tracing::warn!(dropped, "session-store writes were still queued at exit");
        eprintln!(
            "Warning: {dropped} session change(s) were not written: the session store did not answer in time."
        );
    }

    // Perf trace summary (no-op unless AINB_PERF_TRACE is set). Emitted after
    // the alternate screen is torn down so the report lands on the real stderr.
    crate::perf::report();

    result
}

async fn run_tui_loop(
    app: &mut App,
    layout: &mut LayoutComponent,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    agent_status: &mut ainb::agent_status_host::AgentStatusHost,
    inbox_host: &mut ainb::inbox_host::InboxHost,
) -> Result<()> {
    // The ratatui host's own state: geometry, scroll offsets, hover and panel
    // widths. Lives here, beside the `LayoutComponent`, because
    // none of it survives this process or crosses to another surface.
    let mut ui = crate::app::ui_state::UiState::default();
    ui.restore(&app.state.config.app_config);
    // The live tmux client behind the session list's preview pane. The reducer
    // names the session; this owns the PTY.
    let mut clients = ainb::terminal_clients::TerminalClients::default();

    let (keymap, keymap_warning) = Keymap::load_user();
    if let Some(warning) = keymap_warning {
        app.state.add_warning_notification(warning);
    }
    // Layout widths saved as column counts by an older ainb become fractions
    // of this terminal, the surface they were last sized on.
    let columns = terminal.size().map_or(80, |size| size.width);
    // The status bar draws from sections the tick refreshes; fill them now so
    // the frames before the first tick show the statusline CTA, not a gap.
    app.state.refresh_statusline();
    run_intent(
        ainb::app::reports::migrate_layout_widths(columns),
        app,
        &keymap,
        &mut ui,
        terminal,
        &mut clients,
    )
    .await?;
    // Event-poll cadence: how often we wake up to check for a keystroke
    // or paste event. Drives the "time-to-first-response" for any input
    // the user generates — including keystrokes routed to plugin
    // screens via `App::tick_plugin_renders` and its `take_render_dirty`
    // gate. Set to ~30 fps so a keystroke lands in the next iter
    // (< 33 ms) rather than the next 250 ms window.
    // `.max(1)`: the registry's `min` only gates the settings screen and
    // `ainb config set`. A hand-edited `tick_rate_ms = 0` gives a zero poll
    // timeout, so the loop never blocks and repaints continuously at 100% CPU.
    let tick_rate =
        Duration::from_millis(crate::config::tunables::snapshot().ui.tick_rate_ms.max(1));
    // App-tick cadence: how often we run the heavy host-side periodic
    // work (mascot animation, OAuth refresh check, tmux preview
    // capture, async action dispatch, log streaming refresh,
    // workspace/skills load checks). These are coarse-grained and
    // expensive — running them every 33 ms would starve the event
    // loop, stall key processing, and burn CPU. 250 ms is the
    // pre-perf-PR cadence; keeping it isolates the tick-rate cut
    // to only the event-poll path that actually affects perceived
    // latency. See `last_app_tick` below. Both cadences are
    // `[ui]` keys now: a slow terminal wants a coarser poll.
    let app_tick_rate =
        Duration::from_millis(crate::config::tunables::snapshot().ui.app_tick_ms.max(1))
            .max(tick_rate);
    let mut last_tick = Instant::now();
    let mut last_app_tick = Instant::now();

    // Perf trace: timestamp of the most recent keystroke awaiting its paint.
    // Set when a key event is read; consumed at the top of the next loop
    // iteration once the paint that reflects it has completed. Gated behind
    // `AINB_PERF_TRACE` (see `crate::perf`); zero cost when disabled.
    let mut pending_key_at: Option<Instant> = None;

    // Dirty-gate for the host layout repaint (perf: bead `wai`). The TUI used
    // to call `terminal.draw()` unconditionally every 33 ms (~30 fps) even when
    // nothing changed, burning ~5-6% CPU at idle. Animations and periodic state
    // already advance only on the 250 ms `app_tick`, so a frame between ticks
    // was an identical repaint. We now paint only when something actually
    // changed: an input/resize/paste event, a fresh plugin frame, an
    // `app_tick` (covers mascot/spinner/state at their existing 250 ms
    // cadence), or an explicit `ui_needs_refresh`. Starts `true` for the first
    // paint. Worst-case staleness is one `app_tick` (250 ms) — identical to the
    // pre-existing animation cadence — so there is no visible regression.
    let mut needs_redraw = true;

    // Startup guard: Ignore key events for the first 100ms to prevent stray keypresses
    // from triggering actions (e.g., buffered 'n' key opening New Session dialog)
    let startup_time = Instant::now();
    const STARTUP_GUARD_MS: u64 = 100;

    let mut slash_palette = SlashPalette::new(SlashCommandRegistry::built_ins());
    // Events read while re-joining a split paste that were not part of it (a
    // resize, a mouse event), handled on the next passes in order instead of
    // lost.
    let mut replayed: std::collections::VecDeque<Event> = std::collections::VecDeque::new();

    loop {
        // Effects still on the outbox. dispatch, tick and apply_pending_event
        // hand over what they queue, so one path leaves effects here: a tick
        // that fails part way returns its error before handing over what it
        // already queued. They run after the step that queued them finished
        // writing state, and before the frame that shows their result.
        let leftover = app.state.take_effects();
        if !leftover.is_empty() {
            run_effects(leftover, app, &keymap, &mut ui, terminal, &mut clients).await?;
            needs_redraw = true;
        }
        // Reports from background work (a daemon verb) that finished since the
        // last iteration.
        for report in ainb::effect_host::take_deferred_reports() {
            run_intent(report, app, &keymap, &mut ui, terminal, &mut clients).await?;
            needs_redraw = true;
        }
        // Section 20 updates from the agent-status host task. The TUI paints
        // Fleet through the plugin, so a section change is not a repaint here:
        // it is published to the plugins, which fold it into the panel (#1031),
        // and its version is what a mirrored surface subscribes to.
        agent_status.drain_into(&mut app.state);
        agent_status.publish(&app.state, app.plugin_runtime());
        // Section 16: read for the TUI's whole life, slowly while the inbox
        // screen is closed and at the normal cadence while it is open; the
        // legend's badge and the screen both draw from it, so a fold repaints.
        if inbox_host.tick(&mut app.state) {
            needs_redraw = true;
        }

        // Drive plugin-owned screens before every paint. Pushes any
        // host-side state into each plugin and drains its painted
        // WireBuffer into `state.plugins_host.pending_plugin_renders`, so layout's
        // `PluginScreen` can paint without touching the plugin host
        // directly.
        // A fresh plugin frame is a reason to repaint even if nothing else
        // changed (e.g. a self-animating plugin screen).
        if app.tick_plugin_renders(&mut ui.plugin_viewports) {
            needs_redraw = true;
        }

        // A client that ended on its own (detach, session gone, EOF) is
        // reported, so the pane reverts to the read-only preview rather than a
        // dead screen. Releasing changes the layout, so it is a repaint
        // trigger, and so is the pane leaving a screen that no longer shows it.
        if let Some(report) = clients.take_exited() {
            run_intent(report, app, &keymap, &mut ui, terminal, &mut clients).await?;
            needs_redraw = true;
        }
        if app.state.tick_terminal_pane() {
            needs_redraw = true;
        }
        // The own-session rule reads the session this host runs in from state;
        // looking it up is the host's job. The lookup caches a hit and paces a
        // miss, so asking every pass is cheap.
        app.state.set_host_tmux_session(
            crate::tmux::process_detection::host_tmux_session_name().map(str::to_string),
        );

        // Read-only preview is a real tmux client feeding the same vt100
        // parser used after input focus is granted. Keep it aligned with the
        // current selection before painting: the reducer says when to open
        // one, and the effect opens it at the pane's size.
        if app.state.shell.current_screen == crate::app::screens::ids::SESSION_LIST
            && !app.state.is_interactive_pane()
        {
            if let Some(effect) = app.state.request_terminal_observer() {
                run_effects(vec![effect], app, &keymap, &mut ui, terminal, &mut clients).await?;
                needs_redraw = true;
            }
        }
        // The client follows the session state names: one the reducer
        // released or declined closes here, before the frame that would show it.
        if clients.reconcile(&app.state) {
            needs_redraw = true;
        }

        // Live embed output is the third repaint source alongside input and
        // plugin frames: the PTY reader thread marks the embed dirty as bytes
        // stream in, with no host input involved. Without this the dirty-gate
        // would hold the live pane at the 250ms app-tick floor.
        if clients.take_dirty() {
            needs_redraw = true;
        }

        // A conversation is the fourth repaint source, and for the same reason
        // as the embed: its content arrives from the daemon with no host input
        // involved. The chat polls on the frame tick, so under the dirty-gate
        // an open conversation would fetch once, at open, and then sit still
        // while the copilot answered into a socket nobody read. The chat's own
        // in-flight latch keeps that to one fetch per interval, not one per
        // frame.
        if app.state.session_chat_open() {
            needs_redraw = true;
        }

        needs_redraw |= std::mem::take(&mut ui.needs_redraw);

        if needs_redraw {
            // Everything the draw path used to tick inside `terminal.draw`.
            // Under the same dirty gate the draw is, so these keep the cadence
            // they had when they lived in `render`.
            layout.tick_before_draw(&mut app.state);
            layout.tmux_preview_mut().show_terminal(clients.screen());
            let draw_start = Instant::now();
            match terminal.draw(|frame| {
                layout.render(frame, &app.state, &mut ui);
            }) {
                Ok(_) => {
                    crate::perf::record_draw(draw_start.elapsed());
                    // This paint is the first one to reflect any key read in the
                    // previous iteration, so it marks the end of the key-to-render
                    // interval.
                    if let Some(key_at) = pending_key_at.take() {
                        crate::perf::record_key_to_render(key_at.elapsed());
                    }
                    // The embed resize and three pane rects could only be
                    // measured by the frame that just went out.
                    crate::components::layout::publish_after_draw(&mut app.state, &mut ui);
                    crate::components::layout::resize_terminal_client(&mut ui, &mut clients);
                    needs_redraw = false;
                }
                // Transient frame-write failure (e.g. EINTR over a flaky SSH
                // link) — keep the TUI alive and repaint next iteration instead
                // of tearing the whole session down. Only genuinely fatal I/O
                // errors propagate.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    needs_redraw = true;
                }
                Err(e) => return Err(e.into()),
            }
        }

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        // Tolerate transient terminal-read failures: EINTR (e.g. SIGWINCH on
        // resize, common over SSH) must not crash the session. Only fatal I/O
        // errors propagate.
        let has_event = !replayed.is_empty()
            || match crossterm::event::poll(timeout) {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => false,
                Err(e) => return Err(e.into()),
            };
        if has_event {
            // Any input (key/mouse/paste/resize) warrants a repaint on the next
            // loop iteration (perf: bead `wai` dirty-gate).
            needs_redraw = true;
            let read_event = match replayed.pop_front().map_or_else(event::read, Ok) {
                Ok(ev) => ev,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            match read_event {
                Event::Key(key_event) => {
                    // Windows fires Press + Release for every key; macOS/Linux fire only Press.
                    // Drop Release so Enter doesn't immediately re-trigger and close popups.
                    if key_event.kind == KeyEventKind::Release {
                        continue;
                    }

                    // Perf trace: this real keystroke now awaits the next paint.
                    if crate::perf::enabled() {
                        crate::perf::record_key();
                        pending_key_at = Some(Instant::now());
                    }

                    // Startup guard: Ignore key events during startup period
                    if startup_time.elapsed() < Duration::from_millis(STARTUP_GUARD_MS) {
                        tracing::debug!(
                            "Ignoring key event {:?} during startup guard period",
                            key_event.code
                        );
                        continue;
                    }

                    // Ctrl+Q belongs to the terminal screen. Interactive mode
                    // releases; every other session-list state consumes it so
                    // the host's plain `q` shortcut is never timing-dependent.
                    // `None` for keys the keymap has no spelling for; those still
                    // reach the embed, the palette and plugins below, but never
                    // the host keymap.
                    let chord = crate::app::screens::builtin::chord_from_key_event(&key_event);
                    let interactive_detach =
                        chord.as_ref().is_some_and(|chord| keymap.releases_in_place_pane(chord));
                    if interactive_detach {
                        if app.state.is_interactive_pane() {
                            detach_interactive_pane(app, &keymap, &mut ui, terminal, &mut clients)
                                .await?;
                        }
                        if app.state.shell.current_screen == crate::app::screens::ids::SESSION_LIST
                        {
                            continue;
                        }
                    }

                    // Interactive embed has highest input precedence: while a
                    // session is focused in-place, every key is forwarded to the
                    // embedded tmux client (Ctrl+Q was already intercepted
                    // above). This runs BEFORE the slash palette so typing ':'
                    // inside the embed reaches the PTY instead of opening the
                    // palette.
                    if app.state.is_interactive_pane() {
                        if interactive_detach {
                            detach_interactive_pane(app, &keymap, &mut ui, terminal, &mut clients)
                                .await?;
                            continue;
                        }
                        // write_input only errors when the PTY writer thread is
                        // gone — release immediately instead of leaving a
                        // focused pane that silently eats input.
                        if let Some(report) = crate::tmux::encode_key_event(&key_event)
                            .and_then(|bytes| clients.write_input(&bytes))
                        {
                            run_intent(report, app, &keymap, &mut ui, terminal, &mut clients)
                                .await?;
                        }
                        continue;
                    }

                    // Slash-command palette: `:` opens it; while open, all
                    // keypresses go to the palette. Plugin-contributed slash
                    // commands hook in here in Phase 4.
                    //
                    // Don't let the palette steal `:` when the key belongs
                    // to someone else's text input. Two cases:
                    //  - A focused plugin screen owns every non-reserved key
                    //    (the `route_key_to_focused_plugin` contract);
                    //    witr addresses targets as `port:5432` / `pid:4242`
                    //    / `file:/x` / `container:abc`, all needing a
                    //    literal `:`.
                    //  - Host text-input contexts (new-session URL/prompt
                    //    fields, filter prompts) must keep `:` too — e.g.
                    //    typing an `ssh://...` URL.
                    // An already-open palette still consumes keys, so it can
                    // always be closed.
                    let colon = chord.as_ref().is_some_and(|chord| {
                        matches!(
                            keymap.resolve(&[KeyContext::Global], chord),
                            Some(KeyAction::OpenSlashPalette)
                        )
                    });
                    let palette_open_suppressed = colon
                        && !slash_palette.is_open()
                        && (crate::app::screens::builtin::plugin_id_for_screen(
                            &app.state.shell.current_screen,
                        )
                        .is_some()
                            || crate::app::is_in_text_input_context(&app.state));
                    if !palette_open_suppressed && (slash_palette.is_open() || colon) {
                        match slash_palette.handle_key(key_event) {
                            SlashAction::Execute(cmd) => {
                                // Route host-mapped slash commands (e.g. the
                                // learnings plugin's `/recall` + `/memory`,
                                // wired in P9) to the same keymap command the
                                // global keyboard shortcuts use. Commands with
                                // no host mapping fall through to the log-only
                                // stub (plugin-owned dispatch lands later).
                                if let Some(intent) = crate::app::slash_command_intent(&cmd) {
                                    run_intent(
                                        intent,
                                        app,
                                        &keymap,
                                        &mut ui,
                                        terminal,
                                        &mut clients,
                                    )
                                    .await?;
                                } else {
                                    tracing::info!(
                                        "slash command requested (no host mapping): /{}",
                                        cmd
                                    );
                                }
                            }
                            SlashAction::Opened | SlashAction::Closed | SlashAction::None => {}
                        }
                        continue;
                    }

                    // A read-only terminal uses tmux's own scrollback; host
                    // preview scroll mode would swallow navigation invisibly.
                    let observing_terminal = app.state.is_observing_selected_terminal();
                    match preview_scroll_route(
                        &app.state.shell.current_screen,
                        layout.tmux_preview_mut().is_scroll_mode(),
                        observing_terminal,
                    ) {
                        PreviewScrollRoute::Clear => {
                            ui.apply(ScrollAction::PreviewExitScroll, layout, &app.state);
                        }
                        PreviewScrollRoute::Handle => {
                            match chord.as_ref().and_then(|chord| {
                                keymap.resolve(&[KeyContext::PreviewScroll], chord)
                            }) {
                                // Don't let ESC fall through as Quit, or the
                                // arrows navigate sessions behind the pane.
                                Some(KeyAction::Ui(UiAction::Scroll(
                                    action @ (ScrollAction::PreviewExitScroll
                                    | ScrollAction::PreviewScrollUp
                                    | ScrollAction::PreviewScrollDown
                                    | ScrollAction::PreviewPageUp
                                    | ScrollAction::PreviewPageDown),
                                ))) => {
                                    ui.apply(action, layout, &app.state);
                                    continue;
                                }
                                _ => {} // Let other keys pass through to event handler
                            }
                        }
                        PreviewScrollRoute::Ignore => {}
                    }

                    // Plugin screens own every non-reserved key after the
                    // interactive and slash-palette precedence above. Forward
                    // before host dispatch so navigation cannot consume plugin
                    // input such as Hangar's Ctrl+P palette shortcut.
                    {
                        use crate::app::screens::builtin::{
                            PluginRoute, crossterm_to_protocol_key, plugin_id_for_screen,
                            route_key_to_focused_plugin,
                        };
                        let route = match crossterm_to_protocol_key(&key_event) {
                            Some(key) => route_key_to_focused_plugin(&app.state, &keymap, &key),
                            // A key the plugin wire has no shape for (a media
                            // key): a plugin screen still claims it.
                            None if plugin_id_for_screen(&app.state.shell.current_screen)
                                .is_some() =>
                            {
                                PluginRoute::Consumed
                            }
                            None => PluginRoute::Host,
                        };
                        match route {
                            PluginRoute::Forward(effect) => {
                                run_effects(
                                    vec![effect],
                                    app,
                                    &keymap,
                                    &mut ui,
                                    terminal,
                                    &mut clients,
                                )
                                .await?;
                                continue;
                            }
                            PluginRoute::Consumed => continue,
                            PluginRoute::Host => {}
                        }
                    }

                    let Some(chord) = chord else {
                        continue;
                    };
                    // Confirming a dialog queues its async work (a delete, a
                    // stop). Tick straight away so it starts, and the frame
                    // shows it, without waiting out the app tick.
                    let confirming = app.state.shell.confirmation_dialog.is_some();
                    run_intent(
                        ainb::Intent::Key(chord),
                        app,
                        &keymap,
                        &mut ui,
                        terminal,
                        &mut clients,
                    )
                    .await?;
                    // Layout work the table resolved never reaches the reducer.
                    let columns = terminal.size().map_or(80, |size| size.width);
                    for action in ui.take_queued() {
                        if let Some(save) = ui.apply_host(action, layout, &app.state, columns) {
                            run_intent(save, app, &keymap, &mut ui, terminal, &mut clients).await?;
                        }
                    }
                    if confirming
                        && app.state.shell.confirmation_dialog.is_none()
                        && app.state.shell.pending_async_action.is_some()
                    {
                        use tracing::{error, info};
                        info!(">>> Immediately processing async action for responsive UI");
                        match app.tick().await {
                            Ok(effects) => {
                                info!(">>> Immediate tick completed successfully");
                                run_effects(effects, app, &keymap, &mut ui, terminal, &mut clients)
                                    .await?;
                                last_app_tick = Instant::now();
                                // Force UI refresh. The tick runs here
                                // for the same reason it runs before the
                                // main draw: this frame would otherwise
                                // paint an unreconciled `session_tab`,
                                // skip the Ask pane's retarget, and read
                                // `chat_host` where the tick would have
                                // ticked `chat_host_for`.
                                layout.tick_before_draw(&mut app.state);
                                terminal.draw(|frame| {
                                    layout.render(frame, &app.state, &mut ui);
                                })?;
                                crate::components::layout::publish_after_draw(
                                    &mut app.state,
                                    &mut ui,
                                );
                            }
                            Err(e) => {
                                error!(">>> Error during immediate tick: {}", e);
                            }
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    use crossterm::event::{MouseButton, MouseEventKind};

                    // Mode boundary: while the interactive embed owns input,
                    // host mouse handlers must NEVER run — a click/scroll
                    // changing focus or selection under a live embed splits
                    // the mode invariants. Events inside the embed's interior
                    // are translated + forwarded to the PTY as SGR sequences
                    // (ainb-created sessions run with tmux `mouse on`, so the
                    // wheel scrolls and drag selects inside tmux; sessions
                    // without it ignore the sequences). Everything else is
                    // swallowed.
                    if app.state.is_interactive_pane() {
                        if let Some(report) = ui
                            .embed_pane_area
                            .and_then(|inner| crate::tmux::encode_mouse_event(&mouse_event, inner))
                            .and_then(|bytes| clients.write_input(&bytes))
                        {
                            run_intent(report, app, &keymap, &mut ui, terminal, &mut clients)
                                .await?;
                        }
                        continue;
                    }

                    // A focused plugin screen owns the pointer (mirrors the
                    // key-forwarding contract). Forward + consume before the
                    // host's own mouse handling so the two never double-act.
                    {
                        use crate::app::screens::builtin::{
                            PluginRoute, crossterm_to_protocol_mouse, route_mouse_to_focused_plugin,
                        };
                        let screen = &app.state.shell.current_screen;
                        let origin =
                            ui.plugin_render_origins.get(screen).copied().unwrap_or((0, 0));
                        let (width, height) =
                            ui.plugin_viewports.render_areas.get(screen).copied().unwrap_or((0, 0));
                        let area = ainb_plugin_protocol::params::Viewport::new(width, height);
                        match route_mouse_to_focused_plugin(
                            &app.state,
                            origin,
                            area,
                            &crossterm_to_protocol_mouse(&mouse_event),
                        ) {
                            PluginRoute::Forward(effect) => {
                                run_effects(
                                    vec![effect],
                                    app,
                                    &keymap,
                                    &mut ui,
                                    terminal,
                                    &mut clients,
                                )
                                .await?;
                                continue;
                            }
                            PluginRoute::Consumed => continue,
                            PluginRoute::Host => {}
                        }
                    }

                    match mouse_event.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            // Convert coordinates to pane focus
                            let (col, row) = (mouse_event.column, mouse_event.row);

                            // Handle log history view clicks directly
                            if app.state.shell.current_screen
                                == crate::app::screens::ids::LOG_HISTORY
                            {
                                // Log history viewer takes full screen, starts at (0, 0)
                                app.state
                                    .log_streams
                                    .log_history_state
                                    .handle_click(col, row, 0, 0);
                            } else {
                                let press = ainb::Intent::Mouse(
                                    ainb::Pos { x: col, y: row },
                                    ainb::Btn::Left,
                                );
                                run_intent(press, app, &keymap, &mut ui, terminal, &mut clients)
                                    .await?;
                            }
                        }
                        MouseEventKind::Down(MouseButton::Right) => {
                            let press = ainb::Intent::Mouse(
                                ainb::Pos {
                                    x: mouse_event.column,
                                    y: mouse_event.row,
                                },
                                ainb::Btn::Right,
                            );
                            run_intent(press, app, &keymap, &mut ui, terminal, &mut clients)
                                .await?;
                        }
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            // Handle mouse scroll based on current view
                            use crate::app::screens::ids as screen_ids;
                            const SCROLL_LINES: usize = 3; // Lines per mouse wheel tick
                            let is_down = matches!(mouse_event.kind, MouseEventKind::ScrollDown);

                            if app.state.shell.current_screen == screen_ids::HOME {
                                // Scroll welcome panel on home screen (right side only)
                                // Before the first frame there is no rect, so
                                // the whole row counts as sidebar.
                                let sidebar_width =
                                    ui.home_sidebar_rect.map_or(u16::MAX, |rect| rect.width);
                                if mouse_event.column >= sidebar_width {
                                    for _ in 0..SCROLL_LINES {
                                        if is_down {
                                            app.state
                                                .shell
                                                .home_screen_v2_state
                                                .welcome
                                                .scroll_down();
                                        } else {
                                            app.state
                                                .shell
                                                .home_screen_v2_state
                                                .welcome
                                                .scroll_up();
                                        }
                                    }
                                }
                            } else if app.state.shell.current_screen == screen_ids::GIT_VIEW {
                                // The scroll offsets are the git view's state,
                                // so the wheel is a command the reducer applies.
                                let lines = i32::try_from(SCROLL_LINES).unwrap_or(3);
                                let lines = if is_down { lines } else { -lines };
                                run_intent(
                                    ainb::app::pointer::scroll_git_view(lines),
                                    app,
                                    &keymap,
                                    &mut ui,
                                    terminal,
                                    &mut clients,
                                )
                                .await?;
                            } else if app.state.shell.current_screen == screen_ids::LOG_HISTORY {
                                // Scroll log history viewer
                                // Shift+Scroll = horizontal, normal scroll = vertical
                                if mouse_event
                                    .modifiers
                                    .contains(crossterm::event::KeyModifiers::SHIFT)
                                {
                                    // Horizontal scroll
                                    if is_down {
                                        app.state
                                            .log_streams
                                            .log_history_state
                                            .scroll_right(SCROLL_LINES * 4);
                                    } else {
                                        app.state
                                            .log_streams
                                            .log_history_state
                                            .scroll_left(SCROLL_LINES * 4);
                                    }
                                } else {
                                    // Vertical scroll
                                    if is_down {
                                        app.state
                                            .log_streams
                                            .log_history_state
                                            .scroll_down_by(SCROLL_LINES);
                                    } else {
                                        app.state
                                            .log_streams
                                            .log_history_state
                                            .scroll_up_by(SCROLL_LINES);
                                    }
                                }
                            } else if app.state.shell.current_screen == screen_ids::SESSION_LIST
                                && app.state.scroll_session_list_by_mouse(
                                    &ui.sessions_pane,
                                    mouse_event.column,
                                    mouse_event.row,
                                    is_down,
                                    SCROLL_LINES,
                                )
                            {
                                // Session-list scrolling was handled in-memory.
                            } else {
                                // Default: scroll live logs
                                let action = if is_down {
                                    ScrollAction::ScrollLogsDown
                                } else {
                                    ScrollAction::ScrollLogsUp
                                };
                                ui.apply(action, layout, &app.state);
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            let (col, row) = (mouse_event.column, mouse_event.row);

                            // Handle log history text selection drag
                            if app.state.shell.current_screen
                                == crate::app::screens::ids::LOG_HISTORY
                            {
                                app.state.log_streams.log_history_state.update_selection(col, row);
                            } else {
                                apply_gesture(
                                    crate::app::mouse::Gesture::Drag,
                                    ainb::Pos { x: col, y: row },
                                    app,
                                    &keymap,
                                    &mut ui,
                                    terminal,
                                    &mut clients,
                                )
                                .await?;
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            let (col, row) = (mouse_event.column, mouse_event.row);

                            // Handle log history text selection end
                            if app.state.shell.current_screen
                                == crate::app::screens::ids::LOG_HISTORY
                            {
                                app.state.log_streams.log_history_state.end_selection();
                            } else {
                                apply_gesture(
                                    crate::app::mouse::Gesture::Release,
                                    ainb::Pos { x: col, y: row },
                                    app,
                                    &keymap,
                                    &mut ui,
                                    terminal,
                                    &mut clients,
                                )
                                .await?;
                            }
                        }
                        MouseEventKind::Moved => {
                            let (col, row) = (mouse_event.column, mouse_event.row);
                            apply_gesture(
                                crate::app::mouse::Gesture::Move,
                                ainb::Pos { x: col, y: row },
                                app,
                                &keymap,
                                &mut ui,
                                terminal,
                                &mut clients,
                            )
                            .await?;
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
                    if app.state.is_interactive_pane() {
                        // Forward as a bracketed paste so the inner program
                        // doesn't submit multi-line content line-by-line.
                        //
                        // crossterm ends a paste at the first terminator it
                        // reads, so a clipboard carrying its own arrives as a
                        // short paste plus the rest as keys queued behind it
                        // (#1003). Drain what follows and put its keys back
                        // into the paste; the whole is then stripped of escapes
                        // and wrapped once, so none of it runs as typed input.
                        //
                        // The poll waits 1 ms, not zero: crossterm 0.29 only
                        // hands out events its parser already holds when the
                        // poll has time left (`tty.rs` checks the parser
                        // inside `while timeout.leftover() != 0`), so a zero
                        // poll reports nothing and the tail escapes. Draining
                        // continues while events keep arriving within 1 ms,
                        // and a resize or mouse event in between is kept for
                        // the next pass rather than ending the drain.
                        //
                        // Limit: crossterm reads the tty 1024 bytes at a time.
                        // A paste larger than that, or one split in transit
                        // (over ssh), can have its tail arrive more than 1 ms
                        // after the paste event; that tail is not rejoined and
                        // reaches the pane as keys.
                        let mut tail = Vec::new();
                        while crossterm::event::poll(Duration::from_millis(1)).unwrap_or(false) {
                            match event::read() {
                                Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                                    tail.push(key);
                                }
                                Ok(Event::Key(_)) => {}
                                Ok(other) => replayed.push_back(other),
                                Err(_) => break,
                            }
                        }
                        let pasted = crate::tmux::rejoin_paste(&text, &tail);
                        let bytes = ainb_app::tmux::paste::bracketed(&pasted);
                        if let Some(report) = clients.write_input(&bytes) {
                            run_intent(report, app, &keymap, &mut ui, terminal, &mut clients)
                                .await?;
                        }
                    } else {
                        run_intent(
                            ainb::Intent::Text(text),
                            app,
                            &keymap,
                            &mut ui,
                            terminal,
                            &mut clients,
                        )
                        .await?;
                    }
                }
            }
        }

        // Apply the event a background result deferred to this iteration.
        let pending = app.state.apply_pending_event();
        run_effects(pending, app, &keymap, &mut ui, terminal, &mut clients).await?;

        // Update last_tick on every iteration so the event-poll timeout
        // stays accurate. The heavy work below is gated on a SEPARATE
        // `last_app_tick` so it runs at app_tick_rate (250 ms) even
        // though the poll cadence is much faster (33 ms).
        last_tick = Instant::now();

        if last_app_tick.elapsed() >= app_tick_rate {
            // Update mascot animation on home screen
            app.state.shell.home_screen_v2_state.tick_mascot();

            // Handle tmux-related async actions BEFORE app.tick() to get terminal access
            // IMPORTANT: Use match instead of multiple if-let with .take() to avoid dropping unmatched actions
            if let Some(action) = app.state.shell.pending_async_action.take() {
                use crate::app::state::AsyncAction;
                use tracing::{debug, info, warn};

                match action {
                    AsyncAction::KillOtherTmux(session_name) => {
                        use tokio::process::Command;

                        info!("Killing other tmux session '{}'", session_name);

                        // `=name` is an exact target. A bare `-t` resolves
                        // exact, then prefix, so killing "feat-auth" could take
                        // out a live "feat-auth-2".
                        let output = Command::new("tmux")
                            .args(["kill-session", "-t", &format!("={session_name}")])
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
                                    app.state.tmux.selected_other_tmux_index = None;
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
                        app.state.shell.ui_needs_refresh = true;
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

                            // Exact target, see the single-session kill above.
                            let output = Command::new("tmux")
                                .args(["kill-session", "-t", &format!("={session_name}")])
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
                                app.state.tmux.selected_other_tmux_index = None;
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
                        app.state.shell.ui_needs_refresh = true;
                    }

                    AsyncAction::KillWorkspaceShell(workspace_index) => {
                        use tokio::process::Command;

                        info!(
                            "[ACTION] Killing workspace shell, index: {}",
                            workspace_index
                        );

                        // Extract info first to avoid borrow issues
                        let shell_info = if let Some(workspace) =
                            app.state.sessions.workspaces.get_mut(workspace_index)
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
                            // Exact target: a bare `-t` prefix-matches.
                            let _ = Command::new("tmux")
                                .args(["kill-session", "-t", &format!("={tmux_name}")])
                                .output()
                                .await;

                            app.state.add_success_notification(format!(
                                "Killed workspace shell: {}",
                                workspace_name
                            ));
                        }

                        // Refresh workspace list to ensure UI reflects the actual state
                        app.state.load_real_workspaces().await;
                        app.state.shell.ui_needs_refresh = true;
                    }

                    // Put back any other actions we don't handle here
                    other => {
                        debug!(
                            "[ACTION] Passing through unhandled action in main loop: {:?}",
                            std::any::type_name_of_val(&other)
                        );
                        app.state.shell.pending_async_action = Some(other);
                    }
                }
            }

            match app.tick().await {
                Ok(effects) => {
                    run_effects(effects, app, &keymap, &mut ui, terminal, &mut clients).await?;
                    last_app_tick = Instant::now();
                    // Consume the refresh flag; the repaint is handled by the
                    // app-tick redraw below (perf: bead `wai`).
                    let _ = app.needs_ui_refresh();
                }
                Err(e) => {
                    use tracing::error;
                    error!("Error during app tick: {}", e);
                    // Continue running instead of crashing
                    last_app_tick = Instant::now();
                }
            }

            // The app tick is the only place mascot/spinner/streaming state
            // advances, so repaint once per tick (its existing ~250 ms cadence).
            // This is the animation floor of the dirty-gate: between ticks, with
            // no input or plugin frame, we draw nothing. (perf: bead `wai`)
            needs_redraw = true;
        }

        if app.state.shell.should_quit {
            break;
        }
    }

    Ok(())
}

/// Route preview scrolling before focused-plugin key forwarding. `tmux_preview`
/// belongs only to the split-pane session-list surface, so any other screen
/// clears stale preview state before it can claim a plugin key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewScrollRoute {
    Clear,
    Handle,
    Ignore,
}

fn preview_scroll_route(
    current_screen: &str,
    preview_is_scrolling: bool,
    observing_terminal: bool,
) -> PreviewScrollRoute {
    if observing_terminal || current_screen != crate::app::screens::ids::SESSION_LIST {
        PreviewScrollRoute::Clear
    } else if preview_is_scrolling {
        PreviewScrollRoute::Handle
    } else {
        PreviewScrollRoute::Ignore
    }
}

/// Ask the reducer to leave the interactive pane and run what it returns.
///
/// While the embed owns the keyboard the host routes Ctrl+Q itself, but the
/// release is still the reducer's decision, returned as `Effect::Detach`.
async fn detach_interactive_pane(
    app: &mut App,
    keymap: &Keymap,
    ui: &mut crate::app::ui_state::UiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    clients: &mut ainb::terminal_clients::TerminalClients,
) -> Result<()> {
    let command = ainb::CommandId::new(format!(
        "{}.{}",
        KeyContext::EmbedInteractive.name(),
        ainb::app::keymap::EMBED_DETACH_ROW
    ));
    let intent = ainb::Intent::Command(command, serde_json::Value::Null);
    run_intent(intent, app, keymap, ui, terminal, clients).await
}

/// Dispatch one intent and run the effects it returns, in order, once the
/// state it wrote is committed.
async fn run_intent(
    intent: ainb::Intent,
    app: &mut App,
    keymap: &Keymap,
    ui: &mut crate::app::ui_state::UiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    clients: &mut ainb::terminal_clients::TerminalClients,
) -> Result<()> {
    let effects = ainb::dispatch(&mut app.state, keymap, ui, intent);
    run_effects(effects, app, keymap, ui, terminal, clients).await
}

/// Run `effects` in order. Each one's reports are dispatched as soon as it
/// finishes, and whatever those queue runs after the effects already waiting,
/// so one effect never runs inside another.
async fn run_effects(
    effects: Vec<ainb::Effect>,
    app: &mut App,
    keymap: &Keymap,
    ui: &mut crate::app::ui_state::UiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    clients: &mut ainb::terminal_clients::TerminalClients,
) -> Result<()> {
    let mut queue = std::collections::VecDeque::from(effects);
    while let Some(effect) = queue.pop_front() {
        // The step that queued this effect may have released the preview
        // pane (a full-screen attach does). Close that client now, not on the
        // next loop, so it cannot outlive an effect that blocks this loop.
        clients.reconcile(&app.state);
        let plugins = app.plugin_runtime().cloned();
        for report in
            ainb::effect_host::execute(effect, terminal, ui, clients, plugins.as_ref()).await?
        {
            queue.extend(ainb::dispatch(&mut app.state, keymap, ui, report));
        }
    }
    Ok(())
}

/// Apply a drag, release or hover, then dispatch whatever it finished (a
/// resize that now wants saving).
async fn apply_gesture(
    gesture: crate::app::mouse::Gesture,
    pos: ainb::Pos,
    app: &mut App,
    keymap: &Keymap,
    ui: &mut crate::app::ui_state::UiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    clients: &mut ainb::terminal_clients::TerminalClients,
) -> Result<()> {
    match crate::app::mouse::gesture(gesture, pos, &app.state, ui) {
        Some(intent) => run_intent(intent, app, keymap, ui, terminal, clients).await,
        None => Ok(()),
    }
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
    match classify_log_sink(std::env::args().skip(1)) {
        LogSink::Stderr => {
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
        // `hangar daemon run` installs the daemon's own rolling JSONL sink
        // (`ainb_hangar_daemon::observability::install`, writing
        // `<hangar_home>/hangar/logs/daemon.<date>`) from `run_daemon_run`,
        // exactly like the standalone `ainb-hangar-daemon` binary. Installing
        // a subscriber here would make that install panic (the global default
        // would already be set), so leave the slot empty.
        LogSink::DaemonSelf => return,
        LogSink::JsonlFile => {}
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

/// Which tracing sink a CLI invocation gets (see [`classify_log_sink`]).
#[derive(Debug, PartialEq, Eq)]
enum LogSink {
    /// Short-lived one-shot subcommand: stderr only, never a JSONL file.
    Stderr,
    /// Long-running invocation (TUI, `run`, `attach`, `auth`, …): timestamped
    /// JSONL file under `~/.agents-in-a-box/logs`.
    JsonlFile,
    /// `hangar daemon run`: no subscriber installed here — the daemon
    /// installs its own daily-rotated sink under `<hangar_home>/hangar/logs`.
    DaemonSelf,
}

/// Classify an invocation from its raw arguments (`argv[1..]`).
///
/// The previous check looked only at `argv[1]`, so a leading global flag
/// (`ainb --format json fleet needs`) bypassed the short-lived list and fell
/// into the JSONL-file path — one empty log file per invocation, and pollers
/// that shell such commands every second accumulated tens of thousands of
/// them. Skip flag tokens (and the value of value-taking flags like
/// `--format`) to find the real subcommand before classifying.
fn classify_log_sink<I>(args: I) -> LogSink
where
    I: IntoIterator<Item = String>,
{
    // Strip flags: `--help`/`--version` classify immediately (clap prints and
    // exits — always short-lived); `--format` consumes its value; any other
    // `-`-prefixed token (including `--format=json`) is dropped alone.
    // `--format` is the ONLY global value-taking flag today (see the root
    // command in `cli/mod.rs`) — if another one is ever added there, list it
    // here too or its value will be misread as the subcommand.
    let mut positional: Vec<String> = Vec::new();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--help" | "-h" | "--version" | "-V" => return LogSink::Stderr,
            "--format" => {
                let _ = args.next();
            }
            s if s.starts_with('-') => {}
            _ => {
                positional.push(a);
                // Two verbs after the subcommand are enough to classify
                // (`hangar daemon run`); no need to collect an entire
                // `issue create --title …` tail.
                if positional.len() == 3 {
                    break;
                }
            }
        }
    }

    match positional.first().map(String::as_str) {
        Some(
            "list" | "logs" | "status" | "kill" | "config" | "git" | "favorites" | "init"
            | "presets" | "usage" | "claudecode" | "codex" | "statusline" | "completion" | "help"
            | "fleet" | "doctor" | "reflect" | "tmux" | "otel" | "abtop" | "learnings" | "rtk",
        ) => LogSink::Stderr,
        Some("hangar") => {
            // Every hangar verb is a one-shot CLI except the foreground
            // daemon, which owns its logging (see `LogSink::DaemonSelf`).
            if positional.get(1).map(String::as_str) == Some("daemon")
                && positional.get(2).map(String::as_str) == Some("run")
            {
                LogSink::DaemonSelf
            } else {
                LogSink::Stderr
            }
        }
        // Bare `ainb` (`None`) boots the TUI; any other subcommand (`run`,
        // `attach`, `auth`, …) is long-running — both take the JSONL file sink.
        _ => LogSink::JsonlFile,
    }
}

#[cfg(test)]
mod preview_scroll_surface_tests {
    use super::{PreviewScrollRoute, preview_scroll_route};
    use crate::app::screens::ids;

    #[test]
    fn preview_scroll_stays_on_session_list_and_clears_elsewhere() {
        assert_eq!(
            preview_scroll_route(ids::SESSION_LIST, true, false),
            PreviewScrollRoute::Handle
        );
        assert_eq!(
            preview_scroll_route(ids::HANGAR, true, false),
            PreviewScrollRoute::Clear,
            "stale preview scroll must clear before focused-plugin forwarding"
        );
    }

    #[test]
    fn observed_terminal_never_uses_host_preview_scrolling() {
        assert_eq!(
            preview_scroll_route(ids::SESSION_LIST, true, true),
            PreviewScrollRoute::Clear
        );
    }
}

#[cfg(test)]
mod log_sink_tests {
    use super::{LogSink, classify_log_sink};

    fn classify(args: &[&str]) -> LogSink {
        classify_log_sink(args.iter().map(ToString::to_string))
    }

    /// The regression that produced 77k empty log files: a leading global
    /// flag must not defeat the short-lived classification.
    #[test]
    fn global_flag_before_subcommand_is_short_lived() {
        assert_eq!(
            classify(&["--format", "json", "fleet", "needs"]),
            LogSink::Stderr
        );
        assert_eq!(classify(&["--format", "json", "list"]), LogSink::Stderr);
        assert_eq!(classify(&["--format=json", "list"]), LogSink::Stderr);
    }

    #[test]
    fn bare_tui_and_long_lived_commands_get_file_sink() {
        assert_eq!(classify(&[]), LogSink::JsonlFile);
        assert_eq!(classify(&["run"]), LogSink::JsonlFile);
        assert_eq!(classify(&["attach", "foo"]), LogSink::JsonlFile);
    }

    #[test]
    fn short_lived_verbs_get_stderr() {
        assert_eq!(classify(&["list"]), LogSink::Stderr);
        assert_eq!(classify(&["fleet", "needs"]), LogSink::Stderr);
        assert_eq!(classify(&["hangar", "daemon", "status"]), LogSink::Stderr);
        assert_eq!(classify(&["hangar", "issue", "create"]), LogSink::Stderr);
        assert_eq!(classify(&["--help"]), LogSink::Stderr);
        assert_eq!(classify(&["-V"]), LogSink::Stderr);
    }

    /// The foreground daemon installs its own sink; main must stay out of
    /// the way or `observability::install` panics on double-init.
    #[test]
    fn hangar_daemon_run_owns_its_logging() {
        assert_eq!(classify(&["hangar", "daemon", "run"]), LogSink::DaemonSelf);
        assert_eq!(
            classify(&["--format", "json", "hangar", "daemon", "run"]),
            LogSink::DaemonSelf
        );
    }
}

fn setup_panic_handler() {
    use tracing::error;

    std::panic::set_hook(Box::new(|panic_info| {
        // Kill any live embedded tmux-attach client before restoring the
        // terminal, so a panic while an interactive pane is focused doesn't
        // leak the ephemeral client. Killing the client never kills the
        // tmux session — tmux owns that.
        crate::tmux::pty_wrapper::kill_all_embed_children();

        // Ensure terminal is restored before logging the panic
        cleanup_terminal();

        error!("Application panicked: {}", panic_info);
        eprintln!("Application panicked: {}", panic_info);
        eprintln!("Please check the logs for more details.");
    }));
}

#[cfg(test)]
mod informational_only_tests {
    /// The predicate `main` uses to decide whether to skip the config load.
    fn informational_only(args: &[&str]) -> bool {
        !args.is_empty()
            && args
                .iter()
                .all(|arg| matches!(*arg, "-h" | "--help" | "-V" | "--version" | "help"))
    }

    /// Bare `ainb` is the normal TUI launch and must NOT be treated as
    /// informational. `all()` on an empty iterator is `true`, so the obvious
    /// spelling skipped the config load and env bridge for the one invocation
    /// that most needs them.
    #[test]
    fn bare_ainb_is_not_informational() {
        assert!(!informational_only(&[]), "bare `ainb` must load config");
        assert!(!informational_only(&["tui"]));
        assert!(!informational_only(&["config", "show"]));
        assert!(!informational_only(&["--help", "config"]));
    }

    #[test]
    fn help_and_version_are_informational() {
        for args in [&["--help"][..], &["-h"], &["--version"], &["-V"], &["help"]] {
            assert!(
                informational_only(args),
                "{args:?} should skip the config load"
            );
        }
    }
}
