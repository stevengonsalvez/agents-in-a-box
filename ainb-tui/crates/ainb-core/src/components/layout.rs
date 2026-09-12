// ABOUTME: Main layout component handling split-pane arrangement and bottom menu bar

use crate::app::ui_state::UiState;
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

// Premium color palette (TUI Style Guide)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// Notice boxes drawn at once. The rest are counted in a "+N more" line.
const MAX_VISIBLE_NOTIFICATIONS: usize = 4;
/// Wrapped rows of message a single notice box may grow to.
const NOTIFICATION_MAX_TEXT_ROWS: usize = 6;
/// Widest a notice box gets, before the terminal's own width is applied.
const NOTIFICATION_MAX_WIDTH: u16 = 64;
/// Narrowest box still worth drawing a border around.
const NOTIFICATION_MIN_WIDTH: u16 = 28;
/// Printed on the notice stack itself, per the keybinding-hints-near-the-
/// control rule. Matches the chord wired in `events::handle_key_event`.
pub const NOTIFICATION_DISMISS_HINT: &str = "Ctrl+X dismiss";
/// Where a dismissed or expired notice can still be read.
pub const NOTIFICATION_LOG_HINT: &str = "l from home";

/// The glyph a notice's level is drawn with.
fn notice_icon(notification: &crate::app::state::Notification) -> &'static str {
    match notification.notification_type {
        crate::app::state::NotificationType::Success => "✓ ",
        crate::app::state::NotificationType::Error => "✗ ",
        crate::app::state::NotificationType::Warning => "⚠ ",
        crate::app::state::NotificationType::Info => "ℹ ",
    }
}

/// The colour a notice's level is drawn in.
fn notice_color(notification: &crate::app::state::Notification) -> Color {
    match notification.notification_type {
        crate::app::state::NotificationType::Success => SELECTION_GREEN,
        crate::app::state::NotificationType::Error => Color::Rgb(230, 100, 100),
        crate::app::state::NotificationType::Warning => WARNING_ORANGE,
        crate::app::state::NotificationType::Info => CORNFLOWER_BLUE,
    }
}

/// Greedy word-wrap for a notice, with the level icon on the first row and the
/// continuation rows indented under the text.
///
/// Terminal cell width, not `char` count: an emoji in a message (this codebase
/// puts them in plenty) is two cells wide and a `char`-counted wrap overflows
/// the border by exactly as many emoji as the line holds.
///
/// Breaking between words is not enough. An ainb failure names worktree paths,
/// tmux session names and URLs, and a single one of those is routinely wider
/// than the whole box — so a token that cannot fit is split across rows rather
/// than allowed to run past the border, where the `Paragraph` would clip it and
/// take the rest of the message with it.
fn wrap_notification(message: &str, icon: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;

    let indent = " ".repeat(UnicodeWidthStr::width(icon));
    let body = width.saturating_sub(UnicodeWidthStr::width(icon)).max(1);

    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in message.split_whitespace() {
        if UnicodeWidthStr::width(word) > body {
            // Nothing to be gained by starting it on a fresh row: it does not
            // fit on one either. Fill the current row, then break the rest at
            // the border.
            if !current.is_empty() {
                current.push(' ');
            }
            let mut chunks = split_to_width(&format!("{current}{word}"), body);
            // The tail stays open so a following word can share its row.
            current = chunks.pop().unwrap_or_default();
            rows.extend(chunks);
            continue;
        }
        let candidate_width = if current.is_empty() {
            UnicodeWidthStr::width(word)
        } else {
            UnicodeWidthStr::width(current.as_str()) + 1 + UnicodeWidthStr::width(word)
        };
        if !current.is_empty() && candidate_width > body {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }

    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            if i == 0 {
                format!("{icon}{row}")
            } else {
                format!("{indent}{row}")
            }
        })
        .collect()
}

