// ABOUTME: Session list component for displaying workspaces and sessions in hierarchical view

#![allow(dead_code)]

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph},
};

// Premium color palette (TUI Style Guide)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
// ainb-hooks per-session attention markers: `[?]` waiting on a user
// answer, `[!]` blocked on permission/approval, `[✓]` finished.
const ALERT_WAITING_AMBER: Color = Color::Rgb(230, 180, 80);
const ALERT_PERMISSION_RED: Color = Color::Rgb(220, 90, 90);

// Per-agent brand colors for the coding-agent pill chip. The chip is a
// filled block in these colors so the agent is identifiable at a glance —
// orange = Claude, blue = Gemini, etc. — even before the glyph resolves.
const BRAND_CLAUDE: Color = Color::Rgb(217, 119, 87); // Anthropic clay-orange
const BRAND_CODEX: Color = Color::Rgb(236, 236, 241); // OpenAI near-white
const BRAND_COPILOT: Color = Color::Rgb(46, 160, 67); // GitHub green
const BRAND_GEMINI: Color = Color::Rgb(66, 133, 244); // Google blue
const BRAND_ANTIGRAVITY: Color = Color::Rgb(66, 133, 244); // Google blue
const BRAND_KIRO: Color = Color::Rgb(171, 121, 224); // crystal purple
const BRAND_SHELL: Color = Color::Rgb(150, 150, 165); // muted slate
const BRAND_SSH: Color = Color::Rgb(255, 165, 0); // amber

// Powerline rounded caps that wrap the pill chip's filled middle.
const PILL_LEFT: &str = "\u{e0b6}"; //
const PILL_RIGHT: &str = "\u{e0b4}"; //

use ainb_plugin_notifyd::AlertKind;

use crate::app::{
    AppState,
    state::{AttachableRef, SessionContextAction, SessionListRowTarget},
};
use crate::models::{
    Session, SessionAgentType, SessionMode, SessionStatus, ShellSessionStatus, Workspace,
};

/// Width of the leading badge slot rendered before every list row.
/// Two characters: a digit (or space) and a trailing space separator.
const BADGE_SLOT_WIDTH: usize = 2;

/// Highest attach-shortcut index that fits in a single keystroke.
const MAX_BADGE: usize = 9;

/// Empty slot for non-attachable rows (workspace headers, separators,
/// section headers) and for attachable rows past `MAX_BADGE`.
fn empty_badge() -> Span<'static> {
    Span::raw(" ".repeat(BADGE_SLOT_WIDTH))
}

/// Gold-bold badge for the next attachable row. Advances the caller's
/// counter; returns an empty slot once `MAX_BADGE` has been exhausted so
/// the column position never shifts.
fn next_badge(attach_no: &mut usize) -> Span<'static> {
    *attach_no += 1;
    if *attach_no <= MAX_BADGE {
        Span::styled(
            format!("{} ", *attach_no),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )
    } else {
        empty_badge()
    }
}

pub struct SessionListComponent {
    list_state: ListState,
}

fn selected_row_target(state: &AppState) -> Option<SessionListRowTarget> {
    if let Some(workspace_idx) = state.selected_workspace_index {
        return match (state.selected_session_index, state.shell_selected) {
            (Some(session_idx), _) => Some(SessionListRowTarget::Attachable(
                AttachableRef::WorkspaceSession {
                    workspace_idx,
                    session_idx,
                },
            )),
            (None, true) => Some(SessionListRowTarget::Attachable(
                AttachableRef::WorkspaceShell { workspace_idx },
            )),
            (None, false) => Some(SessionListRowTarget::WorkspaceHeader { workspace_idx }),
        };
    }
    state
        .selected_ssh_session_index
        .map(|ssh_idx| SessionListRowTarget::Attachable(AttachableRef::SshSession { ssh_idx }))
        .or_else(|| {
            state.selected_other_tmux_index.map(|other_idx| {
                SessionListRowTarget::Attachable(AttachableRef::OtherTmux { other_idx })
            })
        })
}

