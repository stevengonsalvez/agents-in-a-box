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
                Constraint::Length(4), // Bottom menu bar (2 lines + borders)
            ])
            .split(frame.area());

        // Render top status bar
        self.render_status_bar(frame, main_layout[0], state);

        // Simple 2-panel layout: session list | logs (Claude chat is now a popup)
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
            let rows = area.height.saturating_sub(2);
            let cols = area.width.saturating_sub(2);
            if let Some(e) = state.embed.as_mut() {
                let _ = e.resize(rows, cols);
            }
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
            Line::from(""),
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

    fn render_menu_bar(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Pure decision for which restart-shaped affordance to surface.
        // See test below for the truth table.
        // The session-action group's restart-shaped affordance is split
        // across two keys with different semantics:
        //   - `r` resumes a Stopped Interactive (tmux) session in-place.
        //   - `e` restarts a Boss/Docker session into a fresh container.
        // Show the binding that actually applies to the highlighted row
        // so users don't press the wrong one. See events.rs:834 and
        // events.rs:868 for the dispatch logic.
        let (restart_key, restart_label) = restart_affordance(state.selected_session());

        // Premium styled command bar with separators - 2 lines for better readability
        // Line 1: Navigation, Session Actions
        let line1_spans = vec![
            // Navigation group
            Span::styled("n", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("ew ", Style::default().fg(MUTED_GRAY)),
            Span::styled("E", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("xpand ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Tab",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" focus", Style::default().fg(MUTED_GRAY)),
            Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)),
            // Session actions group
            Span::styled(
                "a",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled("ttach ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                restart_key,
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(restart_label, Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "d",
                Style::default().fg(Color::Rgb(230, 100, 100)).add_modifier(Modifier::BOLD),
            ),
            Span::styled("elete ", Style::default().fg(MUTED_GRAY)),
            Span::styled("$", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" shell ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "o",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" editor", Style::default().fg(MUTED_GRAY)),
        ];

        // Line 2: Git, Tools, System
        let line2_spans = vec![
            // Git group
            Span::styled(
                "g",
                Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("it ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "p",
                Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" commit", Style::default().fg(MUTED_GRAY)),
            Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)),
            // Tools group
            Span::styled(
                "c",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("laude ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "f",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" refresh ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "x",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cleanup", Style::default().fg(MUTED_GRAY)),
            Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)),
            // System group
            Span::styled(
                "r",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" re-auth ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "H",
                Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" help ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "q",
                Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" home", Style::default().fg(MUTED_GRAY)),
        ];

        // ainb-hooks inbox shortcut on the menu bar. Always shown so
        // users can discover the Inbox screen even on a fresh install
        // with zero events. When the store reports unread + non-
        // dismissed rows, a `● N` glyph is rendered alongside the
        // `I inbox` hint to surface that there is something to read.
        let inbox_unread = state
            .inbox_state
            .store
            .as_ref()
            .and_then(|s| s.unread_count().ok())
            .unwrap_or(0);
        let mut line2_spans = line2_spans;
        line2_spans.push(Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)));
        if inbox_unread > 0 {
            line2_spans.push(Span::styled(
                format!("● {inbox_unread} "),
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        }
        line2_spans.push(Span::styled(
            "b",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        line2_spans.push(Span::styled(" inbox", Style::default().fg(MUTED_GRAY)));

        let menu_lines = vec![Line::from(line1_spans), Line::from(line2_spans)];

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

    fn render_status_bar(&self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        let mut status_spans: Vec<Span> = vec![];

        // Current workspace/repo info
        if let Some(workspace_idx) = state.selected_workspace_index {
            if let Some(workspace) = state.workspaces.get(workspace_idx) {
                if let Some(repo_name) = workspace.path.file_name().and_then(|n| n.to_str()) {
                    status_spans.push(Span::styled("📁 ", Style::default().fg(GOLD)));
                    status_spans.push(Span::styled(
                        repo_name.to_string(),
                        Style::default().fg(SOFT_WHITE),
                    ));
                }
            }
        }

        // Active session info
        if let Some(_session_id) = state.get_selected_session_id() {
            if let Some(workspace_idx) = state.selected_workspace_index {
                if let Some(session_idx) = state.selected_session_index {
                    if let Some(workspace) = state.workspaces.get(workspace_idx) {
                        if let Some(session) = workspace.sessions.get(session_idx) {
                            // Separator
                            if !status_spans.is_empty() {
                                status_spans.push(Span::styled(
                                    "  │  ",
                                    Style::default().fg(SUBDUED_BORDER),
                                ));
                            }

                            // Branch info
                            status_spans
                                .push(Span::styled("🌿 ", Style::default().fg(SELECTION_GREEN)));
                            status_spans.push(Span::styled(
                                session.branch_name.clone(),
                                Style::default().fg(SOFT_WHITE),
                            ));

                            // Container info
                            if let Some(container_id) = &session.container_id {
                                let short_id = &container_id[..8.min(container_id.len())];
                                let (status_icon, status_color) = match session.status {
                                    crate::models::SessionStatus::Running => {
                                        ("🟢", SELECTION_GREEN)
                                    }
                                    crate::models::SessionStatus::Stopped => {
                                        ("🔴", Color::Rgb(230, 100, 100))
                                    }
                                    crate::models::SessionStatus::Idle => ("🟡", WARNING_ORANGE),
                                    crate::models::SessionStatus::Error(_) => {
                                        ("❌", Color::Rgb(230, 100, 100))
                                    }
                                };
                                status_spans.push(Span::styled(
                                    "  │  ",
                                    Style::default().fg(SUBDUED_BORDER),
                                ));
                                status_spans.push(Span::styled(
                                    format!("{} ", status_icon),
                                    Style::default().fg(status_color),
                                ));
                                status_spans.push(Span::styled(
                                    format!("{} ", session.name),
                                    Style::default().fg(SOFT_WHITE),
                                ));
                                status_spans.push(Span::styled(
                                    format!("({})", short_id),
                                    Style::default().fg(MUTED_GRAY),
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Claude chat status
        if !status_spans.is_empty() {
            status_spans.push(Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)));
        }
        if state.claude_chat_visible {
            status_spans.push(Span::styled("🗨️ ", Style::default().fg(SELECTION_GREEN)));
            status_spans.push(Span::styled("ON", Style::default().fg(SELECTION_GREEN)));
        } else {
            status_spans.push(Span::styled("🗨️ ", Style::default().fg(MUTED_GRAY)));
            status_spans.push(Span::styled("OFF", Style::default().fg(MUTED_GRAY)));
        }

        // Live OAuth window: append a compact widget when wired AND fresh,
        // a red CTA when not wired (and the user hasn't declined).
        // The status bar gracefully degrades on narrow terminals — we
        // measure the existing content first and drop the live widget if
        // it wouldn't fit.
        let live_spans = build_live_status_spans(state);
        let existing_w: usize = status_spans.iter().map(|s| s.content.chars().count()).sum();
        // 4 chars for the " │  " separator we'd add
        let live_w: usize = live_spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
            .saturating_add(5);
        let area_inner_w = area.width.saturating_sub(2) as usize; // borders
        if !live_spans.is_empty() && existing_w + live_w <= area_inner_w {
            if !status_spans.is_empty() {
                status_spans.push(Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)));
            }
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
/// restarts a Boss/Docker session into a fresh container.
///
/// Showing `e restart` for a stopped Interactive session would point
/// users at the wrong key — pressing `e` triggers Docker restart logic
/// that doesn't apply, while `r` is what actually resumes the tmux
/// pane and relaunches the embedded CLI.
fn restart_affordance(selected: Option<&crate::models::Session>) -> (&'static str, &'static str) {
    use crate::models::{SessionMode, SessionStatus};
    let stopped_interactive = matches!(
        selected,
        Some(s) if matches!(s.mode, SessionMode::Interactive)
            && matches!(s.status, SessionStatus::Stopped)
    );
    if stopped_interactive {
        ("r", " resume ")
    } else {
        ("e", " restart ")
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
pub fn build_live_status_spans(state: &mut AppState) -> Vec<Span<'static>> {
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
    if live.source == Source::Tier1Cache {
        return build_live_widget_spans(&live);
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
            build_cta_spans()
        }
        _ => Vec::new(),
    }
}

fn build_live_widget_spans(live: &crate::models::live_window::LiveWindow) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();

    if let Some(pct) = live.five_hour_pct {
        out.push(Span::styled("5h ", Style::default().fg(MUTED_GRAY)));
        out.push(Span::styled(
            mini_bar(pct),
            Style::default().fg(bar_color_5h(pct)),
        ));
        out.push(Span::styled(
            format!(" {pct}%"),
            Style::default().fg(bar_color_5h(pct)).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(pct) = live.seven_day_pct {
        if !out.is_empty() {
            out.push(Span::styled(" · ", Style::default().fg(SUBDUED_BORDER)));
        }
        out.push(Span::styled("wk ", Style::default().fg(MUTED_GRAY)));
        out.push(Span::styled(
            mini_bar(pct),
            Style::default().fg(bar_color_7d(pct)),
        ));
        out.push(Span::styled(
            format!(" {pct}%"),
            Style::default().fg(bar_color_7d(pct)).add_modifier(Modifier::BOLD),
        ));
    }
    // today_cost_usd intentionally not rendered: Claude Code's
    // /cost/total_cost_usd is the lifetime cost of a *single* session
    // (whichever invoked the statusline most recently), not today's
    // total. Misleading at a glance — keep the field on the cache
    // schema but don't surface it.
    if let Some(d) = live.resets_in {
        if !out.is_empty() {
            out.push(Span::styled(" · ", Style::default().fg(SUBDUED_BORDER)));
        }
        out.push(Span::styled("⏱ ", Style::default().fg(SOFT_WHITE)));
        out.push(Span::styled(format_hms(d), Style::default().fg(SOFT_WHITE)));
    }
    out
}

fn build_cta_spans() -> Vec<Span<'static>> {
    let red = Color::Rgb(230, 100, 100);
    vec![
        Span::styled("⚠ ", Style::default().fg(red).add_modifier(Modifier::BOLD)),
        Span::styled("Live Claude Code usage off", Style::default().fg(red)),
        Span::styled(" · press W to enable", Style::default().fg(MUTED_GRAY)),
    ]
}

/// Three-cell mini-bar: ▰ for filled, ▱ for empty. Matches the brief.
fn mini_bar(pct: u8) -> String {
    let cells = ((pct as f64 / 100.0) * 3.0).round() as usize;
    let filled = cells.min(3);
    let empty = 3 - filled;
    let mut s = String::with_capacity(3);
    for _ in 0..filled {
        s.push('▰');
    }
    for _ in 0..empty {
        s.push('▱');
    }
    s
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

fn format_hms(d: std::time::Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
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
        let live = LiveWindow {
            five_hour_pct: Some(40),
            seven_day_pct: Some(8),
            today_cost_usd: Some(1.5),
            resets_in: Some(Duration::from_secs(2 * 3600)),
            context_pct: None,
            model: None,
            source: Source::Tier1Cache,
        };
        let spans = build_live_widget_spans(&live);
        let text = flatten(&spans);
        assert!(text.contains("5h"));
        assert!(text.contains("40%"));
        assert!(text.contains("wk"));
        assert!(text.contains("8%"));
        // today_cost_usd is in the cache but intentionally not rendered —
        // it's a single session's lifetime cost, not today's total.
        assert!(!text.contains("$"));
        assert!(!text.contains("today"));
        assert!(text.contains("2h 00m"));
    }

    #[test]
    fn live_widget_omits_missing_fields() {
        let live = LiveWindow {
            five_hour_pct: Some(20),
            seven_day_pct: None,
            today_cost_usd: None,
            resets_in: None,
            context_pct: None,
            model: None,
            source: Source::Tier1Cache,
        };
        let spans = build_live_widget_spans(&live);
        let text = flatten(&spans);
        assert!(text.contains("5h"));
        assert!(!text.contains("wk"));
        assert!(!text.contains("$"));
        assert!(!text.contains("⏱"));
    }

    #[test]
    fn mini_bar_clamps_and_buckets() {
        assert_eq!(mini_bar(0), "▱▱▱");
        assert_eq!(mini_bar(33), "▰▱▱");
        assert_eq!(mini_bar(50), "▰▰▱");
        assert_eq!(mini_bar(99), "▰▰▰");
        assert_eq!(mini_bar(100), "▰▰▰");
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
    fn format_hms_smoke() {
        assert_eq!(format_hms(Duration::ZERO), "0m");
        assert_eq!(format_hms(Duration::from_secs(45 * 60)), "45m");
        assert_eq!(format_hms(Duration::from_secs(3600)), "1h 00m");
    }
}

#[cfg(test)]
mod menu_bar_tests {
    use super::restart_affordance;
    use crate::models::{Session, SessionMode, SessionStatus};

    fn stopped_interactive() -> Session {
        let mut s = Session::new("t".to_string(), "/tmp".to_string());
        s.mode = SessionMode::Interactive;
        s.status = SessionStatus::Stopped;
        s
    }

    fn running_interactive() -> Session {
        let mut s = Session::new("t".to_string(), "/tmp".to_string());
        s.mode = SessionMode::Interactive;
        s.status = SessionStatus::Running;
        s
    }

    fn stopped_boss() -> Session {
        let mut s = Session::new("t".to_string(), "/tmp".to_string());
        s.mode = SessionMode::Boss;
        s.status = SessionStatus::Stopped;
        s
    }

    #[test]
    fn stopped_interactive_shows_r_resume() {
        let s = stopped_interactive();
        assert_eq!(restart_affordance(Some(&s)), ("r", " resume "));
    }

    #[test]
    fn running_interactive_shows_e_restart() {
        // `r` is reauth-credentials when the session isn't stopped, so
        // surfacing `r resume` would be wrong. Fall back to `e restart`.
        let s = running_interactive();
        assert_eq!(restart_affordance(Some(&s)), ("e", " restart "));
    }

    #[test]
    fn stopped_boss_shows_e_restart() {
        // Boss/Docker sessions use the Docker container restart path.
        let s = stopped_boss();
        assert_eq!(restart_affordance(Some(&s)), ("e", " restart "));
    }

    #[test]
    fn no_selection_shows_e_restart() {
        assert_eq!(restart_affordance(None), ("e", " restart "));
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
