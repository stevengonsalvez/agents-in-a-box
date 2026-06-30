// ABOUTME: Main layout component handling split-pane arrangement and bottom menu bar

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
/// session info (3) + menu bar (6), and the pane border takes 2 more rows/
/// cols off the interior. `sidebar_width` is the live
/// `sessions_pane_state.effective_width(..)` for the same terminal width.
pub fn interactive_embed_size(width: u16, height: u16, sidebar_width: u16) -> (u16, u16) {
    const VERTICAL_CHROME: u16 = 3 + 3 + 6; // status bar + session info + menu bar
    const PANE_BORDERS: u16 = 2;
    let rows = height.saturating_sub(VERTICAL_CHROME + PANE_BORDERS).max(1);
    let cols = width.saturating_sub(sidebar_width.saturating_add(PANE_BORDERS)).max(1);
    (rows, cols)
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

    pub fn render(&mut self, frame: &mut Frame, state: &mut AppState) {
        // Full-screen views go through the screen registry. Each Screen impl
        // owns its component(s) and renders any screen-specific overlays
        // (e.g. Config's auth-provider/config popups). Help overlay is
        // rendered post-screen as it's universal across full-screen views.
        let frame_size = frame.area();
        if let Some(screen) = self.screens.get_mut(&state.current_screen) {
            tracing::debug!("Rendering screen via registry: {}", state.current_screen);
            screen.render(frame, frame_size, state);
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
            if let Some(ref overlay) = state.mcp_overlay {
                crate::components::mcp_overlay::render(frame, frame_size, overlay);
            }
            // Daemons overlay (read-only; same z-order as MCP overlay).
            if let Some(ref overlay) = state.daemons_overlay {
                crate::components::daemons_overlay::render(frame, frame_size, overlay);
            }
            if state.confirmation_dialog.is_some() {
                self.confirmation_dialog.render(frame, frame_size, state);
            }
            return;
        }

        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Top status bar
                Constraint::Min(0),    // Main content area
                Constraint::Length(3), // Session info (single line + borders)
                Constraint::Length(6), // Bottom menu bar (4 lines + borders)
            ])
            .split(frame.area());

        // Render top status bar
        self.render_status_bar(frame, main_layout[0], state);

        // Simple 2-panel layout: session list | logs (Claude chat is now a popup).
        // The interactive embed honors whatever sidebar layout the user has
        // (decision 2026-06-12: no forced collapse — the sidebar is a fixed
        // ~40 cols, modern TUIs reflow cleanly, and `B` pre-collapses to the
        // rail when maximum embed width is wanted).
        let sessions_width = state.sessions_pane_state.effective_width(main_layout[1].width);
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sessions_width), // Session list
                Constraint::Min(0),                 // Live logs stream
            ])
            .split(main_layout[1]);
        state.sessions_pane_state.set_layout(content_chunks[0], content_chunks[1]);

        // Pass focus information to components
        if state.sessions_pane_state.collapsed {
            self.render_collapsed_sessions_rail(frame, content_chunks[0], state);
        } else {
            self.session_list.render(frame, content_chunks[0], state);
        }

        // Render tmux preview if selected session has tmux, otherwise show live logs
        // This includes both regular Claude sessions AND shell sessions
        let selected_has_tmux = state
            .get_selected_session()
            .and_then(|s| s.tmux_session_name.as_ref())
            .is_some()
            || state.selected_shell_session().is_some();

        if state.is_interactive_pane() {
            // Live interactive embed occupies the right pane. Resize the embed to
            // the pane interior (minus the border) so the inner program reflows,
            // then render the live terminal in place of the read-only preview.
            let area = content_chunks[1];
            let inner = area.inner(Margin {
                vertical: 1,
                horizontal: 1,
            });
            if let Some(e) = state.embed.as_mut() {
                let _ = e.resize(inner.height, inner.width);
            }
            // Publish the interior so mouse events can be translated into
            // 1-based pane-local SGR coordinates (see encode_mouse_event).
            state.embed_pane_area = Some(inner);
            self.tmux_preview.render_interactive(frame, area, state);
        } else if selected_has_tmux {
            // Render tmux preview pane (read-only capture)
            self.tmux_preview.render(frame, content_chunks[1], state);
        } else {
            // Render traditional live logs stream
            self.live_logs_stream.render(frame, content_chunks[1], state);
        }

        // Render bottom logs area (traditional logs viewer)
        self.logs_viewer.render(frame, main_layout[2], state);

        // Render bottom menu bar
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
        if let Some(ref overlay) = state.mcp_overlay {
            crate::components::mcp_overlay::render(frame, frame.size(), overlay);
        }
        // Daemons overlay (read-only; same z-order as MCP overlay).
        if let Some(ref overlay) = state.daemons_overlay {
            crate::components::daemons_overlay::render(frame, frame.size(), overlay);
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

    fn render_collapsed_sessions_rail(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let border_color = if state.sessions_pane_state.edge_highlighted() {
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
        // Below this width the two-column split can't hold the widest
        // session-action line (~41 cols) plus the panels column without
        // clipping, so fall back to the stacked legend.
        const TWO_COL_MIN_WIDTH: u16 = 100;
        if area.width >= TWO_COL_MIN_WIDTH {
            self.render_menu_bar_two_col(frame, area, state);
        } else {
            self.render_menu_bar_stacked(frame, area, state);
        }
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
            desc(" pane "),
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
        // same set), and closing a panel returns to this screen. The
        // inbox hint is always shown so users can discover the Inbox
        // screen even on a fresh install with zero events. When the
        // store reports unread + non-dismissed rows, a `● N` glyph is
        // rendered alongside the `b inbox` hint, capped at `99+` so a
        // large backlog can't widen the bar unbounded.
        let inbox_unread = state
            .inbox_state
            .store
            .as_ref()
            .and_then(|s| s.unread_count().ok())
            .unwrap_or(0);
        let mut line4_spans = Vec::new();
        if let Some(badge) = inbox_unread_badge(inbox_unread) {
            line4_spans.push(Span::styled(
                badge,
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        }
        line4_spans.extend([
            key("b", GOLD),
            desc(" inbox "),
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
                desc(" pane  "),
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
        // The inbox unread badge keeps its place ahead of `b inbox`, capped at
        // `99+` so a backlog can't widen the column past the divider.
        let inbox_unread = state
            .inbox_state
            .store
            .as_ref()
            .and_then(|s| s.unread_count().ok())
            .unwrap_or(0);
        let mut row1 = Vec::new();
        if let Some(badge) = inbox_unread_badge(inbox_unread) {
            row1.push(Span::styled(
                badge,
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        }
        row1.extend([
            key("b", GOLD),
            desc(" inbox  "),
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

    fn render_status_bar(&self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        let mut status_spans: Vec<Span> = vec![];

        // Claude-chat popup toggle — a small global indicator. The
        // workspace / branch / session-status that used to live here were
        // removed: they duplicated the bottom "Session Info" line. This
        // top bar is now a dedicated, full-width live-quota line so both
        // providers fit (and degrade gracefully) instead of being squeezed
        // out by that duplicated content.
        if state.claude_chat_visible {
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
        let live_spans = build_live_status_spans(state, avail);
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

    fn render_notifications(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let notifications = state.get_current_notifications();
        if notifications.is_empty() {
            return;
        }

        // Position notifications in the top-right corner
        let notification_width = 50;
        let notification_height = notifications.len() as u16 * 3; // 3 lines per notification

        let notification_area = Rect {
            x: area.width.saturating_sub(notification_width + 2),
            y: 1,
            width: notification_width,
            height: notification_height.min(area.height.saturating_sub(2)),
        };

        // Render each notification
        for (i, notification) in notifications.iter().enumerate() {
            let y_offset = i as u16 * 3;
            if y_offset >= notification_area.height {
                break; // Don't render notifications that won't fit
            }

            let single_notification_area = Rect {
                x: notification_area.x,
                y: notification_area.y + y_offset,
                width: notification_area.width,
                height: 3.min(notification_area.height - y_offset),
            };

            let (icon, text_color, border_color) = match notification.notification_type {
                crate::app::state::NotificationType::Success => {
                    ("✓ ", SELECTION_GREEN, SELECTION_GREEN)
                }
                crate::app::state::NotificationType::Error => {
                    ("✗ ", Color::Rgb(230, 100, 100), Color::Rgb(230, 100, 100))
                }
                crate::app::state::NotificationType::Warning => {
                    ("⚠ ", WARNING_ORANGE, WARNING_ORANGE)
                }
                crate::app::state::NotificationType::Info => {
                    ("ℹ ", CORNFLOWER_BLUE, CORNFLOWER_BLUE)
                }
            };

            let notification_line = Line::from(vec![
                Span::styled(
                    icon,
                    Style::default().fg(text_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    notification.message.as_str(),
                    Style::default().fg(text_color),
                ),
            ]);

            let notification_widget = Paragraph::new(notification_line)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(border_color))
                        .style(Style::default().bg(PANEL_BG)),
                )
                .wrap(ratatui::widgets::Wrap { trim: true });

            frame.render_widget(notification_widget, single_notification_area);
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
        let commit_message = state.quick_commit_message.as_ref().unwrap_or(&empty_string);

        // Create spans with cursor visualization
        let (before_cursor, after_cursor) =
            commit_message.split_at(state.quick_commit_cursor.min(commit_message.len()));

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

/// Pick the right restart-shaped affordance to show on the bottom menu
/// bar for the currently-highlighted session.
///
/// `r` resumes a Stopped Interactive (tmux) session in-place — the
/// recoverable escape hatch added with the soft-stop feature. `e`
/// recreates a Boss/Docker session in a fresh container.
///
/// The label deliberately reads `recreate` (not the generic `restart`)
/// so it is obvious this is the Docker/Boss path — `e` tears down the
/// old container and spins up a new one. Showing it for a stopped
/// Interactive session would point users at the wrong key — pressing
/// `e` triggers Docker logic that doesn't apply, while `r` is what
/// actually resumes the tmux pane and relaunches the embedded CLI.
/// Format the inbox unread badge for the menu bar, or `None` when there is
/// nothing unread. The count is capped at `99+` so a large backlog can't
/// widen line 1 of the bar past the 80-col minimum (the badge is the only
/// variable-width token on that line). The widest possible badge is
/// `"● 99+ "` (6 columns).
fn inbox_unread_badge(unread: u64) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(format!("● {unread} ")),
        _ => Some("● 99+ ".to_string()),
    }
}

impl Default for LayoutComponent {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the compact "live OAuth window" spans appended to the top status
/// bar. Returns an empty vec when nothing should render (statusline
/// unwired AND user declined, or status detection failed).
///
/// The settings.json read goes through [`AppState::statusline_status_cached`]
/// so the top bar's 30-60Hz redraws don't translate into 30-60Hz
/// filesystem reads.
pub fn build_live_status_spans(state: &mut AppState, max_width: usize) -> Vec<Span<'static>> {
    use crate::cli::statusline_install::StatuslineStatus;
    use crate::config::StatuslineDecision;
    use crate::models::live_window::Source;

    let status = state.statusline_status_cached();
    let decision = state.app_config.ui_preferences.statusline_decision;

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

#[cfg(test)]
mod menu_bar_tests {
    use super::inbox_unread_badge;

    #[test]
    fn inbox_badge_hidden_when_zero() {
        assert_eq!(inbox_unread_badge(0), None);
    }

    #[test]
    fn inbox_badge_shows_exact_count_up_to_99() {
        assert_eq!(inbox_unread_badge(1).as_deref(), Some("● 1 "));
        assert_eq!(inbox_unread_badge(99).as_deref(), Some("● 99 "));
    }

    #[test]
    fn inbox_badge_caps_at_99_plus_and_bounds_width() {
        // Beyond 99 the count is clamped so a huge backlog can't widen the
        // bar. The widest badge must stay at 6 columns ("● 99+ ").
        assert_eq!(inbox_unread_badge(100).as_deref(), Some("● 99+ "));
        for n in [100u64, 655, 9_999, u64::MAX] {
            let badge = inbox_unread_badge(n).expect("badge present");
            assert_eq!(badge, "● 99+ ");
            assert!(badge.chars().count() <= 6, "badge too wide: {badge:?}");
        }
    }

    /// Render the menu bar at the conventional 80-column minimum and assert
    /// every advertised key token survives — i.e. nothing is silently
    /// truncated off either end of the centered four-line bar. This guards
    /// the regression where adding keys (filter, 1-9, Space, F2, del-sel,
    /// inbox) overflowed 80 cols and clipped the inbox shortcut.
    #[test]
    fn menu_bar_keys_not_truncated_at_80_cols() {
        use crate::app::state::AppState;
        use crate::components::layout::LayoutComponent;
        use ratatui::{Terminal, backend::TestBackend};

        let layout = LayoutComponent::new();
        let state = AppState::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        terminal
            .draw(|f| {
                let area = f.size();
                layout.render_menu_bar(f, area, &state);
            })
            .unwrap();

        let rendered: String =
            terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();

        // The restart slot shows `r resume`.
        for token in [
            "ew",       // new
            "xpand",    // expand
            "focus",    // Tab focus
            "ttach",    // attach
            "pane",     // A — in-pane interactive embed
            "1-9",      // quick attach
            "Space",    // multi-select
            "tar",      // star
            "resume",   // r — resume a stopped tmux session
            "del-sel",  // D bulk delete
            "editor",   // o
            "shell",    // $
            "F2",       // rename
            "git",      // g (rendered as g + "it")
            "commit",   // p
            "laude",    // c claude
            "refresh",  // f
            "filter",   // F  ← the key that was missing before
            "re-auth",  // u (moved off A for in-pane attach)
            "?/H",      // help
            "home",     // q
            "inbox",    // b
            "stats",    // i — analytics panel
            "witr",     // w — process-causality browser
            "skills",   // k — skills browser
            "memory",   // m — learnings KB browser
            "abtop",    // t — top-for-agents monitor
        ] {
            assert!(
                rendered.contains(token),
                "menu token {token:?} truncated at 80 cols.\nRendered:\n{rendered}"
            );
        }

        // Stronger guard: the centered Paragraph truncates from both ends when
        // a line is wider than the inner area. Each of the 4 content rows
        // (rows 1..=4; rows 0 and 5 are the rounded border) must therefore
        // keep at least one space of padding against both inner edges — if a
        // row filled edge-to-edge it would mean content was clipped.
        let buf = terminal.backend().buffer();
        for y in 1..=4u16 {
            let left = buf.get(1, y).symbol().to_string();
            let right = buf.get(78, y).symbol().to_string();
            assert!(
                left == " " && right == " ",
                "menu row {y} fills the bar edge-to-edge (clipped). \
                 left={left:?} right={right:?}"
            );
        }
    }

    /// On a wide terminal the legend switches to the two-column split. Assert
    /// the divider and both section headers render (proving we took the split
    /// path, not the stacked fallback) and that every advertised key survives
    /// — the wide analogue of `menu_bar_keys_not_truncated_at_80_cols`.
    #[test]
    fn menu_bar_two_col_keeps_every_key_and_draws_divider() {
        use crate::app::state::AppState;
        use crate::components::layout::LayoutComponent;
        use ratatui::{Terminal, backend::TestBackend};

        let layout = LayoutComponent::new();
        let state = AppState::default();
        // Comfortably above TWO_COL_MIN_WIDTH so the split path renders.
        let mut terminal = Terminal::new(TestBackend::new(140, 6)).unwrap();
        terminal
            .draw(|f| {
                let area = f.size();
                layout.render_menu_bar(f, area, &state);
            })
            .unwrap();

        let rendered: String =
            terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();

        // Vertical divider proves the two-column path (stacked has none).
        assert!(
            rendered.contains('│'),
            "two-column divider missing:\n{rendered}"
        );
        // Section headers ride the top border.
        assert!(
            rendered.contains("Session actions"),
            "left header missing:\n{rendered}"
        );
        assert!(
            rendered.contains("Panels & views"),
            "right header missing:\n{rendered}"
        );

        for token in [
            "ew",       // new
            "ttach",    // attach
            "pane",     // A — in-pane interactive embed
            "1-9",      // quick attach
            "Space",    // multi-select
            "tar",      // star
            "resume",   // r — resume a stopped tmux session
            "del-sel",  // D bulk delete
            "editor",   // o
            "shell",    // $
            "F2",       // rename
            "commit",   // p
            "refresh",  // f
            "filter",   // F
            "re-auth",  // u
            "inbox",    // b
            "stats",    // i
            "witr",     // w
            "skills",   // k
            "memory",   // m
            "abtop",    // t
            "git",      // g
            "claude",   // c
            "xpand",    // E expand
            "focus",    // Tab focus
            "?/H",      // help
            "home",     // q
        ] {
            assert!(
                rendered.contains(token),
                "menu token {token:?} missing in two-col legend.\nRendered:\n{rendered}"
            );
        }
    }

    /// The two-column split first engages at exactly `TWO_COL_MIN_WIDTH` (100),
    /// which is also where its columns are narrowest and clipping would first
    /// bite. Render at the boundary and assert the longest token in each column
    /// (`re-auth` on the left, `home`/`abtop` on the right) survives and the
    /// content rows keep their edge padding — the centred Paragraphs would eat
    /// both ends if a line overflowed its half.
    #[test]
    fn menu_bar_two_col_no_clip_at_threshold_width() {
        use crate::app::state::AppState;
        use crate::components::layout::LayoutComponent;
        use ratatui::{Terminal, backend::TestBackend};

        let layout = LayoutComponent::new();
        let state = AppState::default();
        let mut terminal = Terminal::new(TestBackend::new(100, 6)).unwrap();
        terminal.draw(|f| layout.render_menu_bar(f, f.size(), &state)).unwrap();

        let buf = terminal.backend().buffer();
        let rendered: String = buf.content().iter().map(|c| c.symbol()).collect();

        // Took the split path, not the stacked fallback.
        assert!(
            rendered.contains('│'),
            "no divider at threshold:\n{rendered}"
        );
        for token in ["re-auth", "resume", "del-sel", "abtop", "home", "witr"] {
            assert!(
                rendered.contains(token),
                "token {token:?} clipped at threshold width 100:\nRendered:\n{rendered}"
            );
        }
        // Inner edges (cols 1 and 98) must stay blank on every content row.
        for y in 1..=4u16 {
            let left = buf.get(1, y).symbol().to_string();
            let right = buf.get(98, y).symbol().to_string();
            assert!(
                left == " " && right == " ",
                "menu row {y} clipped to the edge at width 100. left={left:?} right={right:?}"
            );
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
        assert_eq!(interactive_embed_size(120, 30, 40), (16, 78));
        assert_eq!(interactive_embed_size(120, 30, 5), (16, 113));
        assert_eq!(interactive_embed_size(80, 24, 5), (10, 73));
    }

    #[test]
    fn never_returns_zero_cells() {
        assert_eq!(interactive_embed_size(0, 0, 5), (1, 1));
        assert_eq!(interactive_embed_size(7, 14, 40), (1, 1));
    }
}