impl Default for SessionListComponent {
    fn default() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self { list_state }
    }
}

impl SessionListComponent {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState) {
        // Update list state selection based on app state first
        self.update_selection(state);
        state.sessions_pane_state.set_list_scroll_offset(self.list_state.offset());

        let items = SessionListComponent::build_list_items_static(state);

        // Show focus indicator with premium colors
        use crate::app::state::FocusedPane;
        let (border_color, is_focused) = match state.focused_pane {
            FocusedPane::Sessions => (SELECTION_GREEN, true),
            FocusedPane::LiveLogs | FocusedPane::Preview => (SUBDUED_BORDER, false),
        };
        let border_color = if state.sessions_pane_state.edge_highlighted() {
            GOLD
        } else {
            border_color
        };

        // Visible workspace count reflects the filter — workspaces that lose
        // all their sessions to the filter (and have no shell) are dropped
        // from the rendered list, so the header should match.
        let workspace_count = state
            .workspaces
            .iter()
            .filter(|w| {
                w.sessions.iter().any(|s| state.session_passes_filter(s))
                    || w.shell_session.is_some()
            })
            .count();

        let mut title_spans = vec![
            Span::styled(" \u{f07b} ", Style::default().fg(GOLD)),
            Span::styled(
                "Workspaces ",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({})", workspace_count),
                Style::default()
                    .fg(if is_focused {
                        CORNFLOWER_BLUE
                    } else {
                        MUTED_GRAY
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        let filter_label = format!("F [{}]", state.session_filter.label());
        let title_prefix = format!(" \u{f07b} Workspaces ({workspace_count}) ");
        state.sessions_pane_state.set_filter_toggle_area(Rect::new(
            area.x.saturating_add(1 + title_prefix.chars().count() as u16),
            area.y,
            filter_label.chars().count() as u16,
            1,
        ));
        title_spans.push(Span::styled(filter_label, Style::default().fg(GOLD)));
        title_spans.push(Span::raw(" "));
        // 'B' is the keyboard twin of clicking the [-] glyph (hint lives next
        // to the control it drives, not in the bottom menu bar).
        title_spans.push(Span::styled(
            "B",
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        ));
        title_spans.push(Span::styled(
            "[-]",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD),
        ));

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(border_color))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(title_spans))
                    .title_bottom(
                        if state.ssh_session_rename_mode
                            || state.other_tmux_rename_mode
                            || state.session_label_rename_mode
                        {
                            // Rename mode help (SSH or Other tmux)
                            Line::from(vec![
                                Span::styled(
                                    " Enter",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" confirm ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " Esc",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" cancel ", Style::default().fg(MUTED_GRAY)),
                            ])
                        } else if state.is_ssh_session_selected() || state.is_other_tmux_selected()
                        {
                            // SSH or Other tmux selected help
                            Line::from(vec![
                                Span::styled(
                                    " j/k",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" nav ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " a",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" attach ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " F2",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" rename ", Style::default().fg(MUTED_GRAY)),
                            ])
                        } else {
                            // Default help — `j/k` nav segment dropped: arrow keys
                            // also navigate, and the panel was overflowing once
                            // `F filter` was added.
                            Line::from(vec![
                                Span::styled(
                                    " Enter",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" select ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " s",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" ⭐ ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " $",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" shell ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " Space",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" mark ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " D",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" del marked ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " F",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" filter ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                                Span::styled(
                                    " 1-9",
                                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(" attach ", Style::default().fg(MUTED_GRAY)),
                            ])
                        },
                    ),
            )
            .highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG))
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(list, area, &mut self.list_state);

        if state.session_label_rename_mode {
            let width = area.width.min(54);
            let height = 7;
            let popup = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(height) / 2,
                width,
                height,
            );
            frame.render_widget(Clear, popup);
            let block = Block::default()
                .title(" Session label ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(GOLD))
                .style(Style::default().bg(DARK_BG));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            let text = format!(
                "Durable name, Git branch unchanged\n\n{}|\n\nEnter save   Esc cancel   blank clears",
                state.session_label_rename_buffer
            );
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(SOFT_WHITE).bg(DARK_BG)),
                inner,
            );
        }

        if let Some(menu) = state.session_context_menu {
            let actions = state.session_context_actions();
            let width = area.width.min(30);
            let height = (actions.len() as u16 + 3).min(area.height);
            let popup = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(height) / 2,
                width,
                height,
            );
            frame.render_widget(Clear, popup);
            let block = Block::default()
                .title(" Session actions ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(GOLD))
                .style(Style::default().bg(DARK_BG));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            let lines: Vec<Line> = actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    let selected = index == menu.selected;
                    Line::styled(
                        format!(
                            "{} {}",
                            if selected { "▶" } else { " " },
                            context_action_label(*action)
                        ),
                        Style::default()
                            .fg(if selected {
                                SELECTION_GREEN
                            } else {
                                SOFT_WHITE
                            })
                            .bg(if selected { LIST_HIGHLIGHT_BG } else { DARK_BG }),
                    )
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    fn build_list_items_static(state: &AppState) -> Vec<ListItem<'static>> {
        let mut items = Vec::new();

        // Favorite status is precomputed off the render path into
        // `state.favorite_workspace_paths` (see
        // `AppState::recompute_favorite_workspaces`), so this hot loop does an
        // O(1) set lookup instead of re-parsing favorites.yaml and opening a
        // git repo per workspace on every frame. (perf: beads 9ov + 8rn)

        // Must stay in lockstep with AppState::attachable_items_in_order;
        // divergence would attach the wrong session for a given digit.
        let mut attach_no: usize = 0;

        for (workspace_idx, workspace) in state.workspaces.iter().enumerate() {
            let is_selected_workspace = state.selected_workspace_index == Some(workspace_idx);
            // Apply the session filter (Shift+F cycles): only count sessions
            // that pass the predicate so the workspace `(N)` matches what the
            // user actually sees rendered below.
            let session_count =
                workspace.sessions.iter().filter(|s| state.session_passes_filter(s)).count();
            let has_shell = workspace.shell_session.is_some();
            let total_count = session_count + if has_shell { 1 } else { 0 };

            // Hide workspaces that have no visible content under the active
            // filter (no matching sessions and no shell). Skip the entire
            // entry — including its header row — so the tree stays compact.
            if total_count == 0 {
                continue;
            }

            // Determine expand state: expanded if selected OR if expand_all is true
            let is_expanded = is_selected_workspace || state.expand_all_workspaces;

            let workspace_symbol = if total_count == 0 {
                "▷"
            } else if is_expanded {
                "▼"
            } else {
                "▶"
            };

            // Premium workspace styling. Folder headers are cornflower blue
            // (and bold) so they read distinctly as folders/containers against
            // the grey/green/white session rows; selected folder turns green.
            let (symbol_color, name_color) = if is_selected_workspace {
                (SELECTION_GREEN, SELECTION_GREEN)
            } else {
                (CORNFLOWER_BLUE, CORNFLOWER_BLUE)
            };

            let count_display = if total_count > 0 {
                format!(" ({})", total_count)
            } else {
                String::new()
            };

            // Favorite status: O(1) lookup against the precomputed cache
            // (resolved off the render path in
            // `AppState::recompute_favorite_workspaces`). No git2 / YAML here.
            let is_favorite = state.favorite_workspace_paths.contains(&workspace.path);
            let star_indicator = if is_favorite { "\u{f005} " } else { "" }; // fa-star, 1-cell

            let workspace_line = Line::from(vec![
                empty_badge(),
                Span::styled(workspace_symbol, Style::default().fg(symbol_color)),
                Span::styled(
                    " \u{f07b} ",
                    Style::default().fg(if is_selected_workspace {
                        GOLD
                    } else {
                        CORNFLOWER_BLUE
                    }),
                ),
                Span::styled(star_indicator, Style::default().fg(GOLD)),
                Span::styled(
                    workspace.name.clone(),
                    // Always bold — folder headers carry hierarchy weight.
                    Style::default().fg(name_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(count_display, Style::default().fg(MUTED_GRAY)),
            ]);

            items.push(ListItem::new(workspace_line));

            // Show sessions if workspace is expanded
            if is_expanded {
                // Apply the session filter; preserve original indices so the
                // selection match (`selected_session_index`) keeps pointing
                // at the correct entry in `workspace.sessions`.
                let visible: Vec<(usize, &crate::models::Session)> = workspace
                    .sessions
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| state.session_passes_filter(s))
                    .collect();
                let visible_len = visible.len();
                for (visible_pos, &(session_idx, session)) in visible.iter().enumerate() {
                    let is_selected_session =
                        is_selected_workspace && state.selected_session_index == Some(session_idx);
                    let is_last_session = visible_pos == visible_len - 1;

                    // Tree line characters with subdued color
                    let tree_prefix = if is_last_session { "└─" } else { "├─" };

                    let status_indicator = session.status.indicator();

                    // Mode indicator (controlled by show_container_status config).
                    // 1-cell Nerd Font glyphs (dev-docker / fa-desktop) instead of
                    // the 🐳/🖥️ emoji — emoji render 2-cell and the variation
                    // selector made width unpredictable across terminals.
                    let mode_indicator = if state.app_config.ui_preferences.show_container_status {
                        match session.mode {
                            SessionMode::Boss => "\u{e7b0} ",        // dev-docker
                            SessionMode::Interactive => "\u{f108} ", // fa-desktop
                        }
                    } else {
                        ""
                    };

                    // Git changes (controlled by show_git_status config)
                    let changes_text = if state.app_config.ui_preferences.show_git_status
                        && session.git_changes.total() > 0
                    {
                        format!(" ({})", session.git_changes.format())
                    } else {
                        String::new()
                    };

                    // Session state drives the row colour so active vs stopped
                    // reads at a glance: running = green, idle = soft white,
                    // stopped = muted grey, error = red. The selected row is
                    // always green (reinforced by the ▶ arrow + highlight bar).
                    let state_color = if is_selected_session {
                        SELECTION_GREEN
                    } else {
                        match session.status {
                            SessionStatus::Running => SELECTION_GREEN,
                            SessionStatus::Idle => SOFT_WHITE,
                            SessionStatus::Stopped => MUTED_GRAY,
                            SessionStatus::Error(_) => Color::Rgb(230, 100, 100),
                        }
                    };
                    let branch_color = state_color;
                    // Background behind the agent pill's powerline caps so they
                    // blend with the row (panel bg normally, highlight bar when
                    // selected).
                    let row_bg = if is_selected_session {
                        LIST_HIGHLIGHT_BG
                    } else {
                        DARK_BG
                    };

                    let agent_icon = session.agent_type.icon();
                    let agent_color = agent_brand_color(&session.agent_type);
                    // Agent chip: non-selected rows get the filled brand pill;
                    // the selected row drops the fill (bold brand glyph only) so
                    // there is no square box around the glyph on the highlight
                    // bar. Caps collapse to spaces to preserve column width.
                    let (pill_l, pill_r, agent_style) = if is_selected_session {
                        (
                            " ",
                            " ",
                            Style::default().fg(agent_color).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        (
                            PILL_LEFT,
                            PILL_RIGHT,
                            Style::default()
                                .fg(DARK_BG)
                                .bg(agent_color)
                                .add_modifier(Modifier::BOLD),
                        )
                    };
                    let is_multi_selected = state.selected_sessions.contains(&session.id);

                    let checkbox = ballot_checkbox(is_multi_selected);

                    // ainb-hooks attention marker for this session row.
                    // Live "needs you" marker, recomputed from the
                    // session's generating-state each refresh (NOT
                    // notification history): amber `[?]` on any session
                    // that is not actively generating (idle / turn-ended /
                    // at a prompt) and is therefore waiting on the user.
                    // Clears the instant generation resumes.
                    let session_alert = session.live_attention;

                    let mut session_spans = vec![
                        next_badge(&mut attach_no),
                        checkbox,
                        Span::styled(tree_prefix, Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            format!(" {} ", status_indicator),
                            Style::default().fg(state_color),
                        ),
                        Span::styled(mode_indicator.to_string(), Style::default().fg(MUTED_GRAY)),
                        // Coding-agent chip — brand colour carries identity
                        // (orange Claude, blue Gemini, …). Filled pill on normal
                        // rows; bold glyph only on the selected row (no square).
                        Span::styled(pill_l, Style::default().fg(agent_color).bg(row_bg)),
                        Span::styled(format!(" {} ", agent_icon), agent_style),
                        Span::styled(pill_r, Style::default().fg(agent_color).bg(row_bg)),
                        Span::raw(" "),
                        Span::styled(
                            session_list_name(session),
                            Style::default().fg(branch_color).add_modifier(
                                if is_selected_session {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                },
                            ),
                        ),
                        Span::styled(changes_text, Style::default().fg(WARNING_ORANGE)),
                    ];
                    if let Some(kind) = session_alert {
                        let (tag, color) = match kind {
                            AlertKind::NeedsPermission => ("[!]", ALERT_PERMISSION_RED),
                            AlertKind::WaitingOnUser => ("[?]", ALERT_WAITING_AMBER),
                            AlertKind::Finished => ("[✓]", SELECTION_GREEN),
                        };
                        session_spans.push(Span::raw("  "));
                        session_spans.push(Span::styled(
                            tag.to_string(),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ));
                    }
                    let session_line = Line::from(session_spans);

                    items.push(ListItem::new(session_line));
                }

                // Render workspace shell (single shell per workspace)
                if let Some(shell_session) = &workspace.shell_session {
                    let is_selected_shell = is_selected_workspace
                        && state.selected_session_index.is_none()
                        && state.shell_selected;

                    // Shell is always last
                    let tree_prefix = "└─";

                    // Status indicator
                    let status_indicator = shell_session.status.indicator();
                    let (name_color, _prefix_color) = if is_selected_shell {
                        (SELECTION_GREEN, SELECTION_GREEN)
                    } else {
                        match shell_session.status {
                            ShellSessionStatus::Running => (SELECTION_GREEN, GOLD),
                            ShellSessionStatus::Detached => (SOFT_WHITE, MUTED_GRAY),
                            ShellSessionStatus::Stopped => (MUTED_GRAY, MUTED_GRAY),
                        }
                    };

                    let shell_line = Line::from(vec![
                        next_badge(&mut attach_no),
                        Span::styled("  ", Style::default()),
                        Span::styled(tree_prefix, Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            format!(" {} ", status_indicator),
                            Style::default().fg(name_color),
                        ),
                        Span::styled(
                            shell_session.name.clone(),
                            Style::default().fg(name_color).add_modifier(if is_selected_shell {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                        ),
                    ]);

                    items.push(ListItem::new(shell_line));
                }
            }
        }

        // Add "SSH Sessions" section if there are SSH sessions
        if !state.ssh_sessions.is_empty() {
            // Add separator line
            if !items.is_empty() {
                items.push(ListItem::new(Line::from("")));
            }

            let session_count = state.ssh_sessions.len();
            let is_selected_ssh = state.selected_workspace_index.is_none()
                && state.selected_other_tmux_index.is_none()
                && state.selected_ssh_session_index.is_some();

            let ssh_symbol = if state.ssh_sessions_expanded {
                "▼"
            } else {
                "▶"
            };

            // Orange color scheme for SSH section
            let ssh_header_color = if is_selected_ssh || state.selected_ssh_session_index.is_some()
            {
                WARNING_ORANGE
            } else {
                MUTED_GRAY
            };

            let ssh_header = Line::from(vec![
                empty_badge(),
                Span::styled(ssh_symbol, Style::default().fg(ssh_header_color)),
                Span::styled(" 🔐 ", Style::default().fg(ssh_header_color)),
                Span::styled(
                    "SSH Sessions ",
                    Style::default().fg(ssh_header_color).add_modifier(if is_selected_ssh {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
                Span::styled(
                    format!("({})", session_count),
                    Style::default().fg(MUTED_GRAY),
                ),
            ]);

            items.push(ListItem::new(ssh_header));

            // Show SSH sessions if expanded
            if state.ssh_sessions_expanded {
                let session_len = state.ssh_sessions.len();
                for (idx, ssh_session) in state.ssh_sessions.iter().enumerate() {
                    let is_selected =
                        is_selected_ssh && state.selected_ssh_session_index == Some(idx);
                    let is_last = idx == session_len - 1;
                    let is_being_renamed = is_selected && state.ssh_session_rename_mode;

                    let tree_prefix = if is_last { "└─" } else { "├─" };

                    // Use display_name if set, otherwise fall back to ssh_target.display_name() or name
                    let display_text = if is_being_renamed {
                        // Show inline rename editor with cursor
                        format!("✏️ {}_", state.ssh_session_rename_buffer)
                    } else {
                        ssh_session.display_name.clone().unwrap_or_else(|| {
                            if let Some(ref target) = ssh_session.ssh_target {
                                target.display_name()
                            } else {
                                ssh_session.name.clone()
                            }
                        })
                    };

                    let status_icon = match ssh_session.status {
                        SessionStatus::Running => "🟢",
                        SessionStatus::Stopped => "⚫",
                        SessionStatus::Error(_) => "🔴",
                        _ => "○",
                    };

                    let name_color = if is_being_renamed {
                        GOLD // Gold for rename mode
                    } else if is_selected {
                        SELECTION_GREEN
                    } else if ssh_session.status == SessionStatus::Running {
                        WARNING_ORANGE
                    } else {
                        MUTED_GRAY
                    };

                    let session_line = Line::from(vec![
                        next_badge(&mut attach_no),
                        Span::styled("  ", Style::default()),
                        Span::styled(tree_prefix, Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(format!(" {} ", status_icon), Style::default()),
                        Span::styled(
                            display_text,
                            Style::default().fg(name_color).add_modifier(
                                if is_selected || is_being_renamed {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                },
                            ),
                        ),
                    ]);

                    items.push(ListItem::new(session_line));
                }
            }
        }

        // Add "Other tmux" section if there are other tmux sessions
        if !state.other_tmux_sessions.is_empty() {
            // Add separator line
            if !items.is_empty() {
                items.push(ListItem::new(Line::from("")));
            }

            let session_count = state.other_tmux_sessions.len();
            let is_selected_other = state.selected_workspace_index.is_none()
                && state.selected_other_tmux_index.is_some();

            let other_symbol = if state.other_tmux_expanded {
                "▼"
            } else {
                "▶"
            };

            let header_color = if state.selected_workspace_index.is_none() {
                CORNFLOWER_BLUE
            } else {
                MUTED_GRAY
            };

            let other_header = Line::from(vec![
                empty_badge(),
                Span::styled(other_symbol, Style::default().fg(header_color)),
                Span::styled(" 🖥️ ", Style::default().fg(header_color)),
                Span::styled(
                    "Other tmux ",
                    Style::default().fg(header_color).add_modifier(if is_selected_other {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
                Span::styled(
                    format!("({})", session_count),
                    Style::default().fg(MUTED_GRAY),
                ),
            ]);

            items.push(ListItem::new(other_header));

            // Show other tmux sessions if expanded
            if state.other_tmux_expanded {
                let session_len = state.other_tmux_sessions.len();
                for (idx, other_session) in state.other_tmux_sessions.iter().enumerate() {
                    let is_selected =
                        is_selected_other && state.selected_other_tmux_index == Some(idx);
                    let is_multi_selected =
                        state.selected_other_tmux_sessions.contains(&other_session.name);
                    let is_last = idx == session_len - 1;

                    let tree_prefix = if is_last { "└─" } else { "├─" };
                    let status = other_session.status_indicator();

                    let windows_text = if other_session.windows > 1 {
                        format!(" ({}w)", other_session.windows)
                    } else {
                        String::new()
                    };

                    let name_color = if is_selected {
                        SELECTION_GREEN
                    } else if other_session.attached {
                        CORNFLOWER_BLUE
                    } else {
                        MUTED_GRAY
                    };

                    // Check if this session is being renamed
                    let is_being_renamed = is_selected && state.other_tmux_rename_mode;

                    let badge = next_badge(&mut attach_no);
                    let checkbox = ballot_checkbox(is_multi_selected);
                    let session_line = if is_being_renamed {
                        // Show inline rename input
                        Line::from(vec![
                            badge,
                            checkbox,
                            Span::styled(tree_prefix, Style::default().fg(SUBDUED_BORDER)),
                            Span::styled(format!(" {} ", status), Style::default()),
                            Span::styled("✏️ ", Style::default()),
                            Span::styled(
                                format!("{}_", state.other_tmux_rename_buffer),
                                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                            ),
                        ])
                    } else {
                        Line::from(vec![
                            badge,
                            checkbox,
                            Span::styled(tree_prefix, Style::default().fg(SUBDUED_BORDER)),
                            Span::styled(format!(" {} ", status), Style::default()),
                            Span::styled(
                                other_session.name.clone(),
                                Style::default().fg(name_color).add_modifier(if is_selected {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                }),
                            ),
                            Span::styled(windows_text, Style::default().fg(MUTED_GRAY)),
                        ])
                    };

                    items.push(ListItem::new(session_line));
                }
            }
        }

        if items.is_empty() {
            // Distinguish "still loading" from "loaded but empty" — the
            // background workspace scan can take a few seconds on cold
            // launch and a bare "No workspaces found" line during that
            // window reads as "you have nothing here" when the truth is
            // "we haven't looked yet". Spinner matches the one used in
            // the home screen's recent-activity strip so the two
            // surfaces share a vocabulary.
            let empty_line = if state.is_loading_workspaces {
                let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let frame_idx = (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() / 100)
                    .unwrap_or(0)
                    % spinner_frames.len() as u128) as usize;
                Line::from(vec![
                    Span::styled(
                        format!("{} ", spinner_frames[frame_idx]),
                        Style::default().fg(GOLD),
                    ),
                    Span::styled(
                        "Loading workspaces…",
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled("✨ ", Style::default().fg(MUTED_GRAY)),
                    Span::styled(
                        "No workspaces found",
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                ])
            };
            items.push(ListItem::new(empty_line));
        }

        items
    }

    fn update_selection(&mut self, state: &AppState) {
        let Some(selected) = selected_row_target(state) else {
            self.list_state.select(None);
            return;
        };
        let item_count = Self::build_list_items_static(state).len();
        let selected_index =
            (0..item_count).find(|&index| state.session_list_row_target(index) == Some(selected));
        self.list_state.select(selected_index);

        let workspace_header = match selected {
            SessionListRowTarget::Attachable(
                AttachableRef::WorkspaceSession { workspace_idx, .. }
                | AttachableRef::WorkspaceShell { workspace_idx },
            ) => Some(SessionListRowTarget::WorkspaceHeader { workspace_idx }),
            _ => None,
        };
        if let Some(workspace_header) = workspace_header {
            if let Some(header_index) = (0..item_count)
                .find(|&index| state.session_list_row_target(index) == Some(workspace_header))
            {
                if self.list_state.offset() > header_index {
                    *self.list_state.offset_mut() = header_index;
                }
            }
        }
    }

    /// Calculate total visible items for navigation
    pub fn total_visible_items(state: &AppState) -> usize {
        let mut count = 0;

        // Count workspace items
        for workspace in &state.workspaces {
            count += 1; // Workspace header
            if state.expand_all_workspaces {
                count += workspace.sessions.len();
                if workspace.shell_session.is_some() {
                    count += 1;
                }
            }
        }

        // Count "SSH Sessions" section items
        if !state.ssh_sessions.is_empty() {
            if !state.workspaces.is_empty() {
                count += 1; // Empty separator line
            }
            count += 1; // "SSH Sessions" header
            if state.ssh_sessions_expanded {
                count += state.ssh_sessions.len();
            }
        }

        // Count "Other tmux" section items
        if !state.other_tmux_sessions.is_empty() {
            let has_items_above = !state.workspaces.is_empty() || !state.ssh_sessions.is_empty();
            if has_items_above {
                count += 1; // Empty separator line
            }
            count += 1; // "Other tmux" header
            if state.other_tmux_expanded {
                count += state.other_tmux_sessions.len();
            }
        }

        count
    }
}

#[allow(dead_code)]
fn workspace_running_count(workspace: &Workspace) -> usize {
    workspace.running_sessions().len()
}

/// User label followed by the current Git branch, or only the branch when unlabeled.
fn session_list_name(session: &Session) -> String {
    session
        .display_name
        .as_ref()
        .map(|label| format!("{label} · {}", session.branch_name))
        .unwrap_or_else(|| session.branch_name.clone())
}

fn context_action_label(action: SessionContextAction) -> &'static str {
    match action {
        SessionContextAction::Attach => "Attach",
        SessionContextAction::Restart => "Restart",
        SessionContextAction::EditLabel => "Rename session label",
        SessionContextAction::OpenEditor => "Open editor",
        SessionContextAction::OpenShell => "Open shell",
        SessionContextAction::OpenGit => "Git",
        SessionContextAction::QuickCommit => "Quick commit",
        SessionContextAction::Delete => "Delete",
    }
}

/// Multi-select ballot toggle shared by the workspace-session and
/// other-tmux rows: ☑ (green, marked) / ☐ (muted, unmarked).
fn ballot_checkbox(checked: bool) -> Span<'static> {
    if checked {
        Span::styled(
            "☑ ",
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("☐ ", Style::default().fg(MUTED_GRAY))
    }
}

/// Brand color for a coding agent's pill chip. Drives the filled-block
/// identity in the session list (orange Claude, blue Gemini, …).
fn agent_brand_color(agent: &SessionAgentType) -> Color {
    match agent {
        SessionAgentType::Claude => BRAND_CLAUDE,
        SessionAgentType::Codex => BRAND_CODEX,
        SessionAgentType::Copilot => BRAND_COPILOT,
        SessionAgentType::Gemini => BRAND_GEMINI,
        SessionAgentType::Antigravity => BRAND_ANTIGRAVITY,
        SessionAgentType::Kiro => BRAND_KIRO,
        SessionAgentType::Shell => BRAND_SHELL,
        SessionAgentType::Ssh => BRAND_SSH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::SessionFilter;
    use crate::models::{Session, SessionStatus};

    #[test]
    fn selection_uses_visible_row_when_stopped_sessions_are_filtered() {
        let mut state = AppState::new();
        state.workspaces.clear();
        state.session_filter = SessionFilter::ActiveOnly;
        state.expand_all_workspaces = true;

        let mut workspace = Workspace::new("workspace".to_string(), "/tmp/workspace".into());
        let stopped = Session::new("stopped".to_string(), "/tmp/workspace".to_string());
        let mut running = Session::new("running".to_string(), "/tmp/workspace".to_string());
        running.status = SessionStatus::Running;
        workspace.add_session(stopped);
        workspace.add_session(running);
        state.workspaces.push(workspace);
        state.selected_workspace_index = Some(0);
        state.selected_session_index = Some(1);

        let mut list = SessionListComponent::new();
        list.update_selection(&state);

        let selected_row = list.list_state.selected().expect("visible selection");
        assert_eq!(
            state.session_list_row_target(selected_row),
            Some(SessionListRowTarget::Attachable(
                AttachableRef::WorkspaceSession {
                    workspace_idx: 0,
                    session_idx: 1,
                }
            ))
        );
    }

    #[test]
    fn durable_label_keeps_live_branch_visible() {
        let mut session = Session::new("workspace".to_string(), "/tmp/workspace".to_string());
        session.branch_name = "fix/rpc-acp-flake".to_string();
        session.display_name = Some("RPC flake".to_string());

        assert_eq!(session_list_name(&session), "RPC flake · fix/rpc-acp-flake");
    }
}
