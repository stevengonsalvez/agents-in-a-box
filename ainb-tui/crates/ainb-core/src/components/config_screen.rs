// ABOUTME: Configuration screen component with split-pane layout for AINB 2.0

//! Settings screen: a collapsible section tree on the left, the rows of the
//! selected section on the right.
//!
//! The row source is `CONFIG_REGISTRY`, so this pane now paints ~150 rows
//! instead of ~24. Two consequences shape the rendering: the right pane must
//! scroll (a category no longer fits on a screen), and a row is ONE line with
//! its help shown only under the selection, rather than the old four-line block
//! per row that fit six settings in a full-height pane.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph},
};

use crate::app::state::{AppState, ConfigPane, ConfigSetting, ConfigValue};
use crate::config::screen_model;

// Color palette from TUI style guide
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

/// Tree glyphs, matching the style guide's `▼`/`▶` pair. Two columns wide
/// including the trailing space, so a leaf node's blank gutter lines up with an
/// expandable sibling's arrow.
const EXPANDED: &str = "▾ ";
const COLLAPSED: &str = "▸ ";
const LEAF: &str = "  ";

/// Column the `label : value` separator lines up on, so a pane of rows reads as
/// a table rather than a ragged list.
const VALUE_COLUMN: usize = 26;

/// Wider column for `/` results, which print the full dotted key instead of the
/// label — `mcp_pool.daemon_idle_grace_secs` is 31 characters and truncating it
/// to a label's width would hide the very thing the filter is for.
const SEARCH_KEY_COLUMN: usize = 38;

/// Blank a pane's interior before painting it.
///
/// `Paragraph` and `List` set the STYLE of their whole area but only write
/// symbols for the cells they have content for. Every pane on this screen
/// changes length as you move — the title swaps between a hint and a live
/// filter, rows scroll, the tree expands — so without this the tail of a longer
/// previous frame stays on screen ("Usage" repainting as "Usags  s"). Cheap: a
/// few thousand cells once per frame.
fn blank(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
}

pub struct ConfigScreenComponent;

