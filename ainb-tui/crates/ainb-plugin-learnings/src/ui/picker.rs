//! Learnings picker popup: when an entity in the radial map is cited by more
//! than one learning, `o` opens this small list so the user chooses which one
//! to open in the Detail pane. A single-citation entity skips the picker and
//! opens Detail directly (the shell decides; this only handles the >1 case).

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};

use ainb_plugin_sdk::KeyCode;

use super::{
    CORNFLOWER_BLUE, DARK_BG, GOLD, LIST_HIGHLIGHT_BG, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE,
};

/// One pickable learning: its index into the shell's `records` + display title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    /// Index into `LearningsUi::records`.
    pub record_idx: usize,
    /// Learning title (the row label).
    pub title: String,
}

/// Picker popup state. Closed by default.
#[derive(Debug, Default)]
pub struct PickerState {
    open: bool,
    /// The entity whose citing-learnings are listed (popup title).
    entity: String,
    items: Vec<PickerItem>,
    selected: usize,
}

impl PickerState {
    /// `true` while the picker is showing (so the shell routes keys here).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the picker for `entity` with the given citing-learning `items`.
    pub fn open(&mut self, entity: impl Into<String>, items: Vec<PickerItem>) {
        self.entity = entity.into();
        self.items = items;
        self.selected = 0;
        self.open = true;
    }

    /// Close the picker.
    pub fn close(&mut self) {
        self.open = false;
        self.items.clear();
        self.selected = 0;
    }

    /// Route a key while open. Returns `Some(record_idx)` when the user picks a
    /// row with `⏎` (the shell opens Detail for it and closes the picker);
    /// `None` otherwise. `Backspace` closes the picker.
    pub fn handle_key(&mut self, code: &KeyCode) -> Option<usize> {
        match code {
            KeyCode::Down | KeyCode::Char { ch: 'j' } => {
                self.move_by(1);
                None
            }
            KeyCode::Up | KeyCode::Char { ch: 'k' } => {
                self.move_by(-1);
                None
            }
            KeyCode::Enter => self.items.get(self.selected).map(|it| it.record_idx),
            KeyCode::Backspace => {
                self.close();
                None
            }
            _ => None,
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last as isize) as usize;
    }

    /// Render the picker as a centred popup over `area`.
    pub fn render(&self, buf: &mut RBuffer, area: RRect) {
        if !self.open {
            return;
        }
        let popup = centered(60, 50, area);
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(GOLD))
            .style(Style::default().bg(DARK_BG))
            .title(Span::styled(
                format!(" learnings citing {} ", self.entity),
                Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
            ));
        let inner = block.inner(popup);
        block.render(popup, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let lines: Vec<Line> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let is_sel = i == self.selected;
                let marker = if is_sel { "▶ " } else { "  " };
                let style = if is_sel {
                    Style::default()
                        .fg(SELECTION_GREEN)
                        .bg(LIST_HIGHLIGHT_BG)
                        .add_modifier(RModifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE)
                };
                Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(it.title.clone(), style),
                ])
            })
            .collect();
        Paragraph::new(lines).render(rows[0], buf);

        let help = Line::from(vec![
            Span::styled(
                "↑↓",
                Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
            ),
            Span::styled(" move  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("⏎", Style::default().fg(GOLD).add_modifier(RModifier::BOLD)),
            Span::styled(" open  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Bksp",
                Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
            ),
            Span::styled(" close", Style::default().fg(MUTED_GRAY)),
        ]);
        Paragraph::new(help)
            .style(Style::default().fg(CORNFLOWER_BLUE))
            .render(rows[1], buf);
    }
}

/// A centred sub-rect `percent_x`×`percent_y` of `area`.
fn centered(percent_x: u16, percent_y: u16, area: RRect) -> RRect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<PickerItem> {
        (0..n)
            .map(|i| PickerItem {
                record_idx: i * 10,
                title: format!("learning {i}"),
            })
            .collect()
    }

    #[test]
    fn enter_returns_selected_record_index() {
        let mut p = PickerState::default();
        p.open("stale plan execution", items(3));
        assert!(p.is_open());
        // Move to the 2nd row, then Enter → its record_idx (1*10).
        assert_eq!(p.handle_key(&KeyCode::Down), None);
        assert_eq!(p.handle_key(&KeyCode::Enter), Some(10));
    }

    #[test]
    fn backspace_closes_without_selecting() {
        let mut p = PickerState::default();
        p.open("e", items(2));
        assert_eq!(p.handle_key(&KeyCode::Backspace), None);
        assert!(!p.is_open());
    }

    #[test]
    fn navigation_clamps_to_bounds() {
        let mut p = PickerState::default();
        p.open("e", items(2));
        // Up at the top is a clamped no-op.
        p.handle_key(&KeyCode::Up);
        assert_eq!(p.handle_key(&KeyCode::Enter), Some(0));
        p.open("e", items(2));
        p.handle_key(&KeyCode::Down);
        p.handle_key(&KeyCode::Down); // clamped at last
        assert_eq!(p.handle_key(&KeyCode::Enter), Some(10));
    }
}
