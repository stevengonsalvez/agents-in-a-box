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
    AgentSelectionComponent, AttachedTerminalComponent, AuthProviderPopupComponent,
    AuthSetupComponent, ClaudeChatComponent, ConfigPopupComponent, ConfigScreenComponent,
    ConfirmationDialogComponent, HelpComponent, HomeScreenComponent, HomeScreenV2Component,
    LiveLogsStreamComponent, LogHistoryViewerComponent, LogsViewerComponent, NewSessionComponent,
    OnboardingComponent, SessionListComponent, SetupMenuComponent, TmuxPreviewPane,
};
use crate::app::{AppState, state::View};

pub struct LayoutComponent {
    session_list: SessionListComponent,
    logs_viewer: LogsViewerComponent,
    claude_chat: ClaudeChatComponent,
    live_logs_stream: LiveLogsStreamComponent,
    help: HelpComponent,
    new_session: NewSessionComponent,
    confirmation_dialog: ConfirmationDialogComponent,
    attached_terminal: AttachedTerminalComponent,
    auth_setup: AuthSetupComponent,
    tmux_preview: TmuxPreviewPane,
    // AINB 2.0 components
    home_screen: HomeScreenComponent,
    home_screen_v2: HomeScreenV2Component,
    agent_selection: AgentSelectionComponent,
    config_screen: ConfigScreenComponent,
    auth_provider_popup: AuthProviderPopupComponent,
    config_popup: ConfigPopupComponent,
    log_history_viewer: LogHistoryViewerComponent,
    onboarding: OnboardingComponent,
    setup_menu: SetupMenuComponent,
}

