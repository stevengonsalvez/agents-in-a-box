// ABOUTME: Session list component for displaying workspaces and sessions in hierarchical view

#![allow(dead_code)]

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState},
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

use ainb_plugin_notifyd::AlertKind;

use crate::app::AppState;
use crate::models::{SessionMode, SessionStatus, ShellSessionStatus, Workspace};

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
            Span::styled(" 📁 ", Style::default().fg(GOLD)),
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
        if let Some(label) = state.session_filter.title_label() {
            title_spans.push(Span::raw(" "));
            title_spans.push(Span::styled(
                format!("[{}]", label),
                Style::default().fg(GOLD),
            ));
        }
        title_spans.push(Span::raw(" "));
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
                        if state.ssh_session_rename_mode || state.other_tmux_rename_mode {
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

            // Premium workspace styling
            let (symbol_color, name_color) = if is_selected_workspace {
                (SELECTION_GREEN, SELECTION_GREEN)
            } else {
                (MUTED_GRAY, SOFT_WHITE)
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
            let star_indicator = if is_favorite { "⭐ " } else { "" };

            let workspace_line = Line::from(vec![
                empty_badge(),
                Span::styled(workspace_symbol, Style::default().fg(symbol_color)),
                Span::styled(
                    " 📁 ",
                    Style::default().fg(if is_selected_workspace {
                        GOLD
                    } else {
                        CORNFLOWER_BLUE
                    }),
                ),
                Span::styled(star_indicator, Style::default().fg(GOLD)),
                Span::styled(
                    workspace.name.clone(),
                    Style::default().fg(name_color).add_modifier(if is_selected_workspace {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
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

                    // Mode indicator (controlled by show_container_status config)
                    let mode_indicator = if state.app_config.ui_preferences.show_container_status {
                        match session.mode {
                            SessionMode::Boss => "🐳 ",
                            SessionMode::Interactive => "🖥️ ",
                        }
                    } else {
                        ""
                    };

                    // Tmux status indicator
                    let tmux_indicator = if session.is_attached {
                        "🔗"
                    } else if session.tmux_session_name.is_some() {
                        "●"
                    } else {
                        "○"
                    };

                    // Git changes (controlled by show_git_status config)
                    let changes_text = if state.app_config.ui_preferences.show_git_status
                        && session.git_changes.total() > 0
                    {
                        format!(" ({})", session.git_changes.format())
                    } else {
                        String::new()
                    };

                    // Premium session styling
                    let (branch_color, tmux_color) = if is_selected_session {
                        (SELECTION_GREEN, SELECTION_GREEN)
                    } else {
                        match session.status {
                            SessionStatus::Running => (SELECTION_GREEN, SOFT_WHITE),
                            SessionStatus::Stopped => (MUTED_GRAY, MUTED_GRAY),
                            SessionStatus::Idle => (WARNING_ORANGE, SOFT_WHITE),
                            SessionStatus::Error(_) => (Color::Rgb(230, 100, 100), SOFT_WHITE),
                        }
                    };

                    let agent_icon = session.agent_type.icon();
                    let is_multi_selected = state.selected_sessions.contains(&session.id);

                    let checkbox = if is_multi_selected {
                        Span::styled(
                            "[x]",
                            Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::styled("[ ]", Style::default().fg(MUTED_GRAY))
                    };

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
                        Span::styled(format!(" {} ", status_indicator), Style::default()),
                        Span::styled(mode_indicator.to_string(), Style::default()),
                        Span::styled(format!("{} ", agent_icon), Style::default()),
                        Span::styled(
                            format!("{} ", tmux_indicator),
                            Style::default().fg(tmux_color),
                        ),
                        Span::styled(
                            session.branch_name.clone(),
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
                    let checkbox = if is_multi_selected {
                        Span::styled(
                            "[x]",
                            Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::styled("[ ]", Style::default().fg(MUTED_GRAY))
                    };
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
        if let Some(workspace_idx) = state.selected_workspace_index {
            let mut current_index = 0;
            let mut workspace_header_index = 0;

            // When expand_all is true, we need to count items from all workspaces
            for (idx, workspace) in state.workspaces.iter().enumerate() {
                if idx == workspace_idx {
                    // Found the selected workspace
                    current_index += idx; // Add workspace line itself (accounting for skipped sessions)

                    // When expand_all, add all sessions from prior workspaces
                    if state.expand_all_workspaces {
                        for prior_workspace in state.workspaces.iter().take(idx) {
                            current_index += prior_workspace.sessions.len();
                            if prior_workspace.shell_session.is_some() {
                                current_index += 1;
                            }
                        }
                    }

                    // Remember the workspace header position before adding session offset
                    workspace_header_index = current_index;

                    // Add session offset if a regular session is selected
                    if let Some(session_idx) = state.selected_session_index {
                        current_index += session_idx + 1;
                    } else if state.shell_selected {
                        // Shell selected: add all regular sessions + 1 for shell
                        current_index += workspace.sessions.len() + 1;
                    }
                    break;
                }
            }

            self.list_state.select(Some(current_index));

            // Ensure the parent workspace header stays visible when a child session is selected.
            // ratatui auto-scrolls to show the selected item, but this can push the parent
            // folder line off the top of the viewport. Clamp the scroll offset so the
            // workspace header is always the topmost visible item (at minimum).
            if state.selected_session_index.is_some() || state.shell_selected {
                let current_offset = self.list_state.offset();
                if current_offset > workspace_header_index {
                    *self.list_state.offset_mut() = workspace_header_index;
                }
            }
        } else if state.selected_ssh_session_index.is_some() {
            // Selection is in "SSH Sessions" section
            let mut current_index = 0;

            // Count all workspace items first
            for workspace in &state.workspaces {
                current_index += 1; // Workspace header
                if state.expand_all_workspaces {
                    current_index += workspace.sessions.len();
                    if workspace.shell_session.is_some() {
                        current_index += 1;
                    }
                }
            }

            // Add separator + "SSH Sessions" header
            if !state.workspaces.is_empty() && !state.ssh_sessions.is_empty() {
                current_index += 1; // Empty separator line
            }
            current_index += 1; // "SSH Sessions" header

            // Add offset for selected SSH session
            if let Some(ssh_idx) = state.selected_ssh_session_index {
                current_index += ssh_idx;
            }

            self.list_state.select(Some(current_index));
        } else if state.selected_other_tmux_index.is_some() {
            // Selection is in "Other tmux" section
            let mut current_index = 0;

            // Count all workspace items first
            for workspace in &state.workspaces {
                current_index += 1; // Workspace header
                if state.expand_all_workspaces {
                    current_index += workspace.sessions.len();
                    if workspace.shell_session.is_some() {
                        current_index += 1;
                    }
                }
            }

            // Add SSH Sessions section
            if !state.ssh_sessions.is_empty() {
                if !state.workspaces.is_empty() {
                    current_index += 1; // Empty separator line
                }
                current_index += 1; // "SSH Sessions" header
                if state.ssh_sessions_expanded {
                    current_index += state.ssh_sessions.len();
                }
            }

            // Add separator + "Other tmux" header
            let has_items_above = !state.workspaces.is_empty() || !state.ssh_sessions.is_empty();
            if has_items_above && !state.other_tmux_sessions.is_empty() {
                current_index += 1; // Empty separator line
            }
            current_index += 1; // "Other tmux" header

            // Add offset for selected other session
            if let Some(other_idx) = state.selected_other_tmux_index {
                current_index += other_idx;
            }

            self.list_state.select(Some(current_index));
        } else {
            self.list_state.select(None);
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
