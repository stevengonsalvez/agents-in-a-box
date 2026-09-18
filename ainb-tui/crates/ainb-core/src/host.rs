// ABOUTME: The terminal host's `App`: the reducer's `AppState` plus what only this
// process owns (the plugin runtime and its handle, the usage-dir watcher, the
// render outcome receivers), ticked by the TUI loop. It lives in the host crate
// so the runtime handle is out of `ainb-app` altogether, not only out of
// `AppState`.

use crate::app::AppState;
use crate::app::screens::ids as screen_ids;
use crate::docker::LogStreamingCoordinator;
use std::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

pub struct App {
    pub state: AppState,
    /// Owning handle to the plugin runtime's tokio executor. Held by `App`
    /// so dropping `App` joins every plugin task and tears down the runtime.
    /// `None` until [`App::init`] runs.
    plugin_runtime_owner: Option<ainb_plugin_runtime::Runtime>,
    /// The Send + Clone façade onto that runtime. The host's alone: the
    /// reducer never reaches the runtime, it queues `Effect::ForwardToPlugin`
    /// and `Effect::RunPluginAction`, and reads what the runtime knows from
    /// `plugins_host.plugin_presence`.
    plugin_runtime: Option<ainb_plugin_runtime::RuntimeHandle>,
    /// Filesystem watcher that keeps the burndown usage snapshot live by
    /// nudging session-reader to rescan on provider-dir changes. Held by
    /// `App` so the watch (and its debounce task) stops when `App` drops.
    /// `None` until [`App::init`] runs, or when no provider dir is
    /// watchable — the burndown then keeps its press-`r` refresh behaviour.
    usage_dir_watcher: Option<crate::models::usage_dir_watcher::UsageDirWatcher>,
    /// In-flight `plugin/render` outcome receivers, keyed by screen id.
    ///
    /// The frame itself comes back via `RuntimeHandle::try_recv_render`, so
    /// this oneshot used to be dropped on the spot — which threw away the
    /// FAILURE half of the result. A lazy plugin whose subprocess can no
    /// longer be spawned answers every kick with
    /// `RenderOutcome::RuntimeError`, and with the receiver dropped that
    /// error went nowhere: the screen sat on "connecting…" forever while
    /// key and mouse events were silently dropped (`child.is_none()`).
    /// Holding the receiver for one tick and polling it with `try_recv`
    /// keeps `tick_plugin_renders` synchronous while letting the failure
    /// reach `state.plugins_host.plugin_render_errors` and the user.
    plugin_render_outcomes: std::collections::HashMap<
        crate::app::screens::ScreenId,
        tokio::sync::oneshot::Receiver<ainb_plugin_runtime::RenderOutcome>,
    >,
}

impl App {
    pub fn new() -> Self {
        let mut state = AppState::new();
        // So the first frame, drawn before the first tick, has the status bar.
        state.refresh_statusline();
        Self {
            state,
            plugin_runtime_owner: None,
            plugin_runtime: None,
            usage_dir_watcher: None,
            plugin_render_outcomes: std::collections::HashMap::new(),
        }
    }

    /// Move the plugin runtime out so the caller can call
    /// [`ainb_plugin_runtime::Runtime::shutdown`] from a non-async
    /// context. Without this, dropping `App` inside `#[tokio::main]`
    /// trips the tokio "Cannot drop a runtime in a context where
    /// blocking is not allowed" panic on every clean exit. See the
    /// `Runtime::shutdown` doc for the wider picture.
    pub fn take_plugin_runtime(&mut self) -> Option<ainb_plugin_runtime::Runtime> {
        self.plugin_runtime_owner.take()
    }

    /// The host's plugin runtime handle, once [`App::init`] brought it up.
    #[must_use]
    pub const fn plugin_runtime(&self) -> Option<&ainb_plugin_runtime::RuntimeHandle> {
        self.plugin_runtime.as_ref()
    }

    /// Use `handle` as the host's plugin runtime, for a host that brings its
    /// own runtime up (tests, a host without bundled discovery).
    pub fn set_plugin_runtime(&mut self, handle: ainb_plugin_runtime::RuntimeHandle) {
        self.plugin_runtime = Some(handle);
    }

