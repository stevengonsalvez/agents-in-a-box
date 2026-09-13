// ABOUTME: Git view component for displaying git status, changed files, and diffs with commit/push functionality
// Supports hierarchical file tree view and markdown preview for .md files

#![allow(dead_code)]

pub use ainb_app::components::git_view::*;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

// Premium color palette (TUI Style Guide)
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237); // Primary accent, borders
const GOLD: Color = Color::Rgb(255, 215, 0); // Important CTAs, emphasis
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100); // Active selections
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0); // Warnings

// Background colors
const DARK_BG: Color = Color::Rgb(25, 25, 35); // Main UI background
const PANEL_BG: Color = Color::Rgb(30, 30, 40); // Panel backgrounds
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60); // Selection background

// Text colors
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230); // Primary text
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140); // Secondary text
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80); // Secondary borders

// Status colors
const PROGRESS_CYAN: Color = Color::Rgb(100, 200, 230); // Loading/processing

use super::code_review;

pub struct GitViewComponent;

impl GitViewComponent {
    pub fn render(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        // Create main layout - adjust constraints based on commit mode
        let constraints = if git_state.is_in_commit_mode() {
            vec![
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
                Constraint::Length(5), // Commit message input
                Constraint::Length(3), // Status/Actions
            ]
        } else {
            vec![
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Content
                Constraint::Length(3), // Status/Actions
            ]
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Render raised tab style - dynamically include Markdown tab if applicable
        let tab_titles: Vec<&str> =
            if git_state.is_selected_markdown() && !git_state.markdown_content.is_empty() {
                vec!["Review", "Commits", "Markdown"]
            } else {
                vec!["Review", "Commits"]
            };

        let selected_tab = match git_state.active_tab {
            GitTab::Review | GitTab::Files | GitTab::Diff => 0,
            GitTab::Commits => 1,
            GitTab::Markdown => {
                if tab_titles.len() > 2 {
                    2
                } else {
                    0
                }
            }
        };

        Self::render_raised_tabs(frame, chunks[0], &tab_titles, selected_tab);

        // Render content based on active tab
        match git_state.active_tab {
            GitTab::Review => {
                code_review::render::render(
                    frame,
                    chunks[1],
                    &git_state.review,
                    &git_state.review_ui,
                );
            }
            GitTab::Files => Self::render_files_tab(frame, chunks[1], git_state),
            GitTab::Diff => Self::render_diff_tab(frame, chunks[1], git_state),
            GitTab::Commits => Self::render_commits_tab(frame, chunks[1], git_state),
            GitTab::Markdown => Self::render_markdown_tab(frame, chunks[1], git_state),
        }

        // Render commit message input if in commit mode
        if git_state.is_in_commit_mode() {
            Self::render_commit_input(frame, chunks[2], git_state);
            // Status bar is at index 3 when commit input is shown
            Self::render_status_bar(frame, chunks[3], git_state);
        } else {
            // Status bar is at index 2 when no commit input
            Self::render_status_bar(frame, chunks[2], git_state);
        }
    }

    /// Render tabs in classic raised tab style
    /// Active tab has a raised box that connects to content below
    fn render_raised_tabs(frame: &mut Frame, area: Rect, tabs: &[&str], selected: usize) {
        // We need exactly 3 lines for the raised tab effect
        if area.height < 3 {
            return;
        }

        let area_width = area.width as usize;

        // Calculate consistent tab cell widths (each tab occupies same structure across all lines)
        // Structure: " · TabName " where separator is 3 chars, tab name varies, trailing space 1
        // For active: "│ TabName │" where bars are 1 char each, padding 1 each side

        // Calculate the display width each tab cell needs (must be consistent across all lines)
        // Each cell = separator(3) + name + padding = OR = bar(1) + padding(1) + name + padding(1) + bar(1)
        // We use: 3 chars before name + name + 1 char after for inactive
        // We use: 1 bar + 1 space + name + 1 space + 1 bar for active (= 4 + name)
        // Make them equal by using max
        let tab_cell_widths: Vec<usize> = tabs
            .iter()
            .map(|t| {
                let name_len = t.chars().count();
                // Cell width = separator space (3) + name + trailing space (1) = name + 4
                // This matches active: │(1) + space(1) + name + space(1) + │(1) = name + 4
                name_len + 4
            })
            .collect();

        let icon_width = 4; // "[G] "

        // Line 1: Top border - spaces for inactive, ╭───╮ for active
        let mut top_spans: Vec<Span> = vec![];
        top_spans.push(Span::raw(" ".repeat(icon_width))); // space above icon

        for (i, &cell_width) in tab_cell_widths.iter().enumerate() {
            if i == selected {
                // Active tab top: ╭───────╮
                top_spans.push(Span::styled("╭", Style::default().fg(GOLD)));
                top_spans.push(Span::styled(
                    "─".repeat(cell_width - 2),
                    Style::default().fg(GOLD),
                ));
                top_spans.push(Span::styled("╮", Style::default().fg(GOLD)));
            } else {
                // Inactive: just spaces
                top_spans.push(Span::raw(" ".repeat(cell_width)));
            }
        }

        // Fill remaining with spaces
        let top_used: usize = icon_width + tab_cell_widths.iter().sum::<usize>();
        if top_used < area_width {
            top_spans.push(Span::raw(" ".repeat(area_width - top_used)));
        }

        // Line 2: Tab names - │ Name │ for active, " · Name" for inactive
        let mut mid_spans: Vec<Span> = vec![];
        mid_spans.push(Span::styled(
            "[G] ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));

        for (i, &tab_name) in tabs.iter().enumerate() {
            let cell_width = tab_cell_widths[i];
            let name_len = tab_name.chars().count();

            if i == selected {
                // Active: │ Name │
                let inner_width = cell_width - 2; // minus the two │ bars
                let left_pad = (inner_width - name_len) / 2;
                let right_pad = inner_width - name_len - left_pad;

                mid_spans.push(Span::styled("│", Style::default().fg(GOLD)));
                mid_spans.push(Span::raw(" ".repeat(left_pad)));
                mid_spans.push(Span::styled(
                    tab_name.to_string(),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ));
                mid_spans.push(Span::raw(" ".repeat(right_pad)));
                mid_spans.push(Span::styled("│", Style::default().fg(GOLD)));
            } else {
                // Inactive: " · Name " (total = cell_width)
                let remaining = cell_width - 3 - name_len; // 3 for " · "
                mid_spans.push(Span::styled(" · ", Style::default().fg(SUBDUED_BORDER)));
                mid_spans.push(Span::styled(
                    tab_name.to_string(),
                    Style::default().fg(MUTED_GRAY),
                ));
                if remaining > 0 {
                    mid_spans.push(Span::raw(" ".repeat(remaining)));
                }
            }
        }

        // Add "Tab switch" hint
        let mid_used: usize = icon_width + tab_cell_widths.iter().sum::<usize>();
        let hint_text = "Tab switch";
        let hint_len = hint_text.len() + 1;
        if mid_used + hint_len < area_width {
            let padding = area_width - mid_used - hint_len;
            mid_spans.push(Span::raw(" ".repeat(padding)));
            mid_spans.push(Span::styled(
                "Tab",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            mid_spans.push(Span::styled(" switch", Style::default().fg(MUTED_GRAY)));
        }

        // Line 3: Bottom border - ─── for inactive, ┘   └ for active
        let mut bot_spans: Vec<Span> = vec![];
        bot_spans.push(Span::styled(
            "─".repeat(icon_width),
            Style::default().fg(CORNFLOWER_BLUE),
        ));

        for (i, &cell_width) in tab_cell_widths.iter().enumerate() {
            if i == selected {
                // Active: ┘ spaces └
                let inner_width = cell_width - 2;
                bot_spans.push(Span::styled("┘", Style::default().fg(GOLD)));
                bot_spans.push(Span::raw(" ".repeat(inner_width)));
                bot_spans.push(Span::styled("└", Style::default().fg(GOLD)));
            } else {
                // Inactive: continuous line
                bot_spans.push(Span::styled(
                    "─".repeat(cell_width),
                    Style::default().fg(CORNFLOWER_BLUE),
                ));
            }
        }

        // Fill remaining with line
        let bot_used: usize = icon_width + tab_cell_widths.iter().sum::<usize>();
        if bot_used < area_width {
            bot_spans.push(Span::styled(
                "─".repeat(area_width - bot_used),
                Style::default().fg(CORNFLOWER_BLUE),
            ));
        }

        // Render
        let tab_lines = vec![
            Line::from(top_spans),
            Line::from(mid_spans),
            Line::from(bot_spans),
        ];

        let tab_paragraph = Paragraph::new(tab_lines).style(Style::default().bg(DARK_BG));

        frame.render_widget(tab_paragraph, area);
    }

    fn render_files_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        if git_state.file_tree_items.is_empty() {
            let no_changes = Paragraph::new(vec![
                Line::from(Span::styled(
                    "✨ No changes detected",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Working directory is clean",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📁 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Changed Files",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(no_changes, area);
            return;
        }

        let items: Vec<ListItem> = git_state
            .file_tree_items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = i == git_state.selected_tree_index;

                // Build indentation with tree lines (styled)
                let indent = Self::build_tree_indent(item.depth, item.is_last_in_group);
                let indent_style = Style::default().fg(SUBDUED_BORDER);

                if item.is_folder {
                    // Folder rendering with premium styling
                    let expand_symbol = if item.is_expanded { "▼" } else { "▶" };

                    let (folder_color, expand_color) = if is_selected {
                        (SELECTION_GREEN, SELECTION_GREEN)
                    } else {
                        (CORNFLOWER_BLUE, MUTED_GRAY)
                    };

                    let folder_style = Style::default().fg(folder_color);
                    let folder_name_style = if is_selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    };

                    let count_text = if item.file_count > 0 {
                        format!(" ({})", item.file_count)
                    } else {
                        String::new()
                    };

                    // Show status badge for untracked directories
                    let status_prefix = if let Some(ref status) = item.status {
                        let status_style =
                            Style::default().fg(status_color(status)).add_modifier(Modifier::BOLD);
                        vec![
                            Span::styled(format!("[{}]", status.symbol()), status_style),
                            Span::raw(" "),
                        ]
                    } else {
                        vec![]
                    };

                    let folder_icon = if item.is_expanded { "📂" } else { "📁" };

                    let mut spans = vec![Span::styled(indent, indent_style)];
                    if is_selected {
                        spans.insert(0, Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
                    } else {
                        spans.insert(0, Span::raw("  "));
                    }
                    spans.extend(status_prefix);
                    spans.extend(vec![
                        Span::styled(expand_symbol, Style::default().fg(expand_color)),
                        Span::raw(" "),
                        Span::styled(folder_icon, folder_style),
                        Span::raw(" "),
                        Span::styled(&item.display_name, folder_name_style),
                        Span::styled(count_text, Style::default().fg(MUTED_GRAY)),
                    ]);

                    let base_style = if is_selected {
                        Style::default().bg(LIST_HIGHLIGHT_BG)
                    } else {
                        Style::default()
                    };

                    ListItem::new(Line::from(spans)).style(base_style)
                } else {
                    // File rendering with premium styling
                    let status = item.status.as_ref().unwrap_or(&GitFileStatus::Modified);
                    let status_style =
                        Style::default().fg(status_color(status)).add_modifier(Modifier::BOLD);

                    // Use display_name, fallback to full_path if empty
                    let filename = if item.display_name.is_empty() {
                        &item.full_path
                    } else {
                        &item.display_name
                    };

                    let file_icon = Self::get_file_icon(filename);
                    let file_style = if is_selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    };

                    let mut spans = vec![];
                    if is_selected {
                        spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    spans.extend(vec![
                        Span::styled(indent.clone(), indent_style),
                        Span::styled(format!("[{}]", status.symbol()), status_style),
                        Span::raw(" "),
                        Span::raw(file_icon),
                        Span::raw(" "),
                        Span::styled(filename, file_style),
                    ]);

                    let base_style = if is_selected {
                        Style::default().bg(LIST_HIGHLIGHT_BG)
                    } else {
                        Style::default()
                    };

                    ListItem::new(Line::from(spans)).style(base_style)
                }
            })
            .collect();

        let files_list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📁 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Changed Files ",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("({})", git_state.changed_files.len()),
                            Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
                        ),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " Enter",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" toggle ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(" e", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                        Span::styled(" expand ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(" E", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                        Span::styled(" collapse ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Tab",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" switch tab ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG));

        let mut list_state = ListState::default();
        list_state.select(Some(git_state.selected_tree_index));

        frame.render_stateful_widget(files_list, area, &mut list_state);
    }

    /// Build tree indentation string with proper line characters
    fn build_tree_indent(depth: usize, is_last: bool) -> String {
        if depth == 0 {
            return String::new();
        }

        let mut indent = String::new();
        // Add vertical lines for all but the last level
        for _ in 0..(depth - 1) {
            indent.push_str("│  ");
        }
        // Add the final branch character
        if is_last {
            indent.push_str("└─ ");
        } else {
            indent.push_str("├─ ");
        }
        indent
    }

    /// Get file icon based on extension
    fn get_file_icon(filename: &str) -> &'static str {
        if filename.ends_with(".rs") {
            "🦀"
        } else if filename.ends_with(".py") {
            "🐍"
        } else if filename.ends_with(".js") || filename.ends_with(".jsx") {
            "📜"
        } else if filename.ends_with(".ts") || filename.ends_with(".tsx") {
            "📘"
        } else if filename.ends_with(".md") || filename.ends_with(".markdown") {
            "📝"
        } else if filename.ends_with(".json") {
            "📋"
        } else if filename.ends_with(".toml")
            || filename.ends_with(".yaml")
            || filename.ends_with(".yml")
        {
            "⚙️"
        } else if filename.ends_with(".sh") || filename.ends_with(".bash") {
            "🖥️"
        } else if filename.ends_with(".html") {
            "🌐"
        } else if filename.ends_with(".css") || filename.ends_with(".scss") {
            "🎨"
        } else if filename.ends_with(".go") {
            "🐹"
        } else if filename.ends_with(".java") {
            "☕"
        } else {
            "📄"
        }
    }

    fn render_diff_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        if git_state.diff_content.is_empty() {
            let no_diff = Paragraph::new(vec![
                Line::from(Span::styled(
                    "📋 No diff available",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Select a file in the Files tab to view its diff",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📋 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Diff",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(no_diff, area);
            return;
        }

        // Calculate visible lines
        let content_height = area.height.saturating_sub(2) as usize; // Account for borders
        let start_line = git_state.diff_scroll_offset;
        let end_line = (start_line + content_height).min(git_state.diff_content.len());

        // Diff colors (enhanced for visibility)
        let addition_color = Color::Rgb(100, 200, 100); // Softer green
        let deletion_color = Color::Rgb(230, 100, 100); // Softer red
        let hunk_color = PROGRESS_CYAN;
        let file_header_color = WARNING_ORANGE;

        let visible_lines: Vec<Line> = git_state.diff_content[start_line..end_line]
            .iter()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(addition_color)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(deletion_color)
                } else if line.starts_with("@@") {
                    Style::default().fg(hunk_color).add_modifier(Modifier::BOLD)
                } else if line.starts_with("+++") || line.starts_with("---") {
                    Style::default().fg(file_header_color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE)
                };

                Line::from(Span::styled(line.clone(), style))
            })
            .collect();

        let selected_file_name = git_state
            .changed_files
            .get(git_state.selected_file_index)
            .map(|f| f.path.as_str())
            .unwrap_or("No file selected");

        let scroll_info = format!(
            " [{}/{}]",
            git_state.diff_scroll_offset + 1,
            git_state.diff_content.len().max(1)
        );

        let diff_paragraph = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📋 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Diff: ",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(selected_file_name, Style::default().fg(SOFT_WHITE)),
                        Span::styled(scroll_info, Style::default().fg(MUTED_GRAY)),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " j/k",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" scroll ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Tab",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" switch tab ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(diff_paragraph, area);
    }

    fn render_commits_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(DARK_BG))
            .title(Line::from(vec![
                Span::styled(" 📜 ", Style::default().fg(GOLD)),
                Span::styled(
                    "Branch Commits",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if git_state.commits.is_empty() {
            let msg = Paragraph::new(vec![
                Line::from(Span::styled(
                    "No commits on this branch yet",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Commits since diverging from main/master will appear here",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ]);
            frame.render_widget(msg, inner);
            return;
        }

        // Build list items
        let items: Vec<ListItem> = git_state
            .commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let is_selected = i == git_state.selected_commit_index;
                let prefix = if is_selected { "▶ " } else { "  " };

                let line = Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(&commit.hash_short, Style::default().fg(GOLD)),
                    Span::raw(" "),
                    Span::styled(
                        truncate_string(&commit.message, 50),
                        Style::default().fg(SOFT_WHITE),
                    ),
                    Span::raw(" - "),
                    Span::styled(&commit.author, Style::default().fg(MUTED_GRAY)),
                    Span::raw(" "),
                    Span::styled(
                        &commit.date,
                        Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                    ),
                ]);

                let style = if is_selected {
                    Style::default().bg(LIST_HIGHLIGHT_BG)
                } else {
                    Style::default()
                };

                ListItem::new(line).style(style)
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(git_state.selected_commit_index));

        let list = List::new(items).highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG));

        frame.render_stateful_widget(list, inner, &mut list_state);
    }

    fn render_markdown_tab(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        if git_state.markdown_content.is_empty() {
            let no_content = Paragraph::new(vec![
                Line::from(Span::styled(
                    "📝 No markdown content available",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Select a .md file in the Files tab",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📝 ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Markdown Preview",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(no_content, area);
            return;
        }

        // Calculate visible lines
        let content_height = area.height.saturating_sub(2) as usize; // Account for borders
        let start_line = git_state.markdown_scroll_offset;
        let end_line = (start_line + content_height).min(git_state.markdown_content.len());

        // Premium markdown colors
        let heading1_color = PROGRESS_CYAN;
        let heading2_color = CORNFLOWER_BLUE;
        let heading3_color = Color::Rgb(150, 150, 220);
        let code_bg = Color::Rgb(35, 35, 45);
        let code_fg = SELECTION_GREEN;
        let link_color = Color::Rgb(100, 180, 255);
        let quote_color = MUTED_GRAY;

        let visible_lines: Vec<Line> = git_state.markdown_content[start_line..end_line]
            .iter()
            .map(|md_line| {
                let style = match &md_line.style {
                    MarkdownStyle::Heading1 => Style::default()
                        .fg(heading1_color)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    MarkdownStyle::Heading2 => {
                        Style::default().fg(heading2_color).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Heading3 => {
                        Style::default().fg(heading3_color).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Paragraph => Style::default().fg(SOFT_WHITE),
                    MarkdownStyle::CodeBlock => Style::default().fg(code_fg).bg(code_bg),
                    MarkdownStyle::CodeBlockHeader(_) => {
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::ListItem => Style::default().fg(SOFT_WHITE),
                    MarkdownStyle::Bold => {
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                    }
                    MarkdownStyle::Italic => {
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::ITALIC)
                    }
                    MarkdownStyle::InlineCode => {
                        Style::default().fg(Color::Rgb(220, 150, 220)).bg(code_bg)
                    }
                    MarkdownStyle::Link => {
                        Style::default().fg(link_color).add_modifier(Modifier::UNDERLINED)
                    }
                    MarkdownStyle::BlockQuote => {
                        Style::default().fg(quote_color).add_modifier(Modifier::ITALIC)
                    }
                };

                Line::from(Span::styled(md_line.content.clone(), style))
            })
            .collect();

        // Get selected file name for title
        let file_name = git_state
            .file_tree_items
            .get(git_state.selected_tree_index)
            .map(|item| item.display_name.as_str())
            .unwrap_or("Markdown");

        let scroll_info = format!(
            " [{}/{}]",
            git_state.markdown_scroll_offset + 1,
            git_state.markdown_content.len().max(1)
        );

        let markdown_paragraph = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(CORNFLOWER_BLUE))
                    .style(Style::default().bg(DARK_BG))
                    .title(Line::from(vec![
                        Span::styled(" 📝 ", Style::default().fg(GOLD)),
                        Span::styled(
                            file_name,
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(scroll_info, Style::default().fg(MUTED_GRAY)),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " j/k",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" scroll ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Tab",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" switch tab ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(markdown_paragraph, area);
    }

    fn render_commit_input(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        let empty_string = String::new();
        let commit_message = git_state.commit_message_input.as_ref().unwrap_or(&empty_string);

        // Create spans with cursor visualization
        let (before_cursor, after_cursor) =
            commit_message.split_at(git_state.commit_message_cursor.min(commit_message.len()));

        let input_line = Line::from(vec![
            Span::styled(before_cursor, Style::default().fg(SOFT_WHITE)),
            Span::styled("█", Style::default().fg(SELECTION_GREEN)),
            Span::styled(after_cursor, Style::default().fg(SOFT_WHITE)),
        ]);

        let input_paragraph = Paragraph::new(input_line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(SELECTION_GREEN))
                    .style(Style::default().bg(Color::Rgb(35, 35, 45)))
                    .title(Line::from(vec![
                        Span::styled(" ✏️ ", Style::default().fg(GOLD)),
                        Span::styled(
                            "Commit Message",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                    ]))
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " Enter",
                            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" commit ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " Esc",
                            Style::default().fg(WARNING_ORANGE).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" cancel ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
                        Span::styled(
                            " ←/→",
                            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" cursor ", Style::default().fg(MUTED_GRAY)),
                    ])),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(input_paragraph, area);
    }

    fn render_status_bar(frame: &mut Frame, area: Rect, git_state: &GitViewState) {
        let (status_icon, status_text, status_color) = if git_state.is_dirty {
            (
                "🔄",
                format!("{} files changed", git_state.changed_files.len()),
                WARNING_ORANGE,
            )
        } else {
            ("✓", "Working directory clean".to_string(), SELECTION_GREEN)
        };

        let (push_icon, push_text, push_color) = if git_state.can_push {
            ("🚀", "Ready to push", SELECTION_GREEN)
        } else {
            ("✓", "Up to date", MUTED_GRAY)
        };

        // Build the status line with rich formatting
        let status_line = Line::from(vec![
            Span::styled(
                format!(" {} ", status_icon),
                Style::default().fg(status_color),
            ),
            Span::styled(&status_text, Style::default().fg(status_color)),
            Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled(format!("{} ", push_icon), Style::default().fg(push_color)),
            Span::styled(push_text, Style::default().fg(push_color)),
            Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("p", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" push ", Style::default().fg(MUTED_GRAY)),
            Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
            Span::styled(
                " Esc",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" back ", Style::default().fg(MUTED_GRAY)),
        ]);

        let status_paragraph = Paragraph::new(status_line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(SUBDUED_BORDER))
                    .style(Style::default().bg(PANEL_BG)),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(status_paragraph, area);
    }
}

/// Truncate a string to at most `max_len` displayed chars. Delegates to
/// the canonical `truncate_with_ellipsis` (uses `…`, single Unicode char).
/// Renamed-on-merge from a per-file copy that used `...` (3 ASCII chars).
fn truncate_string(s: &str, max_len: usize) -> String {
    crate::widgets::truncate_with_ellipsis(s, max_len).into_owned()
}

/// The colour a file's git status badge is drawn in.
#[must_use]
pub const fn status_color(status: &GitFileStatus) -> Color {
    match status {
        GitFileStatus::Added => Color::Green,
        GitFileStatus::Modified => Color::Yellow,
        GitFileStatus::Deleted => Color::Red,
        GitFileStatus::Renamed => Color::Blue,
        GitFileStatus::Untracked => Color::Magenta,
    }
}