impl ConfigScreenComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        tracing::debug!("Rendering ConfigScreen view");

        // Main container with dark background
        let container = Block::default().style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        // The `/` filter lives in the title bar rather than in a box of its
        // own. A box would push the panes down three rows as it opens, and a
        // vertical shift leaves stale cells behind on every tree row whose
        // label follows a multi-cell emoji — "Usage" repainted as "Usags  s".
        // A layout that never moves cannot desync.
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title bar (doubles as the filter box)
                Constraint::Min(0),    // Content area
                Constraint::Length(2), // Help bar
            ])
            .split(area);

        self.render_title(frame, main_layout[0], state);
        self.render_content(frame, main_layout[1], state);
        self.render_help_bar(frame, main_layout[2], state);
    }

    /// The title bar: the screen's name and row count normally, the live `/`
    /// filter and its match count while one is open.
    fn render_title(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;
        let searching = config_state.is_searching();

        let title_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if searching {
                SELECTION_GREEN
            } else {
                CORNFLOWER_BLUE
            }))
            .style(Style::default().bg(PANEL_BG));

        let inner = title_block.inner(area);
        frame.render_widget(title_block, area);
        blank(frame, inner);

        let spans = if searching {
            let query = config_state.search.clone().unwrap_or_default();
            let matches = config_state.visible_rows.len();
            vec![
                Span::styled(" 🔍 ", Style::default().fg(GOLD)),
                Span::styled(
                    "Search ",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(query, Style::default().fg(SOFT_WHITE)),
                Span::styled("█", Style::default().fg(SELECTION_GREEN)),
                Span::styled(
                    format!(
                        "  ({matches} match{})",
                        if matches == 1 { "" } else { "es" }
                    ),
                    Style::default().fg(CORNFLOWER_BLUE),
                ),
            ]
        } else {
            let row_count: usize = config_state.settings.values().map(Vec::len).sum();
            vec![
                // Deliberately no emoji: `⚙️` carries a variation selector, and
                // ratatui measures that as one column while the terminal draws
                // two. Every later cell on the line is then off by one, so the
                // tail of a longer previous title survives a repaint by a
                // character. The panel titles below use plain (width-2) emoji
                // and are fine.
                Span::styled("  ", Style::default()),
                Span::styled(
                    "Configuration",
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ({row_count} settings)"),
                    Style::default().fg(CORNFLOWER_BLUE),
                ),
                Span::styled("   press ", Style::default().fg(MUTED_GRAY)),
                Span::styled("/", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" to search every setting", Style::default().fg(MUTED_GRAY)),
            ]
        };

        let title_text = Paragraph::new(Line::from(spans))
            .alignment(Alignment::Left)
            .style(Style::default().bg(PANEL_BG));

        frame.render_widget(title_text, inner);
    }

    fn render_content(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Split into the section tree (left) and its rows (right)
        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30), // Tree panel
                Constraint::Percentage(70), // Settings panel
            ])
            .split(area);

        self.render_categories(frame, content_layout[0], state);
        self.render_settings(frame, content_layout[1], state);
    }

    /// The section tree. Depth 0 nodes are categories (icon + label); deeper
    /// nodes are the TOML sub-tables under them, indented two columns per level.
    fn render_categories(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;
        let is_focused =
            config_state.focused_pane == ConfigPane::Categories && !config_state.is_searching();

        let border_color = if is_focused { GOLD } else { CORNFLOWER_BLUE };
        let title_style = if is_focused {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };

        let block = Block::default()
            .title(Span::styled(" Categories ", title_style))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        let visible_height = inner.height as usize;
        let offset = scroll_offset(
            config_state.selected_node,
            config_state.visible_nodes.len(),
            visible_height,
        );

        let items: Vec<ListItem> = config_state
            .visible_nodes
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible_height)
            .filter_map(|(position, node_index)| {
                let node = config_state.tree.get(*node_index)?;
                let is_selected = position == config_state.selected_node;

                let style = if is_selected {
                    Style::default()
                        .fg(SELECTION_GREEN)
                        .bg(LIST_HIGHLIGHT_BG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE)
                };

                let mut spans = vec![Span::styled(
                    if is_selected { "▶" } else { " " },
                    Style::default().fg(SELECTION_GREEN),
                )];
                spans.push(Span::raw(" ".repeat(node.depth * 2)));
                spans.push(Span::styled(
                    if !node.has_children {
                        LEAF
                    } else if config_state.expanded.contains(&node.id()) {
                        EXPANDED
                    } else {
                        COLLAPSED
                    },
                    Style::default().fg(CORNFLOWER_BLUE),
                ));
                if node.depth == 0 {
                    spans.push(Span::styled(
                        format!("{} ", node.category.icon()),
                        Style::default().fg(GOLD),
                    ));
                }
                spans.push(Span::styled(node.label.as_str(), style));

                let base_style = if is_selected {
                    Style::default().bg(LIST_HIGHLIGHT_BG)
                } else {
                    Style::default()
                };
                Some(ListItem::new(Line::from(spans)).style(base_style))
            })
            .collect();

        frame.render_widget(block, area);
        blank(frame, inner);
        frame.render_widget(List::new(items).style(Style::default().bg(PANEL_BG)), inner);
    }

    /// The rows of the selected section (or the `/` filter's matches).
    ///
    /// One line per row so a 60-row section is navigable; the selected row gets
    /// a second line carrying its help text.
    fn render_settings(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;
        let is_focused =
            config_state.focused_pane == ConfigPane::Settings || config_state.is_searching();
        let rows = config_state.current_settings();

        let border_color = if is_focused { GOLD } else { CORNFLOWER_BLUE };
        let title_style = if is_focused {
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_GRAY)
        };
        let title = match (config_state.is_searching(), config_state.current_node()) {
            (true, _) => " Matches ".to_string(),
            (false, Some(node)) => format!(" {} ", node.label),
            (false, None) => " Settings ".to_string(),
        };

        let block = Block::default()
            .title(Span::styled(title, title_style))
            .title_bottom(Line::from(vec![Span::styled(
                format!(
                    " {}/{} ",
                    (config_state.selected_setting + 1).min(rows.len()),
                    rows.len()
                ),
                Style::default().fg(MUTED_GRAY),
            )]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);
        blank(frame, inner);

        if rows.is_empty() {
            let empty = Paragraph::new(vec![
                Line::from(Span::styled(
                    "  ✨ No settings match",
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Esc clears the filter",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                )),
            ])
            .style(Style::default().bg(PANEL_BG));
            frame.render_widget(empty, inner);
            return;
        }

        // Build the line list, then window it. Only the selected row expands, so
        // the list is `rows.len() + 1` lines at most and this stays cheap even
        // at the full ~150-row search result.
        let width = inner.width as usize;
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 1);
        let mut selected_line = 0usize;
        for (index, setting) in rows.iter().enumerate() {
            let is_selected = index == config_state.selected_setting;
            if is_selected {
                selected_line = lines.len();
            }
            lines.push(row_line(
                setting,
                is_selected,
                config_state.is_searching(),
                width,
            ));
            if is_selected {
                lines.push(help_line(setting, width));
            }
        }

        let height = inner.height as usize;
        // Keep both the selected row AND its help line on screen.
        let offset = scroll_offset(selected_line + 1, lines.len(), height);
        let windowed: Vec<Line> = lines.into_iter().skip(offset).take(height).collect();

        frame.render_widget(
            Paragraph::new(windowed).style(Style::default().bg(PANEL_BG)),
            inner,
        );
    }

    fn render_help_bar(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let config_state = &state.config_screen_state;

        let on_secret = matches!(
            config_state.current_setting().map(|row| &row.value),
            Some(ConfigValue::Secret(_))
        );

        let help_items: Vec<(&str, &str)> = if config_state.is_searching() {
            vec![
                ("type", "filter"),
                ("↑↓", "navigate"),
                ("Enter", "edit"),
                ("Esc", "clear filter"),
            ]
        } else if config_state.editing {
            vec![("Enter", "save"), ("Esc", "cancel")]
        } else {
            let mut items = vec![
                ("/", "search"),
                ("↑↓", "navigate"),
                ("←→/Tab", "switch pane"),
                ("Space", "expand"),
                ("Enter", "edit"),
            ];
            if on_secret {
                items.push(("^K", "to keychain"));
            }
            items.push(("S", "save all"));
            items.push(("Esc", "back"));
            items
        };

        let mut spans = vec![Span::styled("  ", Style::default())];
        for (i, (key, desc)) in help_items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)));
            }
            spans.push(Span::styled(
                *key,
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(*desc, Style::default().fg(MUTED_GRAY)));
        }

        let help_bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(DARK_BG));
        frame.render_widget(help_bar, area);
    }
}