    /// Drain any freshly-painted plugin frames into
    /// `state.plugins_host.pending_plugin_renders` so the next `terminal.draw` paints
    /// the latest buffer per plugin-owned screen.
    ///
    /// Architectural contract (enforced by `build.rs` lint): this method
    /// stays *synchronous* — plugin tasks render on the tokio runtime in
    /// the background, the TUI thread only ever `try_recv`s the cached
    /// frame and dispatches a fresh render request. No `.await` on the
    /// render thread, ever.
    ///
    /// Phase 7b: no subprocess plugins are packaged yet (Phase 7c reships
    /// burndown + session-reader as Rust subprocess binaries). Loop is
    /// no-op when discovery returned empty; once 7c lands, the screen
    /// routing table below populates again.
    /// Drive plugin-owned screens. Returns `true` if a fresh plugin frame was
    /// drained into `pending_plugin_renders` this tick, so the render loop can
    /// treat that as a reason to repaint (perf: bead `wai` dirty-gate).
    pub fn tick_plugin_renders(
        &mut self,
        viewports: &mut crate::app::screens::PluginViewports,
    ) -> bool {
        // Clone the cheap Send + Clone handle so we can hold a reference
        // to the runtime while also mutably borrowing the various
        // `state.*` plugin caches below.
        let Some(handle) = self.plugin_runtime.clone() else {
            return false;
        };
        let mut drained = false;

        // Honour any pending plugin close request (root-view Esc) before
        // kicking renders — a closed screen shouldn't get another paint.
        self.state.tick_panel_close_requests(&handle);
        for (_, plugin_id) in crate::app::screens::builtin::PLUGIN_SCREENS {
            let pid = ainb_plugin_runtime::PluginId::from(*plugin_id);
            let running = handle.lifecycle_state(&pid)
                == Some(ainb_plugin_runtime::types::LifecycleState::Running);
            // Read even when stopped: the version seen then is one a restart
            // must not show again.
            let snapshot = handle
                .snapshot_get_versioned(&ainb_plugin_runtime::topics::ui_state_topic(plugin_id));
            self.state.record_plugin_ui_state(plugin_id, running, snapshot);
        }
        self.state.release_plugin_screen_watches(std::time::Instant::now(), |plugin| {
            matches!(
                handle.lifecycle_state(&ainb_plugin_runtime::PluginId::from(plugin)),
                None | Some(ainb_plugin_runtime::types::LifecycleState::Quarantined)
            )
        });

        // Static plugin-screen routing table. Pairs a stable screen id
        // (consumed by `PluginScreen` and matched against
        // `state.shell.current_screen`) with the plugin id that owns it.
        for (screen_id, plugin_id) in crate::app::screens::builtin::PLUGIN_SCREENS {
            let pid = ainb_plugin_runtime::PluginId::from(*plugin_id);

            // Refresh the text-capture flag from the plugin's last frame every
            // tick (one atomic load; `false` for an unregistered plugin). The
            // host key dispatch reads this for the focused screen to suppress
            // its global single-char shortcuts while a plugin input is focused
            // (8hx). Done before the lifecycle skip so an unregistered plugin's
            // stale flag is cleared to false rather than lingering true.
            // Compare first: an unconditional write bumps the section every
            // tick, and a mirrored host would be sent a frame for nothing.
            let captures = handle.captures_text(&pid);
            if self.state.plugins_host.plugin_captures_text.get(*screen_id) != Some(&captures) {
                self.state
                    .plugins_host
                    .plugin_captures_text
                    .insert((*screen_id).to_string(), captures);
            }
            self.state.record_plugin_presence(
                screen_id,
                crate::app::sections::PluginPresence {
                    registered: handle.lifecycle_state(&pid).is_some(),
                    wedged: handle.render_wedged(&pid),
                    // The manifest's ABI, for the keys it can decode (#1171).
                    abi: handle.plugin_abi(&pid).unwrap_or(0),
                },
            );

            // Skip plugins the runtime doesn't know about — keeps the
            // loop cheap and resilient when discovery comes up empty.
            if handle.lifecycle_state(&pid).is_none() {
                continue;
            }

            // Collect the previous kick's outcome before issuing another one.
            // Only the failure half matters here (the frame arrives via
            // `try_recv_render` above), and it MUST be collected: a lazy
            // plugin whose binary is gone answers every kick with
            // `RuntimeError`, and dropping that receiver is what left the
            // screen on "connecting…" indefinitely with no log line.
            self.collect_plugin_render_outcome(screen_id);

            // Drain the cached frame (if any) into the screen map. The
            // plugin task pushes a fresh frame each time it returns from
            // `plugin/render`; `try_recv_render` is the non-blocking
            // hand-off the render thread relies on. Unconditional (no
            // visibility gate): a frame that completed just before the
            // user navigated away must still land in the cache so the
            // screen repaints instantly on return.
            if let Some(buf) = handle.try_recv_render(&pid) {
                self.state
                    .plugins_host
                    .pending_plugin_renders
                    .insert((*screen_id).to_string(), buf);
                drained = true;
            }

            // Visibility gate: only the plugin owning the focused screen
            // gets render kicks. `LayoutComponent::render` dispatches
            // exactly `state.shell.current_screen` through the screen registry,
            // so a hidden screen's buffer is never painted — kicking its
            // renders only burns CPU. Concretely this stops (a) a
            // self-animating plugin (search spinner returning
            // `redraw=true`, which re-marks the dirty flag each frame)
            // from re-rendering an invisible screen at tick cadence, and
            // (b) the startup storm where the registration-seeded dirty
            // flag kicked all five screen plugins on the first tick —
            // `Command::Render` lazy-spawns the subprocess via
            // `ensure_running`, so that defeated `spawn = "lazy"`. Eager
            // plugins are unaffected: registration pokes them with
            // `EnsureSpawned` (see `register_kept` in the runtime), not
            // this loop.
            //
            // MUST stay above `take_render_dirty`: the gate skips the
            // consume, so a hidden plugin's dirty flag survives until the
            // user opens the screen and the first tick after the switch
            // kicks the deferred paint.
            if !self.state.plugin_screen_wanted(screen_id) {
                continue;
            }

            // Shown here, the viewport is the previous frame's allocated area
            // (stashed by `PluginScreen::render`); (0, 0) means that render
            // hasn't happened yet. Kept live for other hosts, it is the
            // largest size a watching host asked for: the frame is not painted
            // here, only its `ui.state` view is read.
            let shown_here = self.state.shell.current_screen == *screen_id;
            let (width, height) = if shown_here {
                viewports.render_areas.get(*screen_id).copied().unwrap_or((0, 0))
            } else {
                self.state.watched_viewport(screen_id).unwrap_or((0, 0))
            };

            // No allocated area stashed yet — the very first entry to this
            // screen, before `PluginScreen::render` has run once. Kicking now
            // would render at the plugin's 80×24 fallback and paint that
            // mostly-void frame across the real (larger) area: the "blank
            // screen" flash on hangar entry. Skip WITHOUT consuming the dirty
            // flag; this draw stashes the real area and the next tick kicks
            // at full size, while the loading placeholder covers the gap.
            if width == 0 || height == 0 {
                continue;
            }

            // Force a render kick whenever the live area differs from
            // the one our last kick used. This is what carries a plugin
            // screen from the seed `(0, 0)` render (which paints into the
            // plugin's fallback size, off-screen for the real layout) to
            // a render at the actual allocated viewport once
            // `PluginScreen::render` has stashed it — and likewise on any
            // resize. Plugins fed by a host-published snapshot get
            // re-marked dirty by that publish, but a screen with no such
            // feed (e.g. `witr` before a scan) would otherwise stay blank
            // forever after its dirty flag was consumed at `(0, 0)`.
            let last_viewport = viewports.last_render_viewport.get(*screen_id).copied();
            let viewport_changed = last_viewport != Some((width, height));

            // Kick the next render when something has actually changed
            // since the last paint — a keystroke landed (`send_key`), a
            // snapshot event arrived (host or plugin `publish_snapshot`),
            // the screen has never painted yet (registration seeds the
            // flag to `true`), or the allocated viewport changed.
            //
            // The dirty gate turns the loop from a fixed-cadence render
            // storm (~4/s at the 250 ms tick) into an event-driven
            // repaint: before it, the per-tick kick compounded with the
            // `event::poll` idle wait to add ~250-300 ms of perceived lag
            // per keystroke. `take_render_dirty` swaps the flag to false,
            // so evaluate the viewport-change escape hatch first to avoid
            // short-circuiting past it.
            let dirty = handle.take_render_dirty(&pid);
            if !dirty && !viewport_changed {
                continue;
            }

            viewports.last_render_viewport.insert((*screen_id).to_string(), (width, height));

            let viewport = ainb_plugin_runtime::Viewport { width, height };
            // The frame lands in the cache for `try_recv_render`; the
            // returned oneshot carries the outcome, and specifically the
            // failure case that cache can never represent. Park it until
            // the next tick's `collect_plugin_render_outcome`.
            let rx = handle.render(&pid, viewport, 0);
            self.plugin_render_outcomes.insert((*screen_id).to_string(), rx);
        }
        drained
    }