impl LayoutComponent {
    pub fn new() -> Self {
        Self {
            session_list: SessionListComponent::new(),
            logs_viewer: LogsViewerComponent::new(),
            claude_chat: ClaudeChatComponent::new(),
            live_logs_stream: LiveLogsStreamComponent::new(),
            help: HelpComponent::new(),
            new_session: NewSessionComponent::new(),
            confirmation_dialog: ConfirmationDialogComponent::new(),
            attached_terminal: AttachedTerminalComponent::new(),
            auth_setup: AuthSetupComponent::new(),
            tmux_preview: TmuxPreviewPane::new(),
            // AINB 2.0 components
            home_screen: HomeScreenComponent::new(),
            home_screen_v2: HomeScreenV2Component::new(),
            agent_selection: AgentSelectionComponent::new(),
            config_screen: ConfigScreenComponent::new(),
            auth_provider_popup: AuthProviderPopupComponent::new(),
            config_popup: ConfigPopupComponent::new(),
            log_history_viewer: LogHistoryViewerComponent::new(),
            onboarding: OnboardingComponent::new(),
            setup_menu: SetupMenuComponent::new(),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, state: &mut AppState) {
        // Special handling for onboarding wizard view (full screen)
        if state.current_view == View::Onboarding {
            if let Some(ref onboarding_state) = state.onboarding_state {
                tracing::debug!("Rendering Onboarding view");
                self.onboarding.render(frame, frame.size(), onboarding_state);
            }
            return;
        }

        // Special handling for setup menu view (overlay on home screen)
        if state.current_view == View::SetupMenu {
            tracing::debug!("Rendering SetupMenu view");
            // First render the home screen as background
            self.home_screen_v2.render_with_loading(
                frame,
                frame.size(),
                &mut state.home_screen_v2_state,
                &state.workspaces,
                state.is_loading_workspaces,
            );
            // Then render setup menu as overlay
            self.setup_menu.render(frame, frame.size(), &state.setup_menu_state);
            return;
        }

        // Special handling for auth setup view (full screen)
        if state.current_view == View::AuthSetup {
            let centered_area = centered_rect(60, 60, frame.size());
            self.auth_setup.render(frame, centered_area, state);
            return;
        }

        // Special handling for attached terminal view (full screen)
        if state.current_view == View::AttachedTerminal {
            self.attached_terminal.render(frame, frame.size(), state);
            return;
        }

        // Special handling for git view (full screen)
        if state.current_view == View::GitView {
            if let Some(ref git_state) = state.git_view_state {
                crate::components::GitViewComponent::render(frame, frame.size(), git_state);
            }
            return;
        }

        // AINB 2.0: Home screen (full screen) - Now using V2 with sidebar and mascot
        if state.current_view == View::HomeScreen {
            tracing::debug!("Rendering HomeScreen V2 view");
            self.home_screen_v2.render_with_loading(
                frame,
                frame.size(),
                &mut state.home_screen_v2_state,
                &state.workspaces,
                state.is_loading_workspaces,
            );
            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on HomeScreen");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // AINB 2.0: Agent selection (full screen)
        if state.current_view == View::AgentSelection {
            tracing::debug!("Rendering AgentSelection view");
            self.agent_selection.render(frame, frame.size(), state);
            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on AgentSelection");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // AINB 2.0: Config screen (full screen)
        if state.current_view == View::Config {
            tracing::debug!("Rendering Config view");
            self.config_screen.render(frame, frame.size(), state);

            // Render auth provider popup on top if visible
            if state.auth_provider_popup_state.show_popup {
                tracing::debug!("Rendering auth provider popup");
                self.auth_provider_popup.render(frame, frame.size(), state);
            }

            // Render config popup on top if visible (for choice/text input)
            if state.config_popup_state.show_popup {
                tracing::debug!("Rendering config popup");
                self.config_popup.render(frame, frame.size(), &state.config_popup_state);
            }

            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on Config");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // AINB 2.0: Log history viewer (full screen)
        if state.current_view == View::LogHistory {
            tracing::debug!("Rendering LogHistory view");
            self.log_history_viewer
                .render(frame, frame.size(), &mut state.log_history_state);
            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on LogHistory");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // Changelog viewer (full screen)
        if state.current_view == View::Changelog {
            tracing::debug!("Rendering Changelog view");
            crate::components::ChangelogComponent::render(
                frame,
                frame.size(),
                &state.changelog_state,
            );
            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on Changelog");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // Usage analytics view (full screen)
        if state.current_view == View::Analytics {
            tracing::debug!("Rendering Usage Analytics view");
            // Compute the enable-card visibility inputs once per frame
            // through the AppState cache so we don't re-read settings.json
            // on every refresh tick.
            let live_source = crate::models::live_window::current().source;
            let statusline_status = state.statusline_status_cached();
            let statusline_decision = state.app_config.ui_preferences.statusline_decision;
            let enable_card_ctx = crate::components::usage::EnableCardCtx {
                live_source,
                statusline_status,
                statusline_decision,
            };
            crate::components::usage::render(
                frame,
                frame.size(),
                &state.usage_state,
                enable_card_ctx,
            );
            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on Analytics");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // Skills browser view (full screen)
        if state.current_view == View::Skills {
            tracing::debug!("Rendering Skills view");
            crate::components::skills::render(frame, frame.size(), &state.skills_state);
            if state.help_visible {
                tracing::debug!("Rendering help overlay on Skills");
                self.help.render(frame, frame.size());
            }
            return;
        }

        // Session recovery view (full screen)
        if state.current_view == View::SessionRecovery {
            tracing::debug!("Rendering SessionRecovery view");
            crate::components::SessionRecovery::render(
                frame,
                frame.size(),
                &mut state.session_recovery_state,
            );
            // Render help overlay on top if visible
            if state.help_visible {
                tracing::debug!("Rendering help overlay on SessionRecovery");
                self.help.render(frame, frame.size());
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
            .split(frame.size());

        // Render top status bar
        self.render_status_bar(frame, main_layout[0], state);

        // Simple 2-panel layout: session list | logs (Claude chat is now a popup)
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(40), // Session list
                Constraint::Percentage(60), // Live logs stream
            ])
            .split(main_layout[1]);

        // Pass focus information to components
        self.session_list.render(frame, content_chunks[0], state);

        // Render tmux preview if selected session has tmux, otherwise show live logs
        // This includes both regular Claude sessions AND shell sessions
        let selected_has_tmux = state
            .get_selected_session()
            .and_then(|s| s.tmux_session_name.as_ref())
            .is_some()
            || state.selected_shell_session().is_some();

        if selected_has_tmux {
            // Render tmux preview pane
            self.tmux_preview.render(frame, content_chunks[1], state);
        } else {
            // Render traditional live logs stream
            self.live_logs_stream.render(frame, content_chunks[1], state);
        }

        // Render bottom logs area (traditional logs viewer)
        self.logs_viewer.render(frame, main_layout[2], state);

        // Render bottom menu bar
        self.render_menu_bar(frame, main_layout[3]);

        // Render help overlay if visible
        if state.help_visible {
            self.help.render(frame, frame.size());
        }

        // Render new session overlay if visible
        if state.current_view == View::NewSession || state.current_view == View::SearchWorkspace {
            self.new_session.render(frame, frame.size(), state);
        }

        // Render Claude chat popup if visible
        if state.current_view == View::ClaudeChat {
            let popup_area = centered_rect(80, 80, frame.size());
            self.claude_chat.render(frame, popup_area, state);
        }

        // Render confirmation dialog if visible (highest priority overlay)
        if state.confirmation_dialog.is_some() {
            self.confirmation_dialog.render(frame, frame.size(), state);
        }

        // Render quick commit dialog if visible
        if state.is_in_quick_commit_mode() {
            self.render_quick_commit_dialog(frame, frame.size(), state);
        }

        // Render notifications (top-right corner)
        self.render_notifications(frame, frame.size(), state);
    }

    /// Get mutable reference to live logs component for scroll handling
    pub fn live_logs_mut(&mut self) -> &mut LiveLogsStreamComponent {
        &mut self.live_logs_stream
    }

    /// Get mutable reference to tmux preview component for scroll handling
    pub fn tmux_preview_mut(&mut self) -> &mut TmuxPreviewPane {
        &mut self.tmux_preview
    }

    fn render_menu_bar(&self, frame: &mut Frame, area: Rect) {
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
                "e",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" restart ", Style::default().fg(MUTED_GRAY)),
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
    use crate::models::live_window::{Source, current};

    let status = state.statusline_status_cached();
    let decision = state.app_config.ui_preferences.statusline_decision;

    // Trust the cache: if Tier1 data is flowing — whether it came from
    // our own command in settings.json (Configured) or from a user's
    // custom statusline that side-channels via `ainb claudecode statusline
    // --cache-only` (Other) — show the live widget. The CTA is for the
    // genuinely-unwired case only.
    let live = current();
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