/// `▶ Label ..... : value`, or the full dotted key while filtering.
///
/// Search results show the key rather than the label so a row found by typing
/// `double_click` is identifiable without knowing which section it came from —
/// which is the whole point of a flat filter over a tree.
fn row_line<'a>(
    setting: &'a ConfigSetting,
    is_selected: bool,
    searching: bool,
    width: usize,
) -> Line<'a> {
    let name = if searching {
        setting.key.as_str()
    } else {
        setting.label.as_str()
    };
    let read_only = screen_model::read_only_reason(&setting.key).is_some();

    let name_style = match (is_selected, read_only) {
        (true, _) => Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        (false, true) => Style::default().fg(MUTED_GRAY),
        (false, false) => Style::default().fg(SOFT_WHITE),
    };
    let value_style = match (read_only, &setting.value) {
        (true, _) => Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        (false, ConfigValue::Secret(secret)) if !secret.resolved => {
            Style::default().fg(WARNING_ORANGE)
        }
        (false, ConfigValue::Secret(_)) => Style::default().fg(SELECTION_GREEN),
        (false, _) => Style::default().fg(SOFT_WHITE),
    };

    let column = if searching {
        SEARCH_KEY_COLUMN
    } else {
        VALUE_COLUMN
    };
    let padded = pad_to(name, column.min(width.saturating_sub(12)));
    let mut spans = vec![
        Span::styled(
            if is_selected { "▶ " } else { "  " },
            Style::default().fg(SELECTION_GREEN),
        ),
        Span::styled(padded, name_style),
        Span::styled(" : ", Style::default().fg(MUTED_GRAY)),
        Span::styled(setting.value.display(), value_style),
    ];
    if read_only {
        spans.push(Span::styled(
            "  [read-only]",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ));
    }

    let line = Line::from(spans);
    if is_selected {
        line.style(Style::default().bg(LIST_HIGHLIGHT_BG))
    } else {
        line
    }
}

/// The selected row's help, plus its dotted key so the user can reach the same
/// setting from `ainb config set`.
fn help_line<'a>(setting: &'a ConfigSetting, width: usize) -> Line<'a> {
    let reason = screen_model::read_only_reason(&setting.key);
    let text = reason.map_or_else(|| setting.description.clone(), |why| why.to_string());
    Line::from(vec![
        Span::styled("    ", Style::default()),
        Span::styled("└─ ", Style::default().fg(SUBDUED_BORDER)),
        Span::styled(
            truncate(&text, width.saturating_sub(8)),
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ),
    ])
}

/// First line index to render so `selected` (1-based) sits inside a `height`
/// tall window. Returns 0 when everything fits.
fn scroll_offset(selected: usize, total: usize, height: usize) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    let selected = selected.min(total.saturating_sub(1));
    if selected < height {
        0
    } else {
        (selected + 1).saturating_sub(height).min(total - height)
    }
}

fn pad_to(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return truncate(text, width);
    }
    format!("{text}{}", " ".repeat(width - len))
}

/// Character-safe truncation: byte slicing a label with a `→` in it panics.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl Default for ConfigScreenComponent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_keeps_the_selection_in_view() {
        assert_eq!(scroll_offset(1, 5, 10), 0, "everything fits");
        assert_eq!(scroll_offset(3, 100, 10), 0, "near the top");
        assert_eq!(scroll_offset(50, 100, 10), 41);
        assert_eq!(scroll_offset(99, 100, 10), 90, "clamped at the end");
        assert_eq!(scroll_offset(5, 100, 0), 0, "zero-height pane");
    }

    #[test]
    fn truncation_is_character_safe() {
        assert_eq!(truncate("token → keychain", 8), "token →…");
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn padding_lines_values_up() {
        assert_eq!(pad_to("Theme", 8), "Theme   ");
        assert_eq!(pad_to("A very long label indeed", 8), "A very …");
    }
}