/// Cut `text` into pieces at most `width` CELLS wide.
///
/// Always advances by at least one character, so a character wider than
/// `width` (a two-cell glyph in a one-cell budget) overflows by a cell rather
/// than looping forever.
fn split_to_width(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;

    let width = width.max(1);
    let mut pieces: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cells = UnicodeWidthChar::width(ch).unwrap_or(0);
        if !current.is_empty() && used + cells > width {
            pieces.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used += cells;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    pieces
}

/// Trim `rows` to `max`, replacing the last kept row with a marker naming what
/// was dropped.
///
/// Silently dropping the tail is what the old fixed-height box did, and it is
/// the reason messages were written short: a notice that stops mid-sentence
/// with no marker tells the operator nothing was missing. Returns the rows to
/// draw and how many were dropped.
fn clamp_notice_rows(rows: Vec<String>, max: usize, width: usize) -> (Vec<String>, usize) {
    if rows.len() <= max {
        return (rows, 0);
    }
    let keep = max.saturating_sub(1);
    let dropped = rows.len() - keep;
    let mut kept: Vec<String> = rows.into_iter().take(keep).collect();
    kept.push(more_lines_marker(dropped, width));
    (kept, dropped)
}

/// The widest "N more lines" marker that fits `width` cells.
fn more_lines_marker(dropped: usize, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;

    for candidate in [
        format!("… +{dropped} more lines — see the log"),
        format!("… +{dropped} more lines"),
        format!("… +{dropped}"),
        "…".to_string(),
    ] {
        if UnicodeWidthStr::width(candidate.as_str()) <= width {
            return candidate;
        }
    }
    "…".to_string()
}

/// The bottom-border label for the last box drawn.
///
/// The dismiss hint is the part that cannot be dropped: it is the only place
/// the chord is advertised while a notice is up, and no one guesses `Ctrl+X`.
/// The suppressed-box count rides along when the border is wide enough for it.
fn notice_footer(suppressed: usize, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;

    let mut candidates = Vec::new();
    if suppressed > 0 {
        candidates.push(format!(
            " +{suppressed} more · {NOTIFICATION_DISMISS_HINT} "
        ));
        candidates.push(format!("+{suppressed} more · {NOTIFICATION_DISMISS_HINT}"));
    }
    candidates.push(format!(" {NOTIFICATION_DISMISS_HINT} "));
    candidates.push(NOTIFICATION_DISMISS_HINT.to_string());

    for candidate in candidates {
        if UnicodeWidthStr::width(candidate.as_str()) <= width {
            return candidate;
        }
    }
    NOTIFICATION_DISMISS_HINT.to_string()
}

use super::{
    ClaudeChatComponent, ConfirmationDialogComponent, HelpComponent, LiveLogsStreamComponent,
    LogsViewerComponent, NewSessionComponent, SessionListComponent, TmuxPreviewPane,
};
use crate::app::{
    AppState, ScreenRegistry,
    screens::{builtin::register_builtins, ids as screen_ids},
};

/// Cell size the embed gets under the interactive layout: the right pane's
/// interior next to the user's CURRENT sidebar (the embed honors the sidebar
/// — collapsed rail or full width — rather than forcing a layout). Used at
/// entry (`EnterInteractivePane`) so the very first attach already matches
/// what the first interactive frame will resize to — otherwise tmux reflows
/// the session twice back-to-back (attach size → layout size).
///
/// Must mirror `render`'s split: vertical chrome is the status bar (3) +
/// session info (3) + current menu bar height, and the pane border takes 2
/// more rows/cols off the interior. `sidebar_width` is the live
/// `sessions_pane_state.effective_width(..)` for the same terminal width.
pub fn interactive_embed_size(
    width: u16,
    height: u16,
    sidebar_width: u16,
    show_menu_bar: bool,
) -> (u16, u16) {
    const PANE_BORDERS: u16 = 2;
    let vertical_chrome = 3 + 3 + session_menu_bar_height(show_menu_bar);
    let rows = height.saturating_sub(vertical_chrome + PANE_BORDERS).max(1);
    let cols = width.saturating_sub(sidebar_width.saturating_add(PANE_BORDERS)).max(1);
    (rows, cols)
}

fn session_menu_bar_height(show_menu_bar: bool) -> u16 {
    if show_menu_bar { 6 } else { 1 }
}

/// Apply the effects of a frame that only the frame could measure.
///
/// The embed's size, the HomeScreen sidebar rect, the welcome panel's viewport
/// and the log-history entry pane all come out of the layout arithmetic, so
/// they cannot be known before the draw. Applying them is a mutation and the
/// draw takes `&AppState`, so the draw records what it measured in [`UiState`]
/// and this hands it back once the frame is out — which is exactly when the
/// hit tests and scroll clamps that read them next run.
pub fn publish_after_draw(state: &mut AppState, ui: &mut UiState) {
    if let Some((rows, cols)) = ui.embed_desired_size.take() {
        if let Some(embed) = state.embed.as_mut() {
            let _ = embed.resize(rows, cols);
        }
    }

    let home = &mut state.home_screen_v2_state;
    if ui.home_sidebar_rect.is_some() {
        home.last_sidebar_rect = ui.home_sidebar_rect;
    }
    (home.welcome.content_height, home.welcome.visible_height) = ui.welcome_viewport;

    state.log_history_state.log_entries_area = ui.log_entries_area;
}

pub struct LayoutComponent {
    session_list: SessionListComponent,
    logs_viewer: LogsViewerComponent,
    claude_chat: ClaudeChatComponent,
    live_logs_stream: LiveLogsStreamComponent,
    help: HelpComponent,
    new_session: NewSessionComponent,
    confirmation_dialog: ConfirmationDialogComponent,
    tmux_preview: TmuxPreviewPane,
    /// Built-in screens (full-screen views). Split-pane fallback (SessionList,
    /// Logs, NewSession, ClaudeChat, SearchWorkspace, NonGitNotification) is
    /// not in the registry; layout's split-pane path renders those.
    screens: ScreenRegistry,
}

impl LayoutComponent {
    pub fn new() -> Self {
        let mut screens = ScreenRegistry::new();
        register_builtins(&mut screens);
        Self {
            session_list: SessionListComponent::new(),
            logs_viewer: LogsViewerComponent::new(),
            claude_chat: ClaudeChatComponent::new(),
            live_logs_stream: LiveLogsStreamComponent::new(),
            help: HelpComponent::new(),
            new_session: NewSessionComponent::new(),
            confirmation_dialog: ConfirmationDialogComponent::new(),
            tmux_preview: TmuxPreviewPane::new(),
            screens,
        }
    }

    /// Paint the tab strip and the footer onto a pane's border.
    ///
    /// Drawn as a title on the existing block rather than as a row of its own:
    /// five words in a dedicated row costs the content a line on every screen,
    /// and the preview pane is the one that can least afford it.
    fn render_tab_strip(
        frame: &mut Frame,
        area: Rect,
        state: &AppState,
        active: crate::components::session_tabs::SessionTab,
    ) {
        use crate::components::session_tabs;
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(SUBDUED_BORDER))
            .title(session_tabs::strip(state, active))
            .title_bottom(session_tabs::footer(state, active, false));
        // Only the border cells are painted, so whatever the pane already drew
        // inside stays exactly as it was.
        frame.render_widget(block, area);
    }

    /// Render one non-preview tab into the right pane.
    fn render_session_tab(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        state: &AppState,
        active: crate::components::session_tabs::SessionTab,
    ) {
        use crate::components::session_tabs::{self, SessionTab};

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBDUED_BORDER))
            .style(Style::default().bg(PANEL_BG))
            .title(session_tabs::strip(state, active))
            // The footer stops promising the attach digits whenever the pane
            // owns keys, not only while text is being typed: on the card half
            // of a conversation a `3` is still the chat's, not an attach.
            .title_bottom(session_tabs::footer(
                state,
                active,
                state.session_tab_owns_keys(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        match active {
            // Handled by the caller, which keeps the tmux mirror it always had.
            SessionTab::Preview => {}
            SessionTab::Ask => session_tabs::render_ask(frame, inner, state),
            SessionTab::Err => session_tabs::render_err(frame, inner, state),
            SessionTab::Log => {
                // The read itself belongs to the worker `tick_before_draw`
                // starts. Asking for it here — inside `terminal.draw` — is what
                // made this pane cost up to 948 ms a frame on a real store.
                let log = state.get_selected_session().map_or(
                    crate::fleet::session_log::Log::Rows(Vec::new()),
                    |session| {
                        state.session_log.read(&crate::fleet::session_log::LogKey::new(
                            &session.workspace_path,
                            AppState::agent_hook_name(session.agent_type),
                        ))
                    },
                );
                session_tabs::render_log(frame, inner, &log);
            }
            // Pal carries a HEADER the thread does not: its engine,
            // model and guardrail dial. The conversation under it is the same
            // state machine either way, so the two cannot drift in what they
            // render or which failures they report.
            SessionTab::Pal => {
                let header = session_tabs::pal_header(&state.pal_dial);
                // Inserted between the header and the conversation rather than
                // replacing either. Both still have something true to say with
                // the daemon down — the dials an operator recovers an adapter
                // with, and the call the chat could not make — and the offer is
                // the one thing neither of them could say.
                let offer = state.pal_daemon_cta_open().then_some(&state.daemon_start_cta);
                // `chat_host`, not `chat_host_for`: the conversation was ticked
                // in `tick_before_draw`, and `chat_host_for` ENDS by calling
                // this, so what is painted is what was ticked rather than a
                // second guess at which host this tab shows.
                session_tabs::render_pal(frame, inner, header, offer, state.chat_host(active));
            }
            SessionTab::Thread => {
                // Checked rows win over the cursor, the same rule `Enter` and
                // `r` follow on this screen: with a multi-select active this
                // pane is a broadcast to the checked set, not one session's
                // private thread. The strip label says which it currently is.
                let targets = state.broadcast_targets();
                if targets.is_empty() {
                    match state.chat_host(active) {
                        Some(host) => session_tabs::render_chat(frame, inner, host),
                        // Reachable only for `thread` with no session selected,
                        // which the strip already dims — say it rather than
                        // paint an empty box.
                        None => frame.render_widget(
                            Paragraph::new("select a session to open its thread")
                                .style(Style::default().fg(MUTED_GRAY)),
                            inner,
                        ),
                    }
                } else {
                    let unreachable = state.broadcast_unreachable();
                    session_tabs::render_broadcast(
                        frame,
                        inner,
                        &state.broadcast,
                        &targets,
                        unreachable,
                    );
                }
            }
        }
    }

    /// Advance every live state machine the draw path used to tick, then let
    /// the draw be a pure read of the result.
    ///
    /// Each of these was written inside `render` because that is where the
    /// operator sees the result, not because painting is when they should
    /// happen: a conversation, a dial, an answer worker and a broadcast all
    /// advance on wall-clock time. The gates are the ones `render` applied —
    /// same screen, same tab, same emptiness check — so nothing ticks that
    /// would not have ticked before.
    pub fn tick_before_draw(&self, state: &mut AppState) {
        use crate::components::session_tabs::{self, SessionTab};

        // Fold in what the daemon workers reported and keep the collector
        // alive. Gated on the screen for the same reason `DaemonsScreen::render`
        // was the only caller: an operator who never opens it never starts a
        // collector thread.
        if state.current_screen == screen_ids::DAEMONS {
            state.hangar.daemons_state.tick();
        }

        // Registry-routed screens return before any of this in `render`.
        if self.screens.contains(&state.current_screen) {
            return;
        }

        // The active tab, reconciled against what is actually available: a tab
        // can go dead under the operator (the ASK is answered, the cursor moves
        // off a session row) and leaving them on a stale pane shows a question
        // they can no longer act on.
        let active = session_tabs::resolve(state, state.session_tab);
        state.session_tab = active;

        // Fold in whatever the answer worker reported. EVERY frame, not only on
        // the `ask` tab: the row's `SENT` chip is painted by the session list,
        // so an operator who sends and then switches tabs would otherwise watch
        // that chip stay SENT forever.
        if state.ask_state.tick() {
            state.ui_needs_refresh = true;
        }

        // An attached embed owns the right pane outright, and `preview` is a
        // tmux mirror with no state machine of its own.
        if state.is_interactive_pane() || active == SessionTab::Preview {
            return;
        }

        match active {
            SessionTab::Preview | SessionTab::Err => {}
            SessionTab::Ask => {
                // Point the pane at the request it is showing BEFORE painting.
                // Without this the focus is only initialised by the first key
                // press, so a request with no options opens with the composer
                // unfocused — no cursor, no caret, and the operator's first
                // characters fall through to the session shortcuts.
                if let Some(chip) = session_tabs::selected_blocking(state).cloned() {
                    state.ask_state.retarget(&chip);
                }
            }
            SessionTab::Log => {
                // Started here rather than at construction, for the same reason
                // the attention poller is: an `ainb` invocation that never opens
                // this pane never opens the notifications store. `spawn` is
                // idempotent.
                crate::fleet::session_log::spawn(&state.session_log, &state.session_log_running);
            }
            SessionTab::Pal => {
                // The dial ticks with the pane, so the registry read and any
                // in-flight configure land without the operator pressing
                // anything, exactly like the chat host's own tick.
                if state.pal_dial.tick() {
                    state.ui_needs_refresh = true;
                }
                // The offer's own tick, for the same reason: the start runs on
                // a detached worker, and its result has to reach the pane
                // without the operator pressing anything else.
                if state.daemon_start_cta.tick() {
                    state.ui_needs_refresh = true;
                }
                let _ = state.chat_host_for(active);
            }
            SessionTab::Thread => {
                // Checked rows win over the cursor, the same rule `Enter` and
                // `r` follow on this screen: with a multi-select active this
                // pane is a broadcast to the checked set, not one session's
                // private thread.
                if state.broadcast_targets().is_empty() {
                    let _ = state.chat_host_for(active);
                } else if state.broadcast.tick() {
                    state.ui_needs_refresh = true;
                }
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame, state: &AppState, ui: &mut UiState) {
        // Both are re-published by the branch that paints the embed. Cleared
        // first so a frame that does NOT paint it can neither replay a stale
        // resize nor leave the mouse forwarder pointing at a pane that is gone
        // — which is what `release_interactive_pane` used to have to do by hand.
        ui.embed_desired_size = None;
        ui.embed_pane_area = None;
        // Full-screen views go through the screen registry. Each Screen impl
        // owns its component(s) and renders any screen-specific overlays
        // (e.g. Config's auth-provider/config popups). Help overlay is
        // rendered post-screen as it's universal across full-screen views.
        let frame_size = frame.area();
        if let Some(screen) = self.screens.get_mut(&state.current_screen) {
            tracing::debug!("Rendering screen via registry: {}", state.current_screen);
            screen.render(frame, frame_size, state, ui);
            // Notifications must render on registry-routed screens too —
            // before this fix they only painted on the legacy
            // fallthrough path, which silently masked any
            // `state.add_*_notification` call from a screen-specific
            // event handler (e.g. SkillManager's [s]→Sync routing,
            // bead v12.1.T3). Painted before the help overlay so the
            // help panel still wins z-order if both are visible.
            self.render_notifications(frame, frame_size, state);
            if state.help_visible {
                tracing::debug!("Rendering help overlay on {}", state.current_screen);
                self.help.render(frame, frame_size);
            }
            // The confirmation dialog is a universal, highest-priority
            // overlay — it must paint on registry-backed screens too
            // (HomeScreen, Inbox, …), not just the split-pane views.
            // Key handling already runs pre-screen in `app::events`, so
            // without this the dialog could be live + interactive but
            // invisible (e.g. the first-run notify-install prompt fired
            // on the HomeScreen).
            // MCP pool overlay paints above the screen, below a confirmation
            // dialog (so a stop confirmation sits on top of it).
            if let Some(ref overlay) = state.mcp_pool.mcp_overlay {
                crate::components::mcp_overlay::render(frame, frame_size, overlay);
            }
            if state.confirmation_dialog.is_some() {
                self.confirmation_dialog.render(frame, frame_size, state);
            }
            return;
        }

        // The bottom keymap legend can be hidden (⇧M) to give the session list
        // more room; hidden it collapses to a single hint row.
        let menu_bar_h =
            session_menu_bar_height(state.config.app_config.ui_preferences.show_session_menu_bar);
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),          // Top status bar
                Constraint::Min(0),             // Main content area
                Constraint::Length(3),          // Session info (single line + borders)
                Constraint::Length(menu_bar_h), // Bottom menu bar / collapsed hint
            ])
            .split(frame.area());

        // Render top status bar
        self.render_status_bar(frame, main_layout[0], state, ui);

        // Simple 2-panel layout: session list | logs (Claude chat is now a popup).
        // The interactive embed honors whatever sidebar layout the user has
        // (decision 2026-06-12: no forced collapse — the sidebar is a fixed
        // ~40 cols, modern TUIs reflow cleanly, and `B` pre-collapses to the
        // rail when maximum embed width is wanted).
        let sessions_width = ui.sessions_pane.effective_width(main_layout[1].width);
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sessions_width), // Session list
                Constraint::Min(0),                 // Live logs stream
            ])
            .split(main_layout[1]);
        ui.sessions_pane.set_layout(content_chunks[0], content_chunks[1]);

        // Pass focus information to components
        if ui.sessions_pane.collapsed {
            self.render_collapsed_sessions_rail(frame, content_chunks[0], state, ui);
        } else {
            self.session_list.render(frame, content_chunks[0], state, ui);
        }

        // The legacy capture renderer supports regular sessions and shells.
        // A live observer can also render SSH and other-tmux sessions, but its
        // initial and failed states must retain those rows' live-logs fallback.
        let selected_has_legacy_preview = state
            .get_selected_session()
            .and_then(|session| session.tmux_session_name.as_ref())
            .is_some()
            || state.selected_shell_session().is_some();
        let observing_selection = state.is_observing_selected_terminal();

        // Already reconciled by `tick_before_draw` against what is actually
        // available, so this is a read of a settled value, not a second guess.
        let active_tab = state.session_tab;

        if state.is_interactive_pane() {
            // Live interactive embed occupies the right pane. Resize the embed to
            // the pane interior (minus the border) so the inner program reflows,
            // then render the live terminal in place of the read-only preview.
            //
            // The embed OUTRANKS the strip: an attached terminal owns every
            // keystroke, so painting a tab strip over it would advertise
            // controls the pane cannot receive.
            let area = content_chunks[1];
            let inner = area.inner(Margin {
                vertical: 1,
                horizontal: 1,
            });
            ui.embed_desired_size = Some((inner.height, inner.width));
            // Publish the interior so mouse events can be translated into
            // 1-based pane-local SGR coordinates (see encode_mouse_event).
            ui.embed_pane_area = Some(inner);
            self.tmux_preview.render_interactive(frame, area, state);
        } else if active_tab == crate::components::session_tabs::SessionTab::Preview {
            // The observer is not a branch of its own: it is what `preview`
            // SHOWS when one is running, a live mirror in place of the
            // read-only capture. Gating it on the tab matters — an
            // `observing_selection` arm that outranked the strip made every
            // other tab unreachable while the state machine still advanced
            // through them, so `ask` could not be opened on the very session
            // whose chip sent the operator looking, and keys routed to a chat
            // surface that was not on screen.
            if observing_selection {
                let area = content_chunks[1];
                let inner = area.inner(Margin {
                    vertical: 1,
                    horizontal: 1,
                });
                ui.embed_desired_size = Some((inner.height, inner.width));
                self.tmux_preview.render_observer(frame, area, state);
            } else if selected_has_legacy_preview {
                // Read-only tmux capture. Initial attach can take one frame, so
                // this stays the transient fallback rather than the
                // steady-state renderer.
                self.tmux_preview.render(frame, content_chunks[1], state);
            } else {
                // Render traditional live logs stream
                self.live_logs_stream.render(frame, content_chunks[1], state);
            }
            // Painted for the observer too. Unlike the interactive embed above,
            // an observer never receives keys — `is_observing_tmux_session`
            // requires `!is_interactive_pane()` — so the strip advertises
            // nothing the pane cannot do, and without it the operator loses the
            // only affordance saying the other tabs exist.
            Self::render_tab_strip(frame, content_chunks[1], state, active_tab);
        } else {
            self.render_session_tab(frame, content_chunks[1], state, active_tab);
        }

        // Render bottom logs area (traditional logs viewer)
        self.logs_viewer.render(frame, main_layout[2], state);

        // Render bottom menu bar. Publish its rect so a mouse click on the
        // legend (or its collapsed hint row) can toggle visibility.
        ui.menu_bar_area = Some(main_layout[3]);
        self.render_menu_bar(frame, main_layout[3], state);

        // Render help overlay if visible
        if state.help_visible {
            self.help.render(frame, frame.area());
        }

        // Render new session overlay if visible
        if state.current_screen == screen_ids::NEW_SESSION
            || state.current_screen == screen_ids::SEARCH_WORKSPACE
        {
            self.new_session.render(frame, frame.area(), state);
        }

        // Render Claude chat popup if visible
        if state.current_screen == screen_ids::CLAUDE_CHAT {
            let popup_area = centered_rect(80, 80, frame.area());
            self.claude_chat.render(frame, popup_area, state);
        }

        // MCP pool overlay (above the screen, below the confirmation dialog).
        if let Some(ref overlay) = state.mcp_pool.mcp_overlay {
            crate::components::mcp_overlay::render(frame, frame.size(), overlay);
        }

        // Render confirmation dialog if visible (highest priority overlay)
        if state.confirmation_dialog.is_some() {
            self.confirmation_dialog.render(frame, frame.area(), state);
        }

        // Render quick commit dialog if visible
        if state.is_in_quick_commit_mode() {
            self.render_quick_commit_dialog(frame, frame.area(), state);
        }

        // Render notifications (top-right corner)
        self.render_notifications(frame, frame.area(), state);
    }

    /// Get mutable reference to live logs component for scroll handling
    pub fn live_logs_mut(&mut self) -> &mut LiveLogsStreamComponent {
        &mut self.live_logs_stream
    }

    /// Get mutable reference to tmux preview component for scroll handling
    pub fn tmux_preview_mut(&mut self) -> &mut TmuxPreviewPane {
        &mut self.tmux_preview
    }

    fn render_collapsed_sessions_rail(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &AppState,
        ui: &UiState,
    ) {
        let border_color = if ui.sessions_pane.edge_highlighted() {
            GOLD
        } else if state.focused_pane == crate::app::state::FocusedPane::Sessions {
            SELECTION_GREEN
        } else {
            SUBDUED_BORDER
        };
        let rail = Paragraph::new(vec![
            Line::from(Span::styled(
                "[+]",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            )),
            // 'B' is the keyboard twin of clicking [+] (hint next to control).
            Line::from(Span::styled(
                "B",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled("S", Style::default().fg(CORNFLOWER_BLUE))),
            Line::from(Span::styled("E", Style::default().fg(CORNFLOWER_BLUE))),
            Line::from(Span::styled("S", Style::default().fg(CORNFLOWER_BLUE))),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color))
                .style(Style::default().bg(DARK_BG)),
        )
        .alignment(Alignment::Center);

        frame.render_widget(rail, area);
    }

    /// Bottom shortcut legend. Two presentations share the same key set:
    ///   - **Two-column** (wide terminals): session actions on the left,
    ///     panels/views on the right, split by a vertical divider. Reclaims
    ///     the horizontal space the old centred stack wasted.
    ///   - **Stacked** (narrow terminals, < `TWO_COL_MIN_WIDTH`): the original
    ///     four centred lines, which the 80-col truncation test still pins.
    ///
    /// The dispatch is width-only so the layout degrades predictably and the
    /// `menu_bar_keys_not_truncated_at_80_cols` test exercises the stacked
    /// path unchanged.
    fn render_menu_bar(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Hidden legend → a single muted hint row (still discoverable: shows the
        // ⇧M un-hide key plus help/home).
        if !state.config.app_config.ui_preferences.show_session_menu_bar {
            self.render_menu_bar_collapsed(frame, area);
            return;
        }
        // Below this width the two-column split can't hold the widest
        // session-action line (~50 cols) plus the panels column without
        // clipping, so fall back to the stacked legend.
        const TWO_COL_MIN_WIDTH: u16 = 110;
        if area.width >= TWO_COL_MIN_WIDTH {
            self.render_menu_bar_two_col(frame, area, state);
        } else {
            self.render_menu_bar_stacked(frame, area, state);
        }
    }

    /// One-line stand-in shown when the keymap legend is hidden.
    fn render_menu_bar_collapsed(&self, frame: &mut Frame, area: Rect) {
        let key = |k: &'static str, color: Color| {
            Span::styled(k, Style::default().fg(color).add_modifier(Modifier::BOLD))
        };
        let desc = |d: &'static str| Span::styled(d, Style::default().fg(MUTED_GRAY));
        let line = Line::from(vec![
            key("⇧M", GOLD),
            desc(" expand/collapse  "),
            key("?/H", CORNFLOWER_BLUE),
            desc(" help  "),
            key("q", CORNFLOWER_BLUE),
            desc(" home"),
        ]);
        frame.render_widget(
            Paragraph::new(line)
                .alignment(Alignment::Center)
                .style(Style::default().bg(PANEL_BG)),
            area,
        );
    }

    fn render_menu_bar_stacked(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Premium styled command bar with separators - 3 lines for better
        // discoverability. Grouped: (1) navigation + selection, (2) session
        // actions, (3) git / tools / system. Every key that the home screen
        // actually binds is surfaced here so nothing is hidden from the user.
        let key = |k: &'static str, color: Color| {
            Span::styled(k, Style::default().fg(color).add_modifier(Modifier::BOLD))
        };
        let desc = |d: &'static str| Span::styled(d, Style::default().fg(MUTED_GRAY));
        let sep = || Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER));
        let red = Color::Rgb(230, 100, 100);

        // Line 1: Navigation + selection. Panel shortcuts (inbox/stats/
        // witr/skills) moved to line 4 so the unread badge can grow
        // without pushing this line past the 80-col minimum (see the
        // `menu_bar_keys_not_truncated_at_80_cols` test).
        let line1_spans = vec![
            key("n", GOLD),
            desc("ew "),
            key("⇧E", GOLD),
            desc("xpand "),
            key("Tab", GOLD),
            desc(" focus"),
            sep(),
            // Attach / select group
            key("a", SELECTION_GREEN),
            desc("ttach "),
            key("→", SELECTION_GREEN),
            desc(" Pane attach "),
            key("1-9", SELECTION_GREEN),
            desc(" quick "),
            key("Space", SELECTION_GREEN),
            desc(" select"),
            sep(),
            key("s", GOLD),
            desc("tar"),
        ];

        // Line 2: Session actions (restart slot swaps r/resume ↔ e/recreate) + git
        let line2_spans = vec![
            key("r", SELECTION_GREEN),
            desc(" resume "),
            key("d", red),
            desc("elete "),
            key("⇧D", red),
            desc(" del-sel "),
            key("o", SELECTION_GREEN),
            desc(" editor "),
            key("$", GOLD),
            desc(" shell "),
            key("F2", SELECTION_GREEN),
            desc(" rename"),
            sep(),
            key("g", CORNFLOWER_BLUE),
            desc("it "),
            key("p", CORNFLOWER_BLUE),
            desc(" commit"),
        ];

        // Line 3: Tools
        let line3_spans = vec![
            key("c", WARNING_ORANGE),
            desc("laude "),
            key("f", WARNING_ORANGE),
            desc(" refresh "),
            key("⇧F", WARNING_ORANGE),
            desc(" filter "),
            sep(),
            key("u", MUTED_GRAY),
            desc(" re-auth "),
        ];

        // Line 4: Panels + System. Every panel screen mirrors its
        // home-menu letter here (the session-list key handler binds the
        // same set), and closing a panel returns to this screen.
        //
        // The `b inbox` hint and its unread badge are gone with the Inbox
        // screen: a session's notification history is the `log` tab on this
        // screen now, and the fleet-wide view is the hangar plugin's own.
        let mut line4_spans = Vec::new();
        line4_spans.extend([
            key("i", GOLD),
            desc(" stats "),
            key("w", GOLD),
            desc(" witr "),
            key("k", GOLD),
            desc(" skills "),
            key("m", GOLD),
            desc(" memory "),
            key("t", GOLD),
            desc(" abtop"),
            sep(),
            key("⇧M", GOLD),
            desc(" expand/collapse "),
            key("?/H", CORNFLOWER_BLUE),
            desc(" help "),
            key("q", CORNFLOWER_BLUE),
            desc(" home"),
        ]);

        let menu_lines = vec![
            Line::from(line1_spans),
            Line::from(line2_spans),
            Line::from(line3_spans),
            Line::from(line4_spans),
        ];

        let menu = Paragraph::new(menu_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(SUBDUED_BORDER))
                    .style(Style::default().bg(PANEL_BG)),
            )
            .alignment(Alignment::Center);

        frame.render_widget(menu, area);
    }

    /// Wide-terminal legend: two columns separated by a vertical rule. The
    /// left column is everything that acts on a session/workspace; the right
    /// column is the panels, views, and navigation.
    fn render_menu_bar_two_col(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let key = |k: &'static str, color: Color| {
            Span::styled(k, Style::default().fg(color).add_modifier(Modifier::BOLD))
        };
        let desc = |d: &'static str| Span::styled(d, Style::default().fg(MUTED_GRAY));
        let red = Color::Rgb(230, 100, 100);

        // ── Left column: session & workspace actions ──────────────────────
        let left_lines = vec![
            Line::from(vec![
                key("n", GOLD),
                desc("ew  "),
                key("a", SELECTION_GREEN),
                desc("ttach  "),
                key("→", SELECTION_GREEN),
                desc(" Pane attach  "),
                key("1-9", SELECTION_GREEN),
                desc(" quick  "),
                key("Space", SELECTION_GREEN),
                desc(" select"),
            ]),
            Line::from(vec![
                key("r", SELECTION_GREEN),
                desc(" resume  "),
                key("d", red),
                desc("elete  "),
                key("⇧D", red),
                desc(" del-sel  "),
                key("s", GOLD),
                desc("tar"),
            ]),
            Line::from(vec![
                key("o", SELECTION_GREEN),
                desc(" editor  "),
                key("$", GOLD),
                desc(" shell  "),
                key("p", CORNFLOWER_BLUE),
                desc(" commit  "),
                key("F2", SELECTION_GREEN),
                desc(" rename"),
            ]),
            Line::from(vec![
                key("f", WARNING_ORANGE),
                desc(" refresh  "),
                key("⇧F", WARNING_ORANGE),
                desc(" filter  "),
                key("u", MUTED_GRAY),
                desc(" re-auth  "),
            ]),
        ];

        // ── Right column: panels, views & navigation ─────────────────────
        let mut row1 = Vec::new();
        row1.extend([
            key("i", GOLD),
            desc(" stats  "),
            key("w", GOLD),
            desc(" witr"),
        ]);
        let right_lines = vec![
            Line::from(row1),
            Line::from(vec![
                key("k", GOLD),
                desc(" skills  "),
                key("m", GOLD),
                desc(" memory  "),
                key("t", GOLD),
                desc(" abtop"),
            ]),
            Line::from(vec![
                key("g", CORNFLOWER_BLUE),
                desc("it  "),
                key("c", WARNING_ORANGE),
                desc("laude  "),
                key("⇧E", GOLD),
                desc("xpand"),
            ]),
            Line::from(vec![
                key("Tab", GOLD),
                desc(" focus  "),
                key("⇧M", GOLD),
                desc(" expand/collapse  "),
                key("?/H", CORNFLOWER_BLUE),
                desc(" help  "),
                key("q", CORNFLOWER_BLUE),
                desc(" home"),
            ]),
        ];

        // Outer frame carries the two section headers on its top border so
        // they cost no inner row — the four content rows mirror the stacked
        // legend's height exactly (menu area stays `Length(6)`).
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBDUED_BORDER))
            .style(Style::default().bg(PANEL_BG))
            .title(
                Line::from(vec![
                    Span::styled(" ⌨ ", Style::default().fg(GOLD)),
                    Span::styled(
                        "Session actions ",
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                    ),
                ])
                .alignment(Alignment::Left),
            )
            .title(
                Line::from(vec![Span::styled(
                    " Panels & views ",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                )])
                .alignment(Alignment::Right),
            );
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Split the body into [left | divider | right]. The columns are
        // centred within their halves so the legend spreads across the bar
        // instead of huddling in the middle the way the old stack did.
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new(left_lines).alignment(Alignment::Center),
            cols[0],
        );

        let divider: Vec<Line> = (0..inner.height)
            .map(|_| Line::from(Span::styled("│", Style::default().fg(SUBDUED_BORDER))))
            .collect();
        frame.render_widget(Paragraph::new(divider), cols[1]);

        frame.render_widget(
            Paragraph::new(right_lines).alignment(Alignment::Center),
            cols[2],
        );
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect, state: &AppState, ui: &mut UiState) {
        let mut status_spans: Vec<Span> = vec![];

        // Claude-chat popup toggle — a small global indicator. The
        // workspace / branch / session-status that used to live here were
        // removed: they duplicated the bottom "Session Info" line. This
        // top bar is now a dedicated, full-width live-quota line so both
        // providers fit (and degrade gracefully) instead of being squeezed
        // out by that duplicated content.
        if state.claude_chat.claude_chat_visible {
            status_spans.push(Span::styled("🗨️ ", Style::default().fg(SELECTION_GREEN)));
            status_spans.push(Span::styled("ON", Style::default().fg(SELECTION_GREEN)));
        } else {
            status_spans.push(Span::styled("🗨️ ", Style::default().fg(MUTED_GRAY)));
            status_spans.push(Span::styled("OFF", Style::default().fg(MUTED_GRAY)));
        }

        // Live OAuth quota (claude + codex). With the duplicated content
        // gone the widget gets nearly the whole bar; it abbreviate-then-
        // sheds to fit whatever columns remain (see `build_live_widget_spans`).
        // The unwired case still falls back to the red CTA.
        let area_inner_w = area.width.saturating_sub(2) as usize; // borders
        let existing_w: usize = status_spans.iter().map(|s| s.content.chars().count()).sum();
        const SEP_W: usize = 5; // "  │  "
        let avail = area_inner_w.saturating_sub(existing_w + SEP_W);
        let live_spans = build_live_status_spans(state, ui, avail);
        if !live_spans.is_empty() {
            status_spans.push(Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)));
            status_spans.extend(live_spans);
        }

        let status_line = if status_spans.is_empty() {
            Line::from(Span::styled(
                "Agents-in-a-Box - No active session",
                Style::default().fg(MUTED_GRAY),
            ))
        } else {
            Line::from(status_spans)
        };

        let status = Paragraph::new(status_line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📊 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Status",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .alignment(Alignment::Left);

        frame.render_widget(status, area);
    }

    /// Draw the notice stack in the top-right corner.
    ///
    /// Sized from the message rather than pinned at one line, because a notice
    /// that lives for a minute can afford to say what happened and what to do
    /// about it. The old fixed 50x3 box silently clipped anything past ~48
    /// columns, which is why the messages feeding it were written terse.
    ///
    /// Bounded so a long-lived notice cannot take the screen: at most
    /// [`MAX_VISIBLE_NOTIFICATIONS`] boxes, at most
    /// [`NOTIFICATION_MAX_TEXT_ROWS`] rows each, and never past the bottom of
    /// `area`. Every bound announces itself — dropped rows leave a marker in
    /// the box, dropped boxes are counted in the footer — because a surface
    /// that quietly shows less than it has is the one this replaced.
    ///
    /// Planned before it is drawn. The dismiss hint has to land on whatever
    /// element turns out to be last, and which element that is depends on how
    /// much room the boxes above it took.
    fn render_notifications(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let notifications = state.get_current_notifications();
        if notifications.is_empty() {
            return;
        }
        if area.width < NOTIFICATION_MIN_WIDTH + 2 || area.height < 4 {
            return; // Terminal too small to draw a box that says anything.
        }

        // Newest matter most, so an old notice is the one dropped — but keep
        // the survivors in arrival order, which is how they have always read.
        let shown = notifications.len().min(MAX_VISIBLE_NOTIFICATIONS);
        let visible = &notifications[notifications.len() - shown..];

        let width = NOTIFICATION_MAX_WIDTH
            .min(area.width.saturating_sub(4))
            .max(NOTIFICATION_MIN_WIDTH);
        let text_width = width.saturating_sub(2) as usize;
        let x = area.width.saturating_sub(width + 2);
        let bottom = area.height.saturating_sub(1);

        // ── Plan ────────────────────────────────────────────────────────────
        struct Planned<'a> {
            notification: &'a crate::app::state::Notification,
            rows: Vec<String>,
            y: u16,
            height: u16,
        }

        let mut planned: Vec<Planned> = Vec::new();
        let mut y = 1u16;
        for notification in visible {
            let room = bottom.saturating_sub(y);
            if room < 3 {
                break; // Not even a one-row box fits below what came before.
            }
            // Shrink to the room actually left rather than skipping the box:
            // a notice nobody can see is worse than a notice cut short and
            // saying so.
            let max_rows = NOTIFICATION_MAX_TEXT_ROWS.min(room as usize - 2);
            let (rows, _dropped) = clamp_notice_rows(
                wrap_notification(&notification.message, notice_icon(notification), text_width),
                max_rows,
                text_width,
            );
            let height = rows.len() as u16 + 2;
            planned.push(Planned {
                notification,
                rows,
                y,
                height,
            });
            y += height;
        }
        if planned.is_empty() {
            return;
        }

        // Boxes that never made it onto the screen, either past the visible
        // cap or past the bottom of the frame.
        let suppressed = notifications.len() - planned.len();
        // A dedicated line is clearer than a border label, so use one when
        // there is room. When there is not, the footer carries the count.
        let more_line_fits = suppressed > 0 && y + 3 <= bottom;
        let footer_suppressed = if more_line_fits { 0 } else { suppressed };
        let footer = notice_footer(footer_suppressed, width.saturating_sub(2) as usize);

        // ── Draw ────────────────────────────────────────────────────────────
        let last_index = planned.len() - 1;
        for (index, item) in planned.iter().enumerate() {
            let color = notice_color(item.notification);
            let mut block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(color))
                .style(Style::default().bg(PANEL_BG));
            // The hint belongs on the control it operates, and it must be on
            // SOMETHING: this is the only place the chord is advertised while
            // a notice is up.
            if index == last_index && !more_line_fits {
                block = block.title_bottom(
                    Line::from(Span::styled(
                        footer.clone(),
                        Style::default().fg(MUTED_GRAY),
                    ))
                    .right_aligned(),
                );
            }

            let lines: Vec<Line> = item
                .rows
                .iter()
                .map(|row| Line::from(Span::styled(row.clone(), Style::default().fg(color))))
                .collect();

            frame.render_widget(
                Paragraph::new(lines).block(block),
                Rect {
                    x,
                    y: item.y,
                    width,
                    height: item.height,
                },
            );
        }

        if more_line_fits {
            let more = Paragraph::new(Line::from(Span::styled(
                format!("+{suppressed} more — see the log ({NOTIFICATION_LOG_HINT})"),
                Style::default().fg(MUTED_GRAY),
            )))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(SUBDUED_BORDER))
                    .style(Style::default().bg(PANEL_BG))
                    .title_bottom(
                        Line::from(Span::styled(footer, Style::default().fg(MUTED_GRAY)))
                            .right_aligned(),
                    ),
            );
            frame.render_widget(
                more,
                Rect {
                    x,
                    y,
                    width,
                    height: 3,
                },
            );
        }
    }

    fn render_quick_commit_dialog(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Clear the ENTIRE area first (proper modal behavior)
        frame.render_widget(Clear, area);

        // Create a centered dialog area (60% width, 25% height for better visibility)
        let dialog_area = centered_rect(60, 25, area);

        // Render outer container with proper TUI styling
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(Line::from(vec![
                Span::styled(" 📋 ", Style::default().fg(GOLD)),
                Span::styled(
                    "Git Commit ",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]));
        frame.render_widget(outer_block, dialog_area);

        // Calculate inner area (inside the border)
        let inner_area = Rect {
            x: dialog_area.x + 1,
            y: dialog_area.y + 1,
            width: dialog_area.width.saturating_sub(2),
            height: dialog_area.height.saturating_sub(2),
        };

        // Create the inner layout
        let inner_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Input field
                Constraint::Length(2), // Instructions
            ])
            .split(inner_area);

        // Render input field with block cursor
        let empty_string = String::new();
        let commit_message = state.git_view.quick_commit_message.as_ref().unwrap_or(&empty_string);

        // Create spans with cursor visualization
        let (before_cursor, after_cursor) =
            commit_message.split_at(state.git_view.quick_commit_cursor.min(commit_message.len()));

        let input_line = Line::from(vec![
            Span::styled(before_cursor, Style::default().fg(SOFT_WHITE)),
            Span::styled("█", Style::default().fg(SELECTION_GREEN)),
            Span::styled(after_cursor, Style::default().fg(SOFT_WHITE)),
        ]);

        let input_paragraph = Paragraph::new(input_line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(SELECTION_GREEN))
                .style(Style::default().bg(DARK_BG))
                .title(Line::from(vec![
                    Span::styled(" ✏️ ", Style::default().fg(GOLD)),
                    Span::styled(
                        "Commit Message ",
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                    ),
                ])),
        );
        frame.render_widget(input_paragraph, inner_layout[0]);

        // Render help bar (gold keys + muted descriptions)
        let help_bar = Paragraph::new(Line::from(vec![
            Span::styled(
                " Enter",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Commit & Push ", Style::default().fg(MUTED_GRAY)),
            Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
            Span::styled(
                " Esc",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Cancel ", Style::default().fg(MUTED_GRAY)),
        ]))
        .alignment(Alignment::Center)
        .style(Style::default().bg(PANEL_BG));
        frame.render_widget(help_bar, inner_layout[1]);
    }
}