    /// Poll the parked `plugin/render` oneshot for `screen_id`, recording any
    /// failure in `state.plugins_host.plugin_render_errors` (and clearing it on success).
    ///
    /// Non-blocking by construction — `try_recv` never awaits, so
    /// `tick_plugin_renders` stays synchronous per its `build.rs`-enforced
    /// contract. A receiver that is still `Empty` is put back, so a render that
    /// outlives one tick is judged on a later tick rather than abandoned
    /// immediately — but only until the next kick for that screen replaces it.
    /// Superseding is deliberate: the newer kick reports the current state of
    /// the same plugin, so a persistent failure is still caught on the very
    /// next tick, and a failure that has since been fixed is not worth
    /// resurrecting.
    fn collect_plugin_render_outcome(&mut self, screen_id: &str) {
        use ainb_plugin_runtime::RenderOutcome;
        use tokio::sync::oneshot::error::TryRecvError;

        let Some(mut rx) = self.plugin_render_outcomes.remove(screen_id) else {
            return;
        };
        let message = match rx.try_recv() {
            Ok(RenderOutcome::Ok(_)) => {
                self.state.plugins_host.plugin_render_errors.remove(screen_id);
                return;
            }
            Ok(RenderOutcome::RuntimeError(e)) => e,
            Ok(RenderOutcome::PluginError { code, message }) => {
                format!("{message} (code {code})")
            }
            Err(TryRecvError::Empty) => {
                // Still rendering. Keep waiting rather than treating a slow
                // frame as a failure.
                self.plugin_render_outcomes.insert(screen_id.to_string(), rx);
                return;
            }
            // Sender dropped without answering: the plugin task is gone.
            Err(TryRecvError::Closed) => "plugin task stopped without answering".to_string(),
        };

        // Log once per distinct message so a failing screen doesn't spam the
        // log at tick cadence while the user sits on it.
        let is_new = self.state.plugins_host.plugin_render_errors.get(screen_id) != Some(&message);
        if is_new {
            warn!(screen = %screen_id, error = %message, "plugin render failed");
        }
        self.state
            .plugins_host
            .plugin_render_errors
            .insert(screen_id.to_string(), message);
    }

