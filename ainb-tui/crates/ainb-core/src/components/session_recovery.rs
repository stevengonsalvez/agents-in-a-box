// ABOUTME: Session recovery component for recovering orphaned agent sessions after crash/shutdown
// Displays orphaned sessions (tmux dead, worktree exists) and orphaned worktrees (broken symlinks, no container)
// Allows resume/cleanup actions for both types

pub use ainb_app::components::session_recovery::*;

use crate::app::ui_state::UiState;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

use crate::config::screen_model::fuzzy_matches;
use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::models::SessionAgentType;

// Color palette (matching TUI style guide)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// Session recovery component renderer
pub struct SessionRecovery;

impl SessionRecovery {
    pub fn render(frame: &mut Frame, area: Rect, state: &SessionRecoveryState, ui: &mut UiState) {
        // Main layout: list on left, details on right
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);

        Self::render_session_list(frame, chunks[0], state, ui);
        Self::render_session_details(frame, chunks[1], state);
    }

    fn render_session_list(
        frame: &mut Frame,
        area: Rect,
        state: &SessionRecoveryState,
        ui: &mut UiState,
    ) {
        // Visible counts, not raw ones: a header saying (5) over a filtered
        // list of 1 reads as a broken filter.
        let session_count = state.visible_sessions().len();
        let worktree_count = state.visible_worktrees().len();
        let total_count = state.current_view_count();

        // Build title based on view mode
        let title_text = match state.view_mode {
            RecoveryViewMode::Sessions => "Sessions",
            RecoveryViewMode::Worktrees => "Worktrees",
            RecoveryViewMode::All => "All Orphans",
        };

        let count_text = match state.view_mode {
            RecoveryViewMode::Sessions => format!("({})", session_count),
            RecoveryViewMode::Worktrees => format!("({})", worktree_count),
            RecoveryViewMode::All => format!("({}/{})", session_count, worktree_count),
        };

        // Dynamic action label based on what's selected
        let action_label = if state.is_worktree_selected() {
            " delete "
        } else {
            " archive "
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG))
            .title(Line::from(vec![
                Span::styled("🔄 ", Style::default().fg(GOLD)),
                Span::styled(
                    title_text,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default()),
                Span::styled(
                    count_text,
                    Style::default()
                        .fg(if total_count > 0 {
                            WARNING_ORANGE
                        } else {
                            SELECTION_GREEN
                        })
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .title_bottom(Line::from(vec![
                // Filter leads the footer. Appended last it fell off the right
                // edge at 100 and 140 cols, hiding the only on-screen
                // affordance for the feature.
                Span::styled("/", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" filter ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(
                    " Tab",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" view ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" r", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" resume ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" d", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(action_label, Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" R", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" refresh ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" A", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" recover all ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(
                    " Space",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" select ", Style::default().fg(MUTED_GRAY)),
                Span::styled("|", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(" D", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" del selected ", Style::default().fg(MUTED_GRAY)),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Layout: tabs at top, optional search row, then list. The search row
        // is zero-height while no filter is in play, so an empty query renders
        // exactly the panel it rendered before search existed.
        let show_search = state.search_active || !state.search_query.is_empty();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(u16::from(show_search)),
                Constraint::Min(0),
            ])
            .split(inner);

        // Render tabs
        Self::render_view_tabs(frame, layout[0], state);

        if show_search {
            let mut spans = vec![
                Span::styled("/", Style::default().fg(GOLD)),
                Span::styled(
                    state.search_query.clone(),
                    Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                ),
            ];
            if state.search_active {
                spans.push(Span::styled("▏", Style::default().fg(SELECTION_GREEN)));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), layout[1]);
        }

        // Loading state
        if state.loading {
            let loading = Paragraph::new("Loading...").style(Style::default().fg(MUTED_GRAY));
            frame.render_widget(loading, layout[2]);
            return;
        }

        // Empty state. A filter that hides everything is not the same news as
        // having nothing to recover, so it gets its own line.
        if total_count == 0 && !state.search_query.is_empty() {
            let no_matches = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("No matches for \"{}\"", state.search_query),
                    Style::default().fg(WARNING_ORANGE),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Esc clears the filter.",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ]);
            frame.render_widget(no_matches, layout[2]);
            return;
        }

        if total_count == 0 {
            let empty_msg = match state.view_mode {
                RecoveryViewMode::Sessions => "No orphaned sessions found",
                RecoveryViewMode::Worktrees => "No orphaned worktrees found",
                RecoveryViewMode::All => "No orphaned items found",
            };
            let empty_state = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("✓ {}", empty_msg),
                    Style::default().fg(SELECTION_GREEN),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "All items are either active or cleaned up.",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ]);
            frame.render_widget(empty_state, layout[2]);
            return;
        }

        // Build list items based on view mode
        let items = Self::build_list_items(state, layout[2].width);
        let list = List::new(items);

        // Selection is core's; only the scroll offset is the widget's own.
        // The clamp is what `after_query_change` used to do by resetting the
        // offset: a filter can shrink the list under a scrolled viewport, and
        // an offset past the end paints an empty pane.
        let list_state = &mut ui.session_recovery_list;
        list_state.select((total_count > 0).then_some(state.selected_index));
        let offset = list_state.offset_mut();
        *offset = (*offset).min(total_count.saturating_sub(1));
        frame.render_stateful_widget(list, layout[2], list_state);

        // Render recovery overlay on top if present
        if let Some(ref overlay) = state.recovery_overlay {
            Self::render_recovery_overlay(frame, area, overlay);
        }
    }

    /// Render the recovery results overlay as a centered popup
    fn render_recovery_overlay(frame: &mut Frame, area: Rect, overlay: &RecoveryOverlay) {
        // Size the popup
        let popup_width = (area.width * 70 / 100).min(80).max(40);
        let popup_height = (overlay.results.len() as u16 + 6).min(area.height * 70 / 100).max(8);
        let x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let y = (area.height.saturating_sub(popup_height)) / 2 + area.y;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        // Clear background
        frame.render_widget(ratatui::widgets::Clear, popup_area);

        let has_failures = overlay.results.iter().any(|r| !r.success);
        let border_color = if has_failures {
            WARNING_ORANGE
        } else {
            SELECTION_GREEN
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG))
            .title(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    &overlay.title,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default()),
            ]))
            .title_bottom(Line::from(vec![
                Span::styled(
                    " Esc",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" dismiss ", Style::default().fg(MUTED_GRAY)),
            ]));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let mut lines = Vec::new();
        lines.push(Line::from(""));

        for result in &overlay.results {
            let (icon, color) = if result.success {
                ("✓", SELECTION_GREEN)
            } else {
                ("✗", WARNING_ORANGE)
            };

            // Truncate name to fit
            let max_name = (inner.width as usize).saturating_sub(6);
            let display_name = if result.name.len() > max_name {
                format!("{}…", &result.name[..max_name.saturating_sub(1)])
            } else {
                result.name.clone()
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", icon),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(display_name, Style::default().fg(SOFT_WHITE)),
            ]));

            // Show detail on next line for failures
            if !result.success {
                let detail_max = (inner.width as usize).saturating_sub(8);
                let detail = if result.detail.len() > detail_max {
                    format!("{}…", &result.detail[..detail_max.saturating_sub(1)])
                } else {
                    result.detail.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled("      ", Style::default()),
                    Span::styled(
                        detail,
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, inner);
    }

    /// Render the view mode tabs
    fn render_view_tabs(frame: &mut Frame, area: Rect, state: &SessionRecoveryState) {
        let session_count = state.visible_sessions().len();
        let worktree_count = state.visible_worktrees().len();

        let tab_titles = vec![
            format!("Sessions ({})", session_count),
            format!("Worktrees ({})", worktree_count),
            format!("All ({})", session_count + worktree_count),
        ];

        let selected_idx = match state.view_mode {
            RecoveryViewMode::Sessions => 0,
            RecoveryViewMode::Worktrees => 1,
            RecoveryViewMode::All => 2,
        };

        let tabs = Tabs::new(tab_titles)
            .select(selected_idx)
            .style(Style::default().fg(MUTED_GRAY))
            .highlight_style(
                Style::default()
                    .fg(GOLD)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED),
            )
            .divider(Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)));

        frame.render_widget(tabs, area);
    }

    /// Build list items based on current view mode
    fn build_list_items(state: &SessionRecoveryState, pane_width: u16) -> Vec<ListItem<'static>> {
        let budget = Self::row_name_budget(pane_width);
        let mut items = Vec::new();
        let mut current_idx = 0;
        // Iterate the SAME filtered sets row_at resolves against, so a list
        // position and a selected_index always mean the same row.
        let sessions = state.visible_sessions();
        let worktrees = state.visible_worktrees();

        // Add sessions if in Sessions or All view
        if matches!(
            state.view_mode,
            RecoveryViewMode::Sessions | RecoveryViewMode::All
        ) {
            for session in sessions.iter().map(|i| &state.orphaned_sessions[*i]) {
                let is_selected = current_idx == state.selected_index;
                let is_marked = state.selected_items.contains(&current_idx);
                items.push(Self::render_session_item(
                    session,
                    is_selected,
                    is_marked,
                    budget,
                ));
                current_idx += 1;
            }
        }

        // Add worktrees if in Worktrees or All view
        if matches!(
            state.view_mode,
            RecoveryViewMode::Worktrees | RecoveryViewMode::All
        ) {
            // Add separator in All view
            if state.view_mode == RecoveryViewMode::All
                && !sessions.is_empty()
                && !worktrees.is_empty()
            {
                items.push(ListItem::new(Line::from(vec![
                    Span::styled("── ", Style::default().fg(SUBDUED_BORDER)),
                    Span::styled(
                        "Worktrees ",
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled("──────────────────", Style::default().fg(SUBDUED_BORDER)),
                ])));
                current_idx += 1; // Separator occupies a list position
            }

            for worktree in worktrees.iter().map(|i| &state.orphaned_worktrees[*i]) {
                let is_selected = current_idx == state.selected_index;
                let is_marked = state.selected_items.contains(&current_idx);
                items.push(Self::render_worktree_item(
                    worktree,
                    is_selected,
                    is_marked,
                    budget,
                ));
                current_idx += 1;
            }
        }

        items
    }

    /// Columns a list row may spend on "<label> · <name>", derived from the
    /// pane it is painted into. A row spends 8 columns on cursor, checkbox and
    /// status glyph and 10 on the trailing " (2h ago)", so the string gets
    /// whatever is left. Floor of 12 keeps a pathologically narrow pane from
    /// collapsing the name to nothing.
    ///
    /// Fixed at 25 before this was width-aware, which was right at 100 columns
    /// and wasted half a wide terminal's pane on every labelled row. The 10
    /// reproduces that 25 exactly at 100 columns; a four-digit age
    /// (" (1440m ago)") is one column over and clips as it did before.
    fn row_name_budget(pane_width: u16) -> usize {
        (pane_width as usize).saturating_sub(8 + 10).max(12)
    }

    /// Name with its durable session label in front, matching the session
    /// list's "<label> · <name>" shape. An unlabeled row returns the bare name,
    /// so it keeps exactly the columns it has today with no stray separator.
    fn with_label(label: Option<&String>, name: &str, budget: usize) -> String {
        let Some(label) = label else {
            return name.chars().take(budget).collect();
        };
        // The two halves share ONE budget. Giving each of them the full budget
        // pushed " (2h ago)" off the row, and age is the signal an operator
        // sorts a recovery list by. Label is capped at half so the name it
        // prefixes never collapses to a couple of characters.
        let label: String = label.chars().take(budget / 2).collect();
        let name_budget = budget.saturating_sub(label.chars().count() + 3);
        format!(
            "{label} · {}",
            name.chars().take(name_budget).collect::<String>()
        )
    }

    /// Render a single session list item
    fn render_session_item(
        session: &OrphanedSession,
        is_selected: bool,
        is_marked: bool,
        budget: usize,
    ) -> ListItem<'static> {
        let resume_indicator = if session.can_resume { "📄" } else { "⚠" };
        let time_indicator = if session.time_ago.is_empty() {
            String::new()
        } else {
            format!(" ({})", session.time_ago)
        };

        let task_preview: String =
            session.task.chars().take(30).collect::<String>().replace('\n', " ");

        let mut spans = vec![];
        // Cursor indicator
        if is_selected {
            spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
        } else {
            spans.push(Span::raw("  "));
        }
        // Multi-select checkbox
        if is_marked {
            spans.push(Span::styled(
                "[x] ",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled("[ ] ", Style::default().fg(MUTED_GRAY)));
        }

        spans.push(Span::styled(
            resume_indicator,
            if session.can_resume {
                Style::default().fg(SELECTION_GREEN)
            } else {
                Style::default().fg(WARNING_ORANGE)
            },
        ));
        spans.push(Span::raw(" "));

        spans.push(Span::styled(
            Self::with_label(session.label.as_ref(), &session.session, budget),
            if is_selected {
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            },
        ));

        spans.push(Span::styled(
            time_indicator,
            Style::default().fg(MUTED_GRAY),
        ));

        let base_style = if is_selected {
            Style::default().bg(LIST_HIGHLIGHT_BG)
        } else {
            Style::default()
        };

        ListItem::new(vec![
            Line::from(spans),
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!("{}...", task_preview),
                    Style::default().fg(MUTED_GRAY),
                ),
            ]),
        ])
        .style(base_style)
    }

    /// Render a single worktree list item
    fn render_worktree_item(
        worktree: &OrphanedWorktree,
        is_selected: bool,
        is_marked: bool,
        budget: usize,
    ) -> ListItem<'static> {
        // Determine if worktree is resumable (not a broken symlink)
        let can_resume = worktree.orphan_type != OrphanType::BrokenSymlink;
        let resume_indicator = if can_resume { "▶" } else { "✗" };
        let time_indicator = if worktree.time_ago.is_empty() {
            String::new()
        } else {
            format!(" ({})", worktree.time_ago)
        };

        let mut spans = vec![];
        // Cursor indicator
        if is_selected {
            spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
        } else {
            spans.push(Span::raw("  "));
        }
        // Multi-select checkbox
        if is_marked {
            spans.push(Span::styled(
                "[x] ",
                Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled("[ ] ", Style::default().fg(MUTED_GRAY)));
        }

        // Show resume indicator
        spans.push(Span::styled(
            resume_indicator,
            if can_resume {
                Style::default().fg(SELECTION_GREEN)
            } else {
                Style::default().fg(WARNING_ORANGE)
            },
        ));
        spans.push(Span::raw(" "));

        // with_label owns the width budget for both halves of the string.
        spans.push(Span::styled(
            Self::with_label(worktree.label.as_ref(), &worktree.name, budget),
            if is_selected {
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            },
        ));

        spans.push(Span::styled(
            time_indicator,
            Style::default().fg(MUTED_GRAY),
        ));

        let base_style = if is_selected {
            Style::default().bg(LIST_HIGHLIGHT_BG)
        } else {
            Style::default()
        };

        // Second line: branch or type label
        let branch_line = if let Some(ref branch) = worktree.branch {
            let branch_display: String = branch.chars().take(30).collect();
            Line::from(vec![
                Span::raw("    "),
                Span::styled(" ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(branch_display, Style::default().fg(MUTED_GRAY)),
            ])
        } else {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    worktree.orphan_type.label().to_string(),
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                ),
            ])
        };

        ListItem::new(vec![Line::from(spans), branch_line]).style(base_style)
    }

    fn render_session_details(frame: &mut Frame, area: Rect, state: &SessionRecoveryState) {
        // Dynamic title based on what's selected
        let title = if state.is_worktree_selected() {
            "Worktree Details"
        } else {
            "Session Details"
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG))
            .title(Line::from(vec![
                Span::styled("📋 ", Style::default().fg(GOLD)),
                Span::styled(
                    title,
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Show action result if any
        if let Some(ref result) = state.action_result {
            let result_area = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(inner);

            let result_widget = Paragraph::new(Line::from(vec![
                Span::styled("✓ ", Style::default().fg(SELECTION_GREEN)),
                Span::styled(result, Style::default().fg(SELECTION_GREEN)),
            ]))
            .wrap(Wrap { trim: true });

            frame.render_widget(result_widget, result_area[0]);

            // Show details below result
            if let Some(session) = state.selected() {
                Self::render_session_info(frame, result_area[1], session);
            } else if let Some(worktree) = state.selected_worktree() {
                Self::render_worktree_info(frame, result_area[1], worktree);
            }
            return;
        }

        // Show error if any
        if let Some(ref error) = state.last_error {
            let error_widget = Paragraph::new(Line::from(vec![
                Span::styled("⚠ Error: ", Style::default().fg(WARNING_ORANGE)),
                Span::styled(error, Style::default().fg(SOFT_WHITE)),
            ]))
            .wrap(Wrap { trim: true });

            frame.render_widget(error_widget, inner);
            return;
        }

        // Show selected item details
        if let Some(session) = state.selected() {
            Self::render_session_info(frame, inner, session);
        } else if let Some(worktree) = state.selected_worktree() {
            Self::render_worktree_info(frame, inner, worktree);
        } else {
            let empty = Paragraph::new(Span::styled(
                "Select an item to view details",
                Style::default().fg(MUTED_GRAY),
            ));
            frame.render_widget(empty, inner);
        }
    }

    /// Detail-pane row for a durable session label, or a blank line when the
    /// item has none, so an unlabeled item's layout does not shift.
    fn label_line(label: Option<&String>) -> Line<'static> {
        match label {
            Some(label) => Line::from(vec![
                Span::styled("Label:    ", Style::default().fg(MUTED_GRAY)),
                Span::styled(label.clone(), Style::default().fg(GOLD)),
            ]),
            None => Line::from(""),
        }
    }

    fn render_session_info(frame: &mut Frame, area: Rect, session: &OrphanedSession) {
        let lines = vec![
            Line::from(vec![
                Span::styled("Session:  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&session.session, Style::default().fg(SOFT_WHITE)),
            ]),
            // The label is what the operator named this run; the tmux name
            // above is machinery. A row that carries one in the list has to
            // carry it here too, or the two halves of the panel disagree
            // about which session is selected.
            Self::label_line(session.label.as_ref()),
            Line::from(""),
            Line::from(vec![
                Span::styled("Status:   ", Style::default().fg(MUTED_GRAY)),
                Span::styled(
                    if session.can_resume {
                        " Resumable"
                    } else {
                        " No transcript"
                    },
                    if session.can_resume {
                        Style::default().fg(SELECTION_GREEN)
                    } else {
                        Style::default().fg(WARNING_ORANGE)
                    },
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Created:  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&session.created, Style::default().fg(SOFT_WHITE)),
                if !session.time_ago.is_empty() {
                    Span::styled(
                        format!(" ({})", session.time_ago),
                        Style::default().fg(MUTED_GRAY),
                    )
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Directory:",
                Style::default().fg(MUTED_GRAY),
            )]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(&session.directory, Style::default().fg(CORNFLOWER_BLUE)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Branch:   ", Style::default().fg(MUTED_GRAY)),
                if let Some(ref branch) = session.worktree_branch {
                    Span::styled(format!(" {}", branch), Style::default().fg(SELECTION_GREEN))
                } else {
                    Span::styled(
                        " Unknown",
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    )
                },
            ]),
            Line::from(""),
            Line::from(vec![Span::styled("Task:", Style::default().fg(MUTED_GRAY))]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    session.task.chars().take(200).collect::<String>(),
                    Style::default().fg(SOFT_WHITE),
                ),
            ]),
        ];

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(paragraph, area);
    }

    fn render_worktree_info(frame: &mut Frame, area: Rect, worktree: &OrphanedWorktree) {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Name:     ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&worktree.name, Style::default().fg(SOFT_WHITE)),
            ]),
            Self::label_line(worktree.label.as_ref()),
            Line::from(""),
            Line::from(vec![
                Span::styled("Type:     ", Style::default().fg(MUTED_GRAY)),
                Span::styled(
                    format!(
                        "{} {}",
                        worktree.orphan_type.icon(),
                        worktree.orphan_type.label()
                    ),
                    Style::default().fg(WARNING_ORANGE),
                ),
            ]),
            Line::from(""),
            Line::from(vec![Span::styled("Path:", Style::default().fg(MUTED_GRAY))]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    worktree.path.to_string_lossy().to_string(),
                    Style::default().fg(CORNFLOWER_BLUE),
                ),
            ]),
            Line::from(""),
        ];

        // Branch
        lines.push(Line::from(vec![
            Span::styled("Branch:   ", Style::default().fg(MUTED_GRAY)),
            if let Some(ref branch) = worktree.branch {
                Span::styled(format!(" {}", branch), Style::default().fg(SELECTION_GREEN))
            } else {
                Span::styled(
                    " Unknown",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )
            },
        ]));
        lines.push(Line::from(""));

        // Source repo
        if let Some(ref repo) = worktree.source_repo {
            lines.push(Line::from(vec![
                Span::styled("Repo:     ", Style::default().fg(MUTED_GRAY)),
                Span::styled(repo, Style::default().fg(SOFT_WHITE)),
            ]));
            lines.push(Line::from(""));
        }

        // Last commit
        if let Some(ref commit) = worktree.last_commit {
            lines.push(Line::from(vec![Span::styled(
                "Commit:   ",
                Style::default().fg(MUTED_GRAY),
            )]));
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    commit.chars().take(60).collect::<String>(),
                    Style::default().fg(SOFT_WHITE),
                ),
            ]));
            lines.push(Line::from(""));
        }

        // Session ID if available
        if let Some(ref id) = worktree.id {
            lines.push(Line::from(vec![
                Span::styled("ID:       ", Style::default().fg(MUTED_GRAY)),
                Span::styled(id, Style::default().fg(MUTED_GRAY)),
            ]));
            lines.push(Line::from(""));
        }

        // Time ago
        if !worktree.time_ago.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Modified: ", Style::default().fg(MUTED_GRAY)),
                Span::styled(&worktree.time_ago, Style::default().fg(SOFT_WHITE)),
            ]));
        }

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(paragraph, area);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OrphanType, OrphanedSession, OrphanedWorktree, RecoveryRow, RecoveryViewMode,
        SessionMetadata, SessionRecovery, SessionRecoveryState, SessionStore,
    };
    use crate::config::SessionLabelStore;
    use crate::models::SessionAgentType;
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn orphan_session(name: &str, label: Option<&str>) -> OrphanedSession {
        OrphanedSession {
            session: name.to_string(),
            task: "some task".to_string(),
            directory: "/tmp/ainb-recovery".to_string(),
            created: String::new(),
            status: "running".to_string(),
            transcript_path: None,
            worktree_branch: None,
            can_resume: false,
            time_ago: String::new(),
            label: label.map(str::to_string),
        }
    }

    /// Render the panel and return the buffer as lines, so a row assertion can
    /// name the row rather than only ask whether text exists somewhere.
    fn render_to_lines(state: &SessionRecoveryState, w: u16, h: u16) -> Vec<String> {
        let mut ui = crate::app::ui_state::UiState::default();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal
            .draw(|frame| SessionRecovery::render(frame, frame.area(), state, &mut ui))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// A recoverable session row carries the same durable label the session
    /// list shows, in the same "<label> · <name>" shape, and the age column it
    /// had before labels existed still fits beside it.
    ///
    /// Sized like a real row on purpose: a 30-char tmux name, a 23-char label
    /// and a narrow 100-col terminal. A short name in a wide terminal cannot
    /// catch the label overflowing the pane.
    #[test]
    fn recovery_session_row_paints_its_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        let mut session = orphan_session(
            "ainb-save-prefix-abeb7e09-a02f",
            Some("RPC flake investigation"),
        );
        session.time_ago = "2h ago".to_string();
        state.orphaned_sessions = vec![session];

        let lines = render_to_lines(&mut state, 100, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("RPC flake in"))
            .unwrap_or_else(|| panic!("recovery row lost its label: {lines:#?}"));
        assert!(
            row.contains(" · ainb-save"),
            "label ate the whole name: {row}"
        );
        assert!(
            row.contains("(2h ago)"),
            "label pushed the age off the row: {row}"
        );
    }

    /// An unlabeled row must render exactly as it did before labels existed:
    /// bare name, no dangling separator.
    #[test]
    fn recovery_row_without_a_label_is_unchanged() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("tmux_plain", None)];

        let lines = render_to_lines(&mut state, 120, 20);
        // Anchor on the checkbox: the details pane on the right prints the bare
        // session name too, and matching that line instead means this guard
        // never looks at the list row it is guarding.
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("tmux_plain"))
            .expect("row rendered");
        assert!(!row.contains('·'), "unlabeled row grew a separator: {row}");
    }

    /// Worktree rows join their label through sessions.json, so they get the
    /// same prefix treatment as session rows, under the same width budget.
    /// Real worktree names run past 30 chars, which is exactly where an
    /// unbudgeted label prefix starts eating the age column.
    #[test]
    fn recovery_worktree_row_paints_its_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Worktrees;
        state.orphaned_worktrees = vec![OrphanedWorktree {
            name: "agents-in-a-box--save-prefix-abeb".to_string(),
            orphan_type: OrphanType::NoTmux,
            label: Some("prove-hangar-acp-leg-b-p3".to_string()),
            time_ago: "2h ago".to_string(),
            ..Default::default()
        }];

        let lines = render_to_lines(&mut state, 100, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("prove-hangar"))
            .unwrap_or_else(|| panic!("recovery worktree row lost its label: {lines:#?}"));
        assert!(
            row.contains(" · agents-in"),
            "label ate the whole name: {row}"
        );
        assert!(
            row.contains("(2h ago)"),
            "label pushed the age off the row: {row}"
        );
    }

    /// A wide terminal gives the list pane far more than the 25 columns the
    /// budget was once hard-coded to, and a labelled row is exactly the row
    /// that pays for the shortfall. At 200 cols both halves must survive whole.
    #[test]
    fn a_wide_terminal_stops_truncating_a_labelled_row() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Worktrees;
        state.orphaned_worktrees = vec![OrphanedWorktree {
            name: "agents-in-a-box--save-prefix-abeb".to_string(),
            orphan_type: OrphanType::NoTmux,
            label: Some("prove-hangar-acp-leg-b-p3".to_string()),
            time_ago: "2h ago".to_string(),
            ..Default::default()
        }];

        let lines = render_to_lines(&mut state, 200, 20);
        let row = lines
            .iter()
            .find(|line| line.contains("[ ] ") && line.contains("prove-hangar"))
            .unwrap_or_else(|| panic!("wide row lost its label: {lines:#?}"));
        assert!(
            row.contains("prove-hangar-acp-leg-b-p3 · agents-in-a-box--save-prefix-abeb"),
            "a 200-col pane still truncated a row that fits: {row}"
        );
        assert!(row.contains("(2h ago)"), "wide row lost its age: {row}");
    }

    /// The details pane is the other half of the recovery panel. A row that
    /// carries a label in the list has to carry it here too, or the operator
    /// reads a bare tmux name and cannot tell which run is selected.
    #[test]
    fn the_details_pane_names_the_selected_row_by_its_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Worktrees;
        state.orphaned_worktrees = vec![OrphanedWorktree {
            name: "agents-in-a-box--save-prefix-abeb".to_string(),
            orphan_type: OrphanType::NoTmux,
            label: Some("prove-hangar-acp-leg-b-p3".to_string()),
            time_ago: "2h ago".to_string(),
            ..Default::default()
        }];
        state.selected_index = 0;

        let lines = render_to_lines(&mut state, 200, 20);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Label:") && line.contains("prove-hangar-acp-leg-b-p3")),
            "details pane shows no label for the selected row: {lines:#?}"
        );
    }

    /// The footer is the only place that tells an operator `/` filters. It has
    /// more hints than fit, so the one the bug report asked for has to survive
    /// a narrow terminal.
    #[test]
    fn the_filter_hint_survives_a_narrow_terminal() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];

        let lines = render_to_lines(&mut state, 100, 20);
        assert!(
            lines.iter().any(|line| line.contains("/ filter")),
            "the filter hint is off-screen at 100 cols: {lines:#?}"
        );
    }

    /// The production join for a worktree label: path -> sessions.json entry ->
    /// tmux name -> label store. Every other label test hands the row a `label`
    /// it built itself, which proves the renderer and nothing about the join.
    #[test]
    fn a_worktree_joins_its_label_through_sessions_json() {
        let worktree_path = PathBuf::from("/tmp/ainb-recovery/by-name/save-prefix");
        let mut store = SessionStore::default();
        store.upsert(SessionMetadata {
            session_id: Uuid::nil(),
            tmux_session_name: "ainb-save-prefix".to_string(),
            worktree_path: worktree_path.clone(),
            workspace_name: "save-prefix".to_string(),
            created_at: chrono::Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        });
        let mut labels = SessionLabelStore::default();
        labels.set(
            "ainb-save-prefix".to_string(),
            Some("RPC flake".to_string()),
        );

        let mut rows = vec![
            OrphanedWorktree {
                path: worktree_path,
                ..orphan_worktree("save-prefix", None)
            },
            // No sessions.json entry: nothing bridges this row to a label.
            orphan_worktree("stranger", None),
        ];
        SessionRecoveryState::enrich_worktrees(&mut rows, &store, &labels);

        assert_eq!(rows[0].label.as_deref(), Some("RPC flake"));
        assert_eq!(
            rows[1].label, None,
            "a row with no metadata invented a label"
        );
    }

    fn orphan_worktree(name: &str, branch: Option<&str>) -> OrphanedWorktree {
        OrphanedWorktree {
            name: name.to_string(),
            branch: branch.map(str::to_string),
            orphan_type: OrphanType::NoTmux,
            ..Default::default()
        }
    }

    /// Three sessions, a query that only the third can match. The list must
    /// show that row and nothing else.
    #[test]
    fn filter_narrows_the_visible_rows() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
            orphan_session("gamma_three", None),
        ];
        for c in "gamma".chars() {
            state.search_push(c);
        }

        let lines = render_to_lines(&mut state, 120, 20);
        let joined = lines.join("\n");
        assert!(
            joined.contains("gamma_three"),
            "filtered row missing: {lines:#?}"
        );
        assert!(
            !joined.contains("alpha_one"),
            "filtered-out row still painted: {lines:#?}"
        );
        assert!(
            !joined.contains("beta_two"),
            "filtered-out row still painted: {lines:#?}"
        );
    }

    /// THE trap: under a filter, view row 0 is NOT raw row 0. Resume and
    /// archive both read `selected()`, so this is what stops the panel acting
    /// on a session the operator never looked at.
    #[test]
    fn filtered_selection_resolves_to_the_underlying_row() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
            orphan_session("gamma_three", None),
        ];
        for c in "gamma".chars() {
            state.search_push(c);
        }

        assert_eq!(state.selected_index, 0, "filter must re-anchor the cursor");
        assert_eq!(
            state.selected().map(|s| s.session.as_str()),
            Some("gamma_three"),
            "view row 0 resolved to the wrong session"
        );
        // Multi-select marks are view positions too, so they map the same way.
        assert_eq!(state.row_at(0), Some(RecoveryRow::Session(2)));
        // And the highlighted row in the painted buffer is that same row.
        let lines = render_to_lines(&mut state, 120, 20);
        let cursor_row = lines.iter().find(|l| l.contains('▶')).expect("a row is selected");
        assert!(
            cursor_row.contains("gamma_three"),
            "cursor is on the wrong row: {cursor_row}"
        );
    }

    /// A label from bug 1 is a filter field: the operator types the prefix
    /// they see, not the tmux name they do not.
    #[test]
    fn filter_matches_the_durable_label() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("tmux_aaa", Some("RPC flake")),
            orphan_session("tmux_bbb", None),
        ];
        for c in "rpc".chars() {
            state.search_push(c);
        }

        assert_eq!(state.current_view_count(), 1);
        assert_eq!(
            state.selected().map(|s| s.session.as_str()),
            Some("tmux_aaa")
        );
    }

    /// All view with every session filtered out. The old index math computed
    /// `0 + 0 - 1` here; this must resolve to the first worktree instead of
    /// panicking or silently selecting nothing.
    #[test]
    fn all_view_survives_filtering_every_session_away() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::All;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];
        state.orphaned_worktrees = vec![
            orphan_worktree("wt-keep", Some("feat/keep")),
            orphan_worktree("wt-drop", None),
        ];
        for c in "keep".chars() {
            state.search_push(c);
        }

        assert_eq!(
            state.current_view_count(),
            1,
            "only the matching worktree is visible"
        );
        assert!(state.is_worktree_selected());
        assert_eq!(state.worktree_index(), Some(0));
        assert_eq!(
            state.selected_worktree().map(|w| w.name.as_str()),
            Some("wt-keep")
        );
        let lines = render_to_lines(&mut state, 120, 20);
        assert!(lines.join("\n").contains("wt-keep"));
    }

    /// A filtered All view still maps worktree rows past the separator back to
    /// the right worktree, not the one at the same raw offset.
    #[test]
    fn all_view_maps_worktree_rows_past_the_separator() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::All;
        state.orphaned_sessions = vec![
            orphan_session("drop_me", None),
            orphan_session("keep_session", None),
        ];
        state.orphaned_worktrees = vec![
            orphan_worktree("drop-wt", None),
            orphan_worktree("keep-wt", None),
        ];
        for c in "keep".chars() {
            state.search_push(c);
        }

        // View is [keep_session][separator][keep-wt].
        assert_eq!(state.row_at(0), Some(RecoveryRow::Session(1)));
        assert_eq!(state.row_at(1), None, "the separator is not an item");
        assert_eq!(state.row_at(2), Some(RecoveryRow::Worktree(1)));
    }

    /// A query change re-points every view position, so stale marks would
    /// delete the wrong worktree. They get dropped instead.
    #[test]
    fn a_query_change_drops_multi_select_marks() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
        ];
        state.toggle_select();
        assert!(state.has_multi_selection());

        state.search_push('b');
        assert!(
            !state.has_multi_selection(),
            "marks survived a query change"
        );
    }

    /// Esc drops the filter and puts every row back, byte for byte.
    #[test]
    fn escape_restores_the_unfiltered_list() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![
            orphan_session("alpha_one", None),
            orphan_session("beta_two", None),
            orphan_session("gamma_three", None),
        ];
        let baseline = render_to_lines(&mut state, 120, 20);

        state.search_active = true;
        for c in "gamma".chars() {
            state.search_push(c);
        }
        // Not assert_ne! against the baseline: the search bar row alone makes
        // the buffers differ, so that check passes even with the filter
        // disconnected. Name the rows that must and must not survive.
        let filtered = render_to_lines(&mut state, 120, 20).join("\n");
        assert!(
            filtered.contains("gamma_three"),
            "filter hid the match: {filtered}"
        );
        assert!(
            !filtered.contains("alpha_one"),
            "filter kept a non-match: {filtered}"
        );

        state.search_cancel();
        assert!(!state.search_active);
        assert_eq!(
            render_to_lines(&mut state, 120, 20),
            baseline,
            "an empty query must render exactly the unfiltered panel"
        );
    }

    /// A filter that matches nothing says so. A blank panel reads as a crash.
    #[test]
    fn no_matches_renders_a_legible_empty_state() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];
        for c in "zzzz".chars() {
            state.search_push(c);
        }

        let lines = render_to_lines(&mut state, 120, 20);
        let joined = lines.join("\n");
        assert!(
            joined.contains("No matches for"),
            "no empty-state line: {lines:#?}"
        );
        assert!(
            joined.contains("zzzz"),
            "empty state does not echo the query: {lines:#?}"
        );
    }

    /// The query is visible while it is being typed, or the operator cannot
    /// tell a filtered list from an empty one.
    #[test]
    fn the_search_bar_paints_the_query() {
        let mut state = SessionRecoveryState::default();
        state.view_mode = RecoveryViewMode::Sessions;
        state.orphaned_sessions = vec![orphan_session("alpha_one", None)];
        state.search_active = true;
        for c in "alp".chars() {
            state.search_push(c);
        }

        let lines = render_to_lines(&mut state, 120, 20);
        assert!(
            lines.iter().any(|l| l.contains("/alp")),
            "search bar missing: {lines:#?}"
        );
    }

    #[test]
    fn default_defers_recovery_scan_until_the_screen_is_opened() {
        let state = SessionRecoveryState::default();

        assert!(!state.loading);
        assert!(state.orphaned_sessions.is_empty());
        assert!(state.orphaned_worktrees.is_empty());
    }
}