impl Default for LayoutComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod menu_bar_render_tests {
    use super::*;
    use crate::app::state::AppState;
    use ratatui::{Terminal, backend::TestBackend};

    fn painted_menu_bar(width: u16, shown: bool) -> String {
        let mut state = AppState::default();
        state.config.app_config.ui_preferences.show_session_menu_bar = shown;
        let component = LayoutComponent::new();
        let height = session_menu_bar_height(shown);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| component.render_menu_bar(frame, frame.area(), &state))
            .expect("draw menu bar");

        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn expanded_menu_bar_advertises_shift_m_at_narrow_and_wide_widths() {
        for width in [80, 200] {
            let painted = painted_menu_bar(width, true);
            assert!(
                painted.contains("⇧M expand/collapse"),
                "Shift+M keymap control missing at {width} columns:\n{painted}"
            );
        }
    }

    #[test]
    fn collapsed_menu_bar_names_shift_m_action() {
        let painted = painted_menu_bar(80, false);
        assert!(
            painted.contains("⇧M expand/collapse"),
            "collapsed keymap hint does not name Shift+M action:\n{painted}"
        );
    }
}

/// Build the compact "live OAuth window" spans appended to the top status
/// bar. Returns an empty vec when nothing should render (statusline
/// unwired AND user declined, or status detection failed).
///
/// The settings.json read goes through [`UiState::statusline_status`]
/// so the top bar's 30-60Hz redraws don't translate into 30-60Hz
/// filesystem reads.
pub fn build_live_status_spans(
    state: &AppState,
    ui: &mut UiState,
    max_width: usize,
) -> Vec<Span<'static>> {
    use crate::cli::statusline_install::StatuslineStatus;
    use crate::config::StatuslineDecision;
    use crate::models::live_window::Source;

    let status = ui.statusline_status(state);
    let decision = state.config.app_config.ui_preferences.statusline_decision;

    // Trust the cache: if Tier1 data is flowing — whether it came from
    // our own command in settings.json (Configured) or from a user's
    // custom statusline that side-channels via `ainb claudecode statusline
    // --cache-only` (Other) — show the live widget. The CTA is for the
    // genuinely-unwired case only.
    //
    // The snapshot is maintained by a background tokio poller so this
    // hot path never touches the filesystem itself.
    let live = state.live_window_watcher.snapshot();
    // Render the widget when Claude Tier1 data is flowing OR Codex usage is
    // present — Codex is overlaid independently (separate cache, its own
    // poller), so a user who runs Codex but never wired the Claude
    // statusline still sees their Codex burn instead of the CTA.
    let has_codex = live.codex_five_hour_pct.is_some() || live.codex_seven_day_pct.is_some();
    if live.source == Source::Tier1Cache || has_codex {
        return build_live_widget_spans(&live, max_width);
    }

    match status {
        Some(StatuslineStatus::Configured) => {
            // Wired our command but no fresh data yet — render nothing
            // rather than misleading "0%" placeholders.
            Vec::new()
        }
        Some(StatuslineStatus::NotConfigured | StatuslineStatus::Other(_))
            if decision != StatuslineDecision::Declined =>
        {
            // Vanish (don't clip) the CTA when it can't fit — parity with
            // the quota widget's shed behaviour and with the old width gate.
            let cta = build_cta_spans();
            if spans_width(&cta) <= max_width {
                cta
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Detail level for the live quota widget, richest → poorest. The renderer
/// picks the richest level whose rendered width fits the available columns
/// (abbreviate-then-shed): drop the reset dates, then abbreviate the labels
/// + weekly into `cl 81%/24%`, then shed the weekly entirely to `cl81%`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaDetail {
    /// `claude 5h 81% ↻ Jun 15 16:50 · wk 24% ↻ Jun 15 18:00`
    FullDated,
    /// `claude 5h 81% · wk 24%`
    Full,
    /// `cl 81%/24%` (5h%/wk%, abbreviated provider label)
    Abbrev,
    /// `cl81%` (5h only — last resort, both providers still visible)
    Tiny,
}

/// All detail levels, richest → poorest.
const QUOTA_DETAIL_LADDER: [QuotaDetail; 4] = [
    QuotaDetail::FullDated,
    QuotaDetail::Full,
    QuotaDetail::Abbrev,
    QuotaDetail::Tiny,
];

/// Total display width (columns) of a span list.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Best-fit live quota spans for `max_width` columns. Tries each detail
/// level richest → poorest and returns the first that fits; if even the
/// poorest overflows it is returned anyway (ratatui clips — showing a
/// clipped `cl81% cx14%` beats a blank bar). Empty when there's no data.
fn build_live_widget_spans(
    live: &crate::models::live_window::LiveWindow,
    max_width: usize,
) -> Vec<Span<'static>> {
    let mut poorest = Vec::new();
    for detail in QUOTA_DETAIL_LADDER {
        let spans = quota_spans(live, detail);
        if spans.is_empty() {
            return spans; // no data at all → nothing to render
        }
        if spans_width(&spans) <= max_width {
            return spans;
        }
        poorest = spans;
    }
    poorest
}

/// Build both provider clusters (`claude …   codex …`) at one detail level.
fn quota_spans(
    live: &crate::models::live_window::LiveWindow,
    detail: QuotaDetail,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    push_provider(
        &mut out,
        ("claude", "cl"),
        live.five_hour_pct,
        live.five_hour_resets_at,
        live.seven_day_pct,
        live.seven_day_resets_at,
        detail,
    );
    push_provider(
        &mut out,
        ("codex", "cx"),
        live.codex_five_hour_pct,
        live.codex_five_hour_resets_at,
        live.codex_seven_day_pct,
        live.codex_seven_day_resets_at,
        detail,
    );
    out
}

/// Render one provider cluster at `detail` onto `out`, separated from a
/// preceding cluster by a gap. No-op when both windows are absent
/// (hide-on-fail). `labels` is `(full, abbreviated)`.
#[allow(clippy::too_many_arguments)]
fn push_provider(
    out: &mut Vec<Span<'static>>,
    labels: (&str, &str),
    five_pct: Option<u8>,
    five_reset: Option<chrono::DateTime<chrono::Utc>>,
    seven_pct: Option<u8>,
    seven_reset: Option<chrono::DateTime<chrono::Utc>>,
    detail: QuotaDetail,
) {
    if five_pct.is_none() && seven_pct.is_none() {
        return;
    }
    let (full_label, abbr_label) = labels;
    let label_style = Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD);
    if !out.is_empty() {
        let gap = match detail {
            QuotaDetail::Abbrev | QuotaDetail::Tiny => "  ",
            _ => "   ",
        };
        out.push(Span::styled(gap, Style::default()));
    }

    match detail {
        QuotaDetail::FullDated | QuotaDetail::Full => {
            let show_reset = detail == QuotaDetail::FullDated;
            out.push(Span::styled(format!("{full_label} "), label_style));
            let mut first = true;
            push_quota_window(
                out,
                "5h",
                five_pct,
                bar_color_5h,
                five_reset,
                show_reset,
                &mut first,
            );
            push_quota_window(
                out,
                "wk",
                seven_pct,
                bar_color_7d,
                seven_reset,
                show_reset,
                &mut first,
            );
        }
        QuotaDetail::Abbrev => {
            // `cl 81%/24%`
            out.push(Span::styled(format!("{abbr_label} "), label_style));
            if let Some(p) = five_pct {
                out.push(Span::styled(
                    format!("{p}%"),
                    Style::default().fg(bar_color_5h(p)).add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(p) = seven_pct {
                if five_pct.is_some() {
                    out.push(Span::styled("/", Style::default().fg(MUTED_GRAY)));
                }
                out.push(Span::styled(
                    format!("{p}%"),
                    Style::default().fg(bar_color_7d(p)).add_modifier(Modifier::BOLD),
                ));
            }
        }
        QuotaDetail::Tiny => {
            // `cl81%` — 5h only (fall back to wk if 5h is absent) so the
            // provider still shows a number in the tightest space.
            out.push(Span::styled(abbr_label.to_string(), label_style));
            let (pct, color): (u8, fn(u8) -> Color) = match (five_pct, seven_pct) {
                (Some(p), _) => (p, bar_color_5h),
                (None, Some(p)) => (p, bar_color_7d),
                (None, None) => return,
            };
            out.push(Span::styled(
                format!("{pct}%"),
                Style::default().fg(color(pct)).add_modifier(Modifier::BOLD),
            ));
        }
    }
}

/// Push one window — `5h NN%` (+ ` ↻ <reset>` when `show_reset`) — within a
/// provider cluster, with a ` · ` separator before all but the first
/// window. No-op when `pct` is `None`.
#[allow(clippy::too_many_arguments)]
fn push_quota_window(
    out: &mut Vec<Span<'static>>,
    label: &str,
    pct: Option<u8>,
    color: fn(u8) -> Color,
    reset: Option<chrono::DateTime<chrono::Utc>>,
    show_reset: bool,
    first: &mut bool,
) {
    let Some(pct) = pct else {
        return;
    };
    if !*first {
        out.push(Span::styled(" · ", Style::default().fg(SUBDUED_BORDER)));
    }
    *first = false;
    out.push(Span::styled(
        format!("{label} "),
        Style::default().fg(MUTED_GRAY),
    ));
    out.push(Span::styled(
        format!("{pct}%"),
        Style::default().fg(color(pct)).add_modifier(Modifier::BOLD),
    ));
    if show_reset {
        if let Some(reset) = reset {
            out.push(Span::styled(
                format!(" ↻ {}", format_reset_at(reset)),
                Style::default().fg(MUTED_GRAY),
            ));
        }
    }
}

fn build_cta_spans() -> Vec<Span<'static>> {
    let red = Color::Rgb(230, 100, 100);
    vec![
        Span::styled("⚠ ", Style::default().fg(red).add_modifier(Modifier::BOLD)),
        Span::styled("Live Claude Code usage off", Style::default().fg(red)),
        Span::styled(" · press W to enable", Style::default().fg(MUTED_GRAY)),
    ]
}

fn bar_color_5h(pct: u8) -> Color {
    if pct >= 85 {
        Color::Rgb(230, 100, 100)
    } else if pct >= 60 {
        WARNING_ORANGE
    } else {
        SELECTION_GREEN
    }
}

fn bar_color_7d(pct: u8) -> Color {
    if pct >= 90 {
        Color::Rgb(230, 100, 100)
    } else if pct >= 70 {
        WARNING_ORANGE
    } else {
        SELECTION_GREEN
    }
}

/// Format an absolute quota-reset instant for the top bar, in the
/// viewer's local timezone: e.g. `Jun 8 05:00`. Date + time so both the
/// 5-hour (same/next-day) and weekly (days-out) windows read unambiguously.
fn format_reset_at(reset: chrono::DateTime<chrono::Utc>) -> String {
    reset.with_timezone(&chrono::Local).format("%b %-d %H:%M").to_string()
}

#[cfg(test)]
mod live_widget_tests {
    use super::*;
    use crate::models::live_window::{LiveWindow, Source};
    use std::time::Duration;

    fn flatten(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn cta_spans_contain_warning_and_w_shortcut_hint() {
        let spans = build_cta_spans();
        let text = flatten(&spans);
        assert!(text.contains("Live Claude Code usage off"));
        // The CTA points at the global `W` shortcut so the keystroke is
        // discoverable without navigating into Stats first.
        assert!(text.contains("press W"));
        assert!(!text.contains("Stats"), "stale Stats hint must be gone");
    }

    #[test]
    fn live_widget_renders_5h_7d_and_reset() {
        use chrono::{TimeZone, Utc};
        let live = LiveWindow {
            five_hour_pct: Some(40),
            seven_day_pct: Some(8),
            today_cost_usd: Some(1.5),
            resets_in: Some(Duration::from_secs(2 * 3600)),
            five_hour_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 8, 5, 0, 0).unwrap()),
            seven_day_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 12, 5, 0, 0).unwrap()),
            context_pct: None,
            model: None,
            source: Source::Tier1Cache,
            ..Default::default()
        };
        let spans = build_live_widget_spans(&live, 1000);
        let text = flatten(&spans);
        assert!(text.contains("claude"), "provider label present: {text}");
        assert!(text.contains("5h"));
        assert!(text.contains("40%"));
        assert!(text.contains("wk"));
        assert!(text.contains("8%"));
        // today_cost_usd is in the cache but intentionally not rendered —
        // it's a single session's lifetime cost, not today's total.
        assert!(!text.contains("$"));
        assert!(!text.contains("today"));
        // The combined "⏱ Xh Ym" countdown is gone; each window carries its
        // own absolute reset stamp prefixed by ↻ (local-tz date+time).
        assert!(!text.contains("⏱"));
        assert_eq!(text.matches('↻').count(), 2, "one reset stamp per window");
    }

    #[test]
    fn live_widget_omits_missing_fields() {
        let live = LiveWindow {
            five_hour_pct: Some(20),
            seven_day_pct: None,
            today_cost_usd: None,
            resets_in: None,
            five_hour_resets_at: None,
            seven_day_resets_at: None,
            context_pct: None,
            model: None,
            source: Source::Tier1Cache,
            ..Default::default()
        };
        let spans = build_live_widget_spans(&live, 1000);
        let text = flatten(&spans);
        assert!(text.contains("claude"));
        assert!(text.contains("5h"));
        assert!(!text.contains("wk"));
        assert!(!text.contains("$"));
        assert!(!text.contains("⏱"));
        assert!(!text.contains("↻"), "no reset stamp when instants absent");
    }

    #[test]
    fn live_widget_renders_codex_windows_next_to_claude() {
        use chrono::{TimeZone, Utc};
        let live = LiveWindow {
            five_hour_pct: Some(40),
            seven_day_pct: Some(8),
            source: Source::Tier1Cache,
            codex_five_hour_pct: Some(10),
            codex_five_hour_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 15, 0, 21, 0).unwrap()),
            codex_seven_day_pct: Some(44),
            codex_seven_day_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 18, 13, 0, 0).unwrap()),
            ..Default::default()
        };
        let text = flatten(&build_live_widget_spans(&live, 1000));
        // Both provider clusters render: `claude 5h 40% · wk 8%   codex …`.
        assert!(text.contains("claude"), "claude cluster present: {text}");
        assert!(text.contains("40%"));
        assert!(text.contains("8%"));
        assert!(text.contains("codex"), "codex cluster present: {text}");
        assert!(text.contains("10%"));
        assert!(text.contains("44%"));
        // Claude leads, Codex follows (overlaid second).
        assert!(text.starts_with("claude"), "claude leads: {text}");
        let (claude_at, codex_at) = (text.find("claude").unwrap(), text.find("codex").unwrap());
        assert!(claude_at < codex_at, "claude before codex: {text}");
        // Only the two Codex windows carry reset instants here → two ↻.
        assert_eq!(text.matches('↻').count(), 2);
    }

    #[test]
    fn live_widget_renders_codex_only_when_claude_absent() {
        // User runs Codex but never wired the Claude statusline.
        let live = LiveWindow {
            source: Source::None,
            codex_five_hour_pct: Some(10),
            codex_seven_day_pct: Some(44),
            ..Default::default()
        };
        let text = flatten(&build_live_widget_spans(&live, 1000));
        // Codex is the first (and only) cluster — no Claude cluster precedes it.
        assert!(
            text.starts_with("codex"),
            "codex leads when Claude absent: {text}"
        );
        assert!(!text.contains("claude"));
    }

    #[test]
    fn live_widget_omits_codex_when_absent() {
        let live = LiveWindow {
            five_hour_pct: Some(40),
            source: Source::Tier1Cache,
            ..Default::default()
        };
        let text = flatten(&build_live_widget_spans(&live, 1000));
        assert!(text.contains("claude"));
        assert!(!text.contains("codex"));
    }

    /// Both providers, all four windows + resets, for the degradation tests.
    fn both_providers_live() -> LiveWindow {
        use chrono::{TimeZone, Utc};
        LiveWindow {
            five_hour_pct: Some(40),
            seven_day_pct: Some(8),
            five_hour_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 8, 5, 0, 0).unwrap()),
            seven_day_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 12, 5, 0, 0).unwrap()),
            source: Source::Tier1Cache,
            codex_five_hour_pct: Some(10),
            codex_five_hour_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 15, 0, 21, 0).unwrap()),
            codex_seven_day_pct: Some(44),
            codex_seven_day_resets_at: Some(Utc.with_ymd_and_hms(2026, 6, 18, 13, 0, 0).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn degrade_full_dated_when_room() {
        // Wide → richest: full labels, all four windows, four ↻ reset stamps.
        let text = flatten(&build_live_widget_spans(&both_providers_live(), 1000));
        assert!(text.contains("claude") && text.contains("codex"));
        assert!(text.contains("5h ") && text.contains("wk "));
        assert_eq!(
            text.matches('↻').count(),
            4,
            "all four reset stamps: {text}"
        );
    }

    #[test]
    fn degrade_drops_dates_first() {
        // 60 cols fits the no-dates form (~45) but not the dated one (~100+).
        let text = flatten(&build_live_widget_spans(&both_providers_live(), 60));
        assert!(text.contains("claude") && text.contains("codex"));
        assert!(text.contains("40%") && text.contains("8%"));
        assert!(text.contains("wk "), "weekly still shown: {text}");
        assert!(!text.contains('↻'), "reset dates dropped first: {text}");
    }

    #[test]
    fn degrade_abbreviates_then() {
        // 30 cols fits the abbreviated `cl 40%/8%  cx 10%/44%` (~21) only.
        let text = flatten(&build_live_widget_spans(&both_providers_live(), 30));
        assert!(text.contains("cl ") && text.contains("cx "));
        assert!(!text.contains("claude") && !text.contains("codex"));
        assert!(
            text.contains("40%") && text.contains("8%"),
            "5h+wk kept: {text}"
        );
        assert!(text.contains('/'), "abbreviated 5h/wk: {text}");
        assert!(!text.contains('↻'));
    }

    #[test]
    fn degrade_tiny_keeps_both_providers() {
        // 15 cols fits only `cl40% cx10%` (~12): 5h-only, both providers.
        let text = flatten(&build_live_widget_spans(&both_providers_live(), 15));
        assert!(text.contains("cl40%"), "claude 5h kept: {text}");
        assert!(text.contains("cx10%"), "codex 5h kept: {text}");
        assert!(!text.contains('/'), "weekly shed: {text}");
        assert!(!text.contains("wk"));
    }

    #[test]
    fn degrade_tiny_is_floor_even_if_overflowing() {
        // Absurdly narrow → still return the Tiny floor (clipped), not blank.
        let text = flatten(&build_live_widget_spans(&both_providers_live(), 1));
        assert!(!text.is_empty(), "floor renders rather than blanking");
        assert!(text.contains("cl40%"));
    }

    #[test]
    fn degrade_empty_when_no_data() {
        let text = flatten(&build_live_widget_spans(&LiveWindow::empty(), 1000));
        assert!(text.is_empty(), "no data → nothing, regardless of width");
    }

    #[test]
    fn bar_color_5h_thresholds() {
        assert_eq!(bar_color_5h(0), SELECTION_GREEN);
        assert_eq!(bar_color_5h(59), SELECTION_GREEN);
        assert_eq!(bar_color_5h(60), WARNING_ORANGE);
        assert_eq!(bar_color_5h(84), WARNING_ORANGE);
        assert_eq!(bar_color_5h(85), Color::Rgb(230, 100, 100));
    }

    #[test]
    fn bar_color_7d_thresholds() {
        assert_eq!(bar_color_7d(0), SELECTION_GREEN);
        assert_eq!(bar_color_7d(69), SELECTION_GREEN);
        assert_eq!(bar_color_7d(70), WARNING_ORANGE);
        assert_eq!(bar_color_7d(89), WARNING_ORANGE);
        assert_eq!(bar_color_7d(90), Color::Rgb(230, 100, 100));
    }

    #[test]
    fn format_reset_at_is_local_date_and_time() {
        use chrono::{TimeZone, Utc};
        let reset = Utc.with_ymd_and_hms(2026, 6, 8, 5, 0, 0).unwrap();
        let s = format_reset_at(reset);
        // Host-local tz varies, but the rendered shape is fixed:
        // "<Mon> <D> <HH>:<MM>" — three space-separated parts.
        let parts: Vec<&str> = s.split(' ').collect();
        assert_eq!(parts.len(), 3, "expected `Mon D HH:MM`, got {s:?}");
        // Month: 3-letter English abbreviation (chrono's %b, locale-independent).
        assert_eq!(parts[0].len(), 3, "month abbrev: {s:?}");
        assert!(
            parts[0].chars().all(|c| c.is_ascii_alphabetic()),
            "month abbrev: {s:?}"
        );
        // Day: 1–2 digits, no zero-pad (%-d).
        assert!(
            (1..=2).contains(&parts[1].len()) && parts[1].chars().all(|c| c.is_ascii_digit()),
            "day numeral: {s:?}"
        );
        // Clock: zero-padded HH:MM.
        let clock: Vec<&str> = parts[2].split(':').collect();
        assert_eq!(clock.len(), 2, "HH:MM: {s:?}");
        assert!(
            clock[0].len() == 2 && clock[0].bytes().all(|b| b.is_ascii_digit()),
            "HH: {s:?}"
        );
        assert!(
            clock[1].len() == 2 && clock[1].bytes().all(|b| b.is_ascii_digit()),
            "MM: {s:?}"
        );
    }

    // format_reset_at runs in the per-frame render path, so it must never
    // panic regardless of the instant handed to it. Exercise the extremes
    // of chrono's representable range plus the epoch boundary; the test
    // passing at all (no panic/unwind) is the assertion.
    #[test]
    fn format_reset_at_never_panics_on_extreme_instants() {
        use chrono::{TimeZone, Utc};
        let cases = [
            Utc.timestamp_opt(0, 0).unwrap(),
            Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(9999, 12, 31, 23, 59, 59).unwrap(),
        ];
        for c in cases {
            assert!(!format_reset_at(c).is_empty(), "non-empty for {c}");
        }
    }
}