    pub async fn init(&mut self) {
        // Discover + register bundled plugins (best-effort). Each plugin
        // task lazy-spawns its subprocess on first command; the runtime
        // comes up cheap and stays empty when no plugins are installed.
        match crate::plugins::init_plugin_runtime() {
            Ok((runtime, handle, outcome)) => {
                if !outcome.loaded.is_empty() {
                    info!(loaded = ?outcome.loaded, "plugin runtime initialised");
                }
                for (name, err) in &outcome.failed {
                    warn!(plugin = %name, error = %err, "plugin failed to load");
                }
                self.plugin_runtime_owner = Some(runtime);
                self.plugin_runtime = Some(handle.clone());

                // Surface each loaded plugin's `[[config]]` schema in the
                // Settings ▸ Plugins category. `from_app_config` built the
                // config screen before discovery ran (the handle is `None` at
                // `AppState` construction), so we backfill the per-plugin rows
                // here now that the manifests are known. Defaults resolve from
                // the persisted `[plugins.<name>]` table first, else the
                // schema default. Idempotent — only the plugin rows are
                // rebuilt; the static enable/disable rows are kept.
                let manifests: Vec<ainb_plugin_protocol::manifest::Manifest> =
                    handle.registered_plugins().iter().map(|p| p.manifest.clone()).collect();
                // The rows and the table they read defaults from are one
                // section now, so the plugin list is cloned out before the
                // section is borrowed mutably.
                self.state.config.update(|config| {
                    config
                        .config_screen_state
                        .apply_plugin_manifests(&manifests, &config.app_config.plugins);
                    true
                });

                // A fresh runtime means a fresh snapshot store whose
                // version counter restarts at 0 — drop any version
                // watermark from a previous runtime so an equal-valued
                // version can't mask a new close request. Init runs once
                // today; this keeps any future runtime-restart path safe.
                self.state.shell.last_panel_close_version = None;
                // Keep the burndown usage snapshot live: watch provider
                // session dirs and nudge session-reader to rescan on
                // change, so "today" appears without the user pressing
                // `r`. Best-effort — `None` when no dir is watchable.
                self.usage_dir_watcher =
                    crate::models::usage_dir_watcher::UsageDirWatcher::start(handle);
            }
            Err(e) => {
                warn!(error = %e, "plugin runtime init failed — running plugin-free");
            }
        }

        // Kick off the live-window background poller. Render path reads
        // from its snapshot — never calls live_window::current() inline.
        self.state.host.live_window_watcher.start();

        // Initialize log streaming coordinator
        let (mut coordinator, log_sender) = LogStreamingCoordinator::new();

        // Only initialize the streaming manager if Docker is available
        // (log streaming requires Docker for Boss mode containers)
        //
        // DISPLAY CLASS: cached. Startup, and the cache is empty here anyway,
        // so this is the probe every other startup call site then reuses.
        if AppState::is_docker_available_sync() {
            info!("Docker available - initializing log streaming manager");
            if let Err(e) = coordinator.init_manager(log_sender.clone()) {
                warn!("Failed to initialize log streaming manager: {}", e);
            } else {
                info!("Log streaming coordinator initialized successfully");
            }
        } else {
            info!("Docker not available - skipping log streaming manager initialization");
            info!("Log streaming will be available when Docker is started");
        }

        self.state.host.log_streaming_coordinator = Some(coordinator);
        self.state.host.log_sender = Some(log_sender);

        // Refresh OAuth tokens that are close to expiry before first-time
        // setup is checked; quiet, because nothing is on screen yet.
        self.state.refresh_oauth_tokens_if_due(false).await;

        // REMOVED: Auth check moved to Boss mode selection only
        // Interactive mode should work without Docker authentication
        // Authentication is only required for Boss mode (Docker-based sessions)
        info!("App::init() - skipping upfront auth check (deferred to Boss mode selection)");

        // Always start with SessionList view
        info!("Starting with SessionList view (auth deferred until Boss mode)");
        // Initialize Claude integration
        if let Err(e) = self.state.init_claude_integration().await {
            warn!("Failed to initialize Claude integration: {}", e);
        }

        self.state.check_current_directory_status();

        // Load workspaces in the background so a slow Docker cannot hang
        // startup; the load policy is the state's own.
        info!("Starting background workspace loading");
        self.state.start_workspace_load();

        // Note: Log streaming will be initialized after workspaces are loaded
        // This happens in tick() when check_workspace_loading_complete() returns true
    }

    /// Initialize log streaming for all running sessions
    async fn init_log_streaming_for_sessions(&mut self) -> anyhow::Result<()> {
        if let Some(coordinator) = &mut self.state.host.log_streaming_coordinator {
            // Collect session info for streaming
            let sessions: Vec<(Uuid, String, String, crate::models::SessionMode)> = self
                .state
                .sessions
                .workspaces
                .iter()
                .flat_map(|w| &w.sessions)
                .filter(|s| s.status == crate::models::SessionStatus::Running)
                .filter_map(|s| {
                    s.container_id.clone().map(|container_id| {
                        (
                            s.id,
                            container_id,
                            format!("{}-{}", s.name, s.branch_name),
                            s.mode.clone(),
                        )
                    })
                })
                .collect();

            if !sessions.is_empty() {
                info!(
                    "Starting log streaming for {} running sessions",
                    sessions.len()
                );
                for (session_id, container_id, container_name, session_mode) in &sessions {
                    if let Err(e) = coordinator
                        .start_streaming(
                            *session_id,
                            container_id.clone(),
                            container_name.clone(),
                            session_mode.clone(),
                        )
                        .await
                    {
                        warn!(
                            "Failed to start log streaming for session {}: {}",
                            session_id, e
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Advance background work one step and hand back the effects it queued.
    ///
    /// Effects are returned only after the whole step has written state, so
    /// the host acts on committed state.
    #[must_use = "the effects are host work the tick did not perform; run them or they are lost"]
    pub async fn tick(&mut self) -> anyhow::Result<Vec<crate::app::effect::Effect>> {
        self.tick_inner().await?;
        Ok(self.state.take_effects())
    }

    async fn tick_inner(&mut self) -> anyhow::Result<()> {
        // Clean up expired notifications
        self.state.cleanup_expired_notifications();

        self.state.refresh_statusline();

        // Check for completed background workspace loading
        if self.state.check_workspace_loading_complete() {
            info!("Background workspace loading completed, initializing log streaming");
            // Now that workspaces are loaded, initialize log streaming
            if let Err(e) = self.init_log_streaming_for_sessions().await {
                warn!("Failed to initialize log streaming: {}", e);
            }
            // Also load other tmux sessions (quick operation)
            self.state.load_other_tmux_sessions().await;
            self.state.shell.ui_needs_refresh = true;
        }

        // Check for completed background skills scan
        if self.state.check_skills_load_complete() {
            self.state.shell.ui_needs_refresh = true;
        }

        // Check for completed background drift scan
        // (skill-manager v1.2 bead v12.E.4).
        if self.state.check_drift_load_complete() {
            self.state.shell.ui_needs_refresh = true;
        }
        // Check for a completed base-branch refresh (Configure picker)
        if self.state.check_branch_refresh_complete() {
            self.state.shell.ui_needs_refresh = true;
        }
        // Check for a completed remote-repo pre-flight (Configure screen)
        if self.state.check_repo_check_complete() {
            self.state.shell.ui_needs_refresh = true;
        }
        // Check for a completed empty-remote initialization ([i] on Configure)
        if self.state.check_repo_init_complete() {
            self.state.shell.ui_needs_refresh = true;
        }

        // Drain + lazily refresh the MCP pool overlay (no-op when closed).
        self.state.check_mcp_overlay();
        // Re-ensure the Headroom proxy if a Headroom session is live but the
        // proxy died (throttled, async, best-effort).
        self.state.headroom_watchdog();

        // Periodic OAuth token refresh check (every 5 minutes)
        let now = Instant::now();
        let should_check_token = self
            .state
            .host
            .last_token_refresh_check
            .map(|last| now.duration_since(last).as_secs() >= 300) // Check every 5 minutes
            .unwrap_or(true); // First time

        if should_check_token {
            self.state.host.last_token_refresh_check = Some(now);
            self.state.refresh_oauth_tokens_if_due(true).await;
        }

        // Periodic session snapshot (every 30 minutes)
        let should_snapshot = self
            .state
            .host
            .last_snapshot_time
            .map(|last| now.duration_since(last).as_secs() >= 1800)
            .unwrap_or(true);

        if should_snapshot {
            self.state.host.last_snapshot_time = Some(now);
            tokio::spawn(async {
                match crate::app::snapshot::SnapshotManager::take_snapshot().await {
                    Ok(snapshot) => {
                        if let Err(e) =
                            crate::app::snapshot::SnapshotManager::save_snapshot(&snapshot).await
                        {
                            tracing::warn!("Failed to save session snapshot: {}", e);
                        } else if let Err(e) =
                            crate::app::snapshot::SnapshotManager::prune_snapshots(48).await
                        {
                            tracing::warn!("Failed to prune old snapshots: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to take session snapshot: {}", e);
                    }
                }
            });
        }

        // Process incoming log entries (non-blocking)
        let mut log_entries = Vec::new();
        if let Some(coordinator) = &mut self.state.host.log_streaming_coordinator {
            // Collect all available log entries without blocking
            while let Some((session_id, log_entry)) = coordinator.try_next_log() {
                log_entries.push((session_id, log_entry));
            }
        }

        // Add log entries to the state
        for (session_id, log_entry) in log_entries {
            self.state.add_live_log(session_id, log_entry);
        }

        // Update tmux session previews for Interactive mode sessions
        // This captures pane content from tmux and updates session.preview_content
        if let Err(e) = self.state.update_tmux_previews().await {
            warn!("Failed to update tmux previews: {}", e);
        }

        // Process any pending async actions
        if self.state.shell.pending_async_action.is_some() {
            info!(
                ">>> tick() detected pending_async_action: {:?}",
                self.state.shell.pending_async_action
            );
        }
        match self.state.process_async_action().await {
            Ok(()) => {
                if self.state.shell.pending_async_action.is_some() {
                    info!(
                        ">>> After process_async_action, still pending: {:?}",
                        self.state.shell.pending_async_action
                    );
                }
            }
            Err(e) => {
                warn!("Error processing async action: {}", e);
                // Return to safe state if there was an error
                // BUT don't interrupt onboarding wizard or setup menu
                if self.state.shell.current_screen != screen_ids::ONBOARDING
                    && self.state.shell.current_screen != screen_ids::SETUP_MENU
                {
                    self.state.new_session.new_session_state = None;
                    self.state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
                }
                self.state.shell.pending_async_action = None;
            }
        }

        // Update logic for the app (e.g., refresh container status)

        // Periodic log updates for attached sessions
        let now = Instant::now();
        let should_update_logs = self
            .state
            .host
            .last_log_check
            .map(|last| now.duration_since(last).as_secs() >= 3) // Update every 3 seconds
            .unwrap_or(true); // First time

        if should_update_logs {
            self.state.host.last_log_check = Some(now);

            // If we have an attached session, fetch its logs
            if let Some(attached_id) = self.state.sessions.attached_session_id {
                // Check if we should update this session's logs (don't spam updates)
                let should_update_session = self
                    .state
                    .host
                    .log_last_updated
                    .get(&attached_id)
                    .map(|last| now.duration_since(*last).as_secs() >= 2) // Update session logs every 2 seconds
                    .unwrap_or(true);

                if should_update_session {
                    // Fetch logs in the background (don't block the UI)
                    if let Err(e) = self.state.fetch_claude_logs(attached_id).await {
                        warn!("Failed to fetch logs for session {}: {}", attached_id, e);
                    } else {
                        self.state.host.log_last_updated.insert(attached_id, now);
                        // Set flag to refresh UI with new logs
                        self.state.shell.ui_needs_refresh = true;
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if UI needs immediate refresh and clear the flag
    pub fn needs_ui_refresh(&mut self) -> bool {
        if self.state.shell.ui_needs_refresh {
            self.state.shell.ui_needs_refresh = false;
            true
        } else {
            false
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod plugin_render_gate_tests {
    //! Visibility gate on the render-tick loop: only the plugin owning
    //! `state.shell.current_screen` gets render kicks. A hidden plugin's dirty
    //! flag must SURVIVE the gate (it is consumed by `take_render_dirty`
    //! only after the screen check) so the deferred first paint happens
    //! on the first tick after the user opens the screen.

    use std::path::PathBuf;

    use ainb_plugin_protocol::manifest::{
        Capabilities, Lifecycle, Manifest, PluginMeta, Provides, SpawnMode, Subscribes,
    };
    use ainb_plugin_runtime::{PluginId, RegisteredPlugin, Runtime};

    use super::App;
    use crate::app::screens::ids;

    fn lazy_manifest(name: &str) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: name.into(),
                version: "0.1.0".into(),
                abi_version: 2,
                description: String::new(),
            },
            capabilities: Capabilities::default(),
            provides: Provides::default(),
            subscribes: Subscribes::default(),
            lifecycle: Lifecycle {
                spawn: SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
            config: Vec::new(),
        }
    }

    /// Real runtime + an `App` wired to it, with the named lazy plugins
    /// registered. The binary path is deliberately nonexistent — these
    /// tests assert host-side kick bookkeeping only; an actual spawn
    /// attempt would fail harmlessly on the runtime's executor.
    fn app_with_plugins(names: &[&str]) -> (Runtime, App) {
        let (runtime, handle) = Runtime::new().expect("runtime constructs without plugins");
        for name in names {
            runtime.register(RegisteredPlugin::new(
                lazy_manifest(name),
                PathBuf::from("/nonexistent/plugin-binary"),
                PathBuf::from("/nonexistent/manifest.toml"),
            ));
        }
        let mut app = App::new();
        app.set_plugin_runtime(handle);
        (runtime, app)
    }

    #[test]
    fn hidden_screen_gets_no_render_kick_and_stays_dirty() {
        let (runtime, mut app) = app_with_plugins(&["learnings"]);
        let mut viewports = crate::app::screens::PluginViewports::default();
        let handle = app.plugin_runtime().cloned().expect("handle wired");
        let pid = PluginId::from("learnings");

        app.state.shell.current_screen = ids::SESSION_LIST.to_string();
        app.tick_plugin_renders(&mut viewports);
        app.tick_plugin_renders(&mut viewports);

        // No kick: `plugin_last_render_viewport` is only written when a
        // render is dispatched.
        assert!(
            !viewports.last_render_viewport.contains_key(ids::LEARNINGS),
            "hidden screen must not receive a render kick"
        );
        // The registration-seeded dirty flag survived both ticks, so the
        // deferred first paint still happens when the screen opens.
        assert!(
            handle.take_render_dirty(&pid),
            "hidden plugin's dirty flag must survive the gated ticks"
        );

        runtime.shutdown();
    }

    #[test]
    fn dirty_plugin_kick_deferred_until_viewport_known() {
        let (runtime, mut app) = app_with_plugins(&["learnings"]);
        let mut viewports = crate::app::screens::PluginViewports::default();
        let handle = app.plugin_runtime().cloned().expect("handle wired");
        let pid = PluginId::from("learnings");

        // Ticks while hidden: gated, dirty preserved (proved above).
        app.state.shell.current_screen = ids::SESSION_LIST.to_string();
        app.tick_plugin_renders(&mut viewports);

        // User opens the learnings screen. No allocated area is stashed yet,
        // so the tick must NOT kick: a (0, 0) seed kick made the plugin paint
        // its 80×24 fallback across the real (larger) area — the blank-flash
        // bug on first entry.
        app.state.shell.current_screen = ids::LEARNINGS.to_string();
        app.tick_plugin_renders(&mut viewports);
        assert!(
            !viewports.last_render_viewport.contains_key(ids::LEARNINGS),
            "no render kick before the real viewport is known"
        );

        // The draw pass stashes the allocated area (what `PluginScreen::render`
        // does) → the next tick kicks at full size and consumes the flag.
        viewports.render_areas.insert(ids::LEARNINGS.to_string(), (120, 40));
        app.tick_plugin_renders(&mut viewports);
        assert_eq!(
            viewports.last_render_viewport.get(ids::LEARNINGS),
            Some(&(120, 40)),
            "first tick with a known viewport must kick at the real size"
        );
        assert!(
            !handle.take_render_dirty(&pid),
            "the kick must consume the dirty flag"
        );

        runtime.shutdown();
    }

    #[test]
    fn only_the_focused_plugin_screen_is_kicked() {
        let (runtime, mut app) = app_with_plugins(&["learnings", "burndown"]);
        let mut viewports = crate::app::screens::PluginViewports::default();
        let handle = app.plugin_runtime().cloned().expect("handle wired");

        app.state.shell.current_screen = ids::LEARNINGS.to_string();
        // Focused screen has painted once (area known); the hidden one hasn't.
        viewports.render_areas.insert(ids::LEARNINGS.to_string(), (100, 30));
        app.tick_plugin_renders(&mut viewports);

        assert!(
            viewports.last_render_viewport.contains_key(ids::LEARNINGS),
            "focused plugin screen must be kicked"
        );
        assert!(
            !viewports.last_render_viewport.contains_key(ids::ANALYTICS),
            "unfocused plugin screen must not be kicked"
        );
        assert!(
            handle.take_render_dirty(&PluginId::from("burndown")),
            "unfocused plugin must stay dirty for its deferred first paint"
        );

        runtime.shutdown();
    }

    /// A lazy plugin only execs its binary on first use, so one that vanished
    /// after discovery (`brew upgrade` deleting the keg a running TUI was
    /// launched from) fails at the render kick. The render oneshot used to be
    /// dropped, so that failure reached nobody: the screen sat on
    /// "connecting…" forever while key and mouse events were dropped. The
    /// outcome must land in `plugin_render_errors` for the placeholder to
    /// paint instead.
    #[test]
    fn unspawnable_plugin_records_a_render_error() {
        // `app_with_plugins` registers against /nonexistent/plugin-binary,
        // which is exactly the post-upgrade state.
        let (runtime, mut app) = app_with_plugins(&["learnings"]);
        let mut viewports = crate::app::screens::PluginViewports::default();

        app.state.shell.current_screen = ids::LEARNINGS.to_string();
        viewports.render_areas.insert(ids::LEARNINGS.to_string(), (120, 40));

        // First tick kicks the render; the spawn attempt and its failure
        // happen on the runtime's executor, so poll a bounded number of
        // ticks for the outcome rather than assuming one is enough.
        let mut recorded = None;
        for _ in 0..200 {
            app.tick_plugin_renders(&mut viewports);
            if let Some(err) = app.state.plugins_host.plugin_render_errors.get(ids::LEARNINGS) {
                recorded = Some(err.clone());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let err = recorded.expect(
            "a plugin whose binary cannot be spawned must record a render error, \
             not leave the screen on the loading placeholder forever",
        );
        assert!(
            !err.is_empty(),
            "the recorded render error must carry a message to show the user"
        );

        runtime.shutdown();
    }
}