/// Helper function to create a centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod interactive_embed_size_tests {
    use super::interactive_embed_size;

    #[test]
    fn matches_the_interactive_layout_interior() {
        // 120x30 terminal: chrome = 3+3+6 plus the pane border (2) → rows 16.
        // Cols follow the user's CURRENT sidebar: the default 40-col sidebar
        // plus the border (2) → 78; pre-collapsed to the 5-col rail → 113.
        // Must equal what the first interactive frame resizes the embed to
        // (the tripwire drives the real render path against this).
        assert_eq!(interactive_embed_size(120, 30, 40, true), (16, 78));
        assert_eq!(interactive_embed_size(120, 30, 5, true), (16, 113));
        assert_eq!(interactive_embed_size(80, 24, 5, true), (10, 73));
        assert_eq!(interactive_embed_size(120, 30, 40, false), (21, 78));
    }

    #[test]
    fn never_returns_zero_cells() {
        assert_eq!(interactive_embed_size(0, 0, 5, true), (1, 1));
        assert_eq!(interactive_embed_size(7, 14, 40, true), (1, 1));
    }
}

#[cfg(test)]
mod notification_layout_tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// Every row a notice draws must fit inside the box, or the border is
    /// overwritten and the message reads as corrupted rather than long.
    #[test]
    fn no_wrapped_row_is_wider_than_the_box() {
        let width = 40;
        let message = "No session selected to attach. Pick a session row with the arrow keys \
                       and press a again; press n to start one.";
        for row in wrap_notification(message, "✗ ", width) {
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= width,
                "row overflows the {width}-cell box: {row:?}"
            );
        }
    }

    /// Cell width, not `char` count. This codebase puts emoji in plenty of
    /// notices and each one is two cells wide; counting `char`s overflows the
    /// border by exactly as many emoji as the row holds.
    #[test]
    fn an_emoji_counts_as_the_two_cells_it_paints() {
        let width = 20;
        let message = "⚠️ 🔥 🔥 🔥 🔥 🔥 🔥 🔥 🔥 🔥 🔥 🔥";
        for row in wrap_notification(message, "⚠ ", width) {
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= width,
                "emoji row overflows the {width}-cell box: {row:?}"
            );
        }
    }

    /// A detailed message is what the longer lifetime buys, so it must survive
    /// the trip to the screen instead of being clipped to one line.
    #[test]
    fn a_detailed_message_wraps_instead_of_being_clipped() {
        let message = "Codex bridge conflict. Restart Ainb, then retry. \
                       Cause: Codex app-server WebSocket handshake failed: invalid token";
        let rows = wrap_notification(message, "✗ ", 60);
        assert!(rows.len() > 1, "a long message must occupy several rows");
        let rejoined = rows.join(" ");
        for word in ["Restart", "Cause:", "handshake", "token"] {
            assert!(
                rejoined.contains(word),
                "wrapping dropped {word:?} from the notice: {rejoined:?}"
            );
        }
    }

    /// The first row carries the level icon and the rest line up under the
    /// text, so a three-row failure reads as one message.
    #[test]
    fn continuation_rows_align_under_the_first() {
        let rows = wrap_notification("one two three four five six seven eight", "✗ ", 14);
        assert!(rows[0].starts_with("✗ "));
        for row in &rows[1..] {
            assert!(
                row.starts_with("  "),
                "continuation row not indented: {row:?}"
            );
        }
    }

    /// An empty message must still produce a row, or the box collapses to its
    /// two borders and paints as a stray rectangle.
    #[test]
    fn an_empty_message_still_draws_one_row() {
        assert_eq!(wrap_notification("", "ℹ ", 20).len(), 1);
    }

    /// A token wider than the box is BROKEN, not run past the border.
    ///
    /// The previous version of this test asserted only the row count, which
    /// stayed true while the row itself overflowed and the `Paragraph` clipped
    /// it — losing the rest of the message. Assert the width.
    #[test]
    fn a_word_wider_than_the_box_is_broken_to_fit() {
        let width = 20;
        let rows = wrap_notification(&"x".repeat(200), "✗ ", width);
        assert!(
            rows.len() > 1,
            "a 200-cell token cannot occupy one 20-cell row"
        );
        for row in &rows {
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= width,
                "row runs past the border and will be clipped: {row:?}"
            );
        }
    }

    /// And breaking it loses nothing. This is the case that matters: ainb
    /// failures name worktree paths, and a clipped path is a message whose
    /// second half never reaches the screen.
    #[test]
    fn a_worktree_path_survives_the_wrap_intact() {
        let path =
            "/Users/dev/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-improve--17a4b207";
        let message = format!("Failed to attach to '{path}': no such session.");
        let rows = wrap_notification(&message, "✗ ", 40);
        for row in &rows {
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= 40,
                "row overflows: {row:?}"
            );
        }
        let rejoined: String = rows
            .iter()
            .map(|r| r.trim_start_matches(['✗', '\u{a0}', ' ']))
            .collect::<Vec<_>>()
            .concat()
            .replace(' ', "");
        assert!(
            rejoined.contains(&path.replace(' ', "")),
            "the path did not survive wrapping: {rejoined}"
        );
    }

    /// A one-cell budget must terminate, not loop forever on a two-cell glyph.
    #[test]
    fn a_glyph_wider_than_the_budget_still_terminates() {
        let rows = wrap_notification("🔥🔥🔥", "", 1);
        assert_eq!(rows.len(), 3, "each glyph takes its own row: {rows:?}");
    }

    /// Rows past the cap are marked, not silently dropped. A message that ends
    /// mid-sentence with no marker is what this whole surface replaced.
    #[test]
    fn dropped_rows_leave_a_marker_that_names_the_count() {
        let rows: Vec<String> = (0..20).map(|i| format!("row {i}")).collect();
        let (kept, dropped) = clamp_notice_rows(rows, 6, 40);
        assert_eq!(kept.len(), 6, "the cap is honoured");
        assert_eq!(dropped, 15, "fifteen rows went, and the caller is told so");
        assert!(
            kept.last().unwrap().contains("15"),
            "the marker must name what is missing: {:?}",
            kept.last()
        );
        assert!(kept.last().unwrap().starts_with('…'));
    }

    #[test]
    fn rows_within_the_cap_are_left_alone() {
        let rows: Vec<String> = (0..4).map(|i| format!("row {i}")).collect();
        let (kept, dropped) = clamp_notice_rows(rows.clone(), 6, 40);
        assert_eq!(kept, rows);
        assert_eq!(dropped, 0);
    }

    /// The marker itself must fit the box it is marking.
    #[test]
    fn the_marker_shrinks_to_the_width_it_has() {
        for width in 1..40usize {
            let marker = more_lines_marker(12, width);
            assert!(
                UnicodeWidthStr::width(marker.as_str()) <= width.max(1),
                "marker overflows a {width}-cell row: {marker:?}"
            );
        }
    }

    /// The dismiss hint is the one part of the footer that cannot be dropped:
    /// it is the only advertisement of the chord while a notice is up.
    #[test]
    fn the_footer_keeps_the_hint_at_every_width() {
        for width in 1..70usize {
            for suppressed in [0usize, 3, 12] {
                let footer = notice_footer(suppressed, width);
                assert!(
                    footer.contains(NOTIFICATION_DISMISS_HINT),
                    "the hint vanished at width {width} with {suppressed} suppressed: {footer:?}"
                );
            }
        }
    }

    /// And it fits, once the border is wide enough to hold it at all.
    #[test]
    fn the_footer_fits_the_border_it_is_drawn_on() {
        let usable = (NOTIFICATION_MIN_WIDTH - 2) as usize;
        for suppressed in [0usize, 3, 12] {
            let footer = notice_footer(suppressed, usable);
            assert!(
                UnicodeWidthStr::width(footer.as_str()) <= usable,
                "footer overflows the narrowest box: {footer:?}"
            );
        }
    }
}

/// The notice stack rendered against a real backend.
///
/// The wrap maths and the footer text are unit tested above; what those cannot
/// see is which element ends up LAST on a given screen, and the dismiss hint
/// rides on whichever one that is. On a short terminal the boxes are cut by the
/// bottom of the frame and the "+N more" line has nowhere to go, and that is
/// precisely where the hint used to disappear — from the one surface whose
/// point is that it can be dismissed, by a chord nobody guesses.
#[cfg(test)]
mod notification_render_tests {
    use super::*;
    use crate::app::state::AppState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(width: u16, height: u16, messages: &[&str]) -> String {
        let mut state = AppState::default();
        for message in messages {
            state.add_error_notification((*message).to_string());
        }
        let component = LayoutComponent::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| component.render_notifications(frame, frame.area(), &state))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|col| buffer[(col, row)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_single_notice_carries_the_hint() {
        let painted = screen(120, 20, &["the daemon is not answering"]);
        assert!(
            painted.contains(NOTIFICATION_DISMISS_HINT),
            "no dismiss hint on screen:\n{painted}"
        );
        assert!(painted.contains("daemon is not answering"), "{painted}");
    }

    /// Boxes cut by the bottom of the frame, with no room left for the "+N
    /// more" line either. The hint must still be somewhere.
    #[test]
    fn a_short_terminal_still_advertises_the_chord() {
        let painted = screen(
            120,
            8,
            &[
                "first failure",
                "second failure",
                "third failure",
                "fourth failure",
            ],
        );
        assert!(
            painted.contains(NOTIFICATION_DISMISS_HINT),
            "the dismiss hint vanished on a short terminal:\n{painted}"
        );
    }

    /// And what it could not show, it counts.
    #[test]
    fn what_does_not_fit_is_counted_not_dropped_in_silence() {
        let painted = screen(
            120,
            8,
            &[
                "first failure",
                "second failure",
                "third failure",
                "fourth failure",
            ],
        );
        assert!(
            painted.contains("more"),
            "suppressed notices were not counted anywhere:\n{painted}"
        );
    }

    /// A message longer than the box's row budget is cut with a marker naming
    /// how much is missing, never trailing off mid-sentence.
    #[test]
    fn an_over_long_message_says_how_much_it_is_hiding() {
        let long = format!("failure: {}", "detail ".repeat(120));
        let painted = screen(120, 30, &[&long]);
        assert!(
            painted.contains("more lines"),
            "a truncated message must say so:\n{painted}"
        );
    }

    /// No row may run past the border. This is what a clipped path looked like
    /// before the wrap learned to break a token.
    #[test]
    fn a_long_path_does_not_run_past_the_border() {
        let path =
            "/Users/dev/.agents-in-a-box/worktrees/by-name/agents-in-a-box--f-improve--17a4b207";
        let painted = screen(120, 20, &[&format!("Failed to attach to '{path}': gone.")]);
        // The right border column of every notice row must still be a border
        // glyph. A row that overflowed would have overwritten it with text.
        for line in painted.lines().filter(|l| l.contains('│') || l.contains('╮')) {
            let trimmed = line.trim_end();
            assert!(
                trimmed.ends_with(['│', '╮', '╯']),
                "text overwrote the notice border: {trimmed:?}"
            );
        }
        assert!(
            painted.contains("17a4b207"),
            "the tail of the path never reached the screen:\n{painted}"
        );
    }
}
