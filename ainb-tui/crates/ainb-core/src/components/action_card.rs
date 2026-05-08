// ABOUTME: Reusable action card widget for the AINB home screen grid

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);

/// Action card configuration
#[derive(Debug, Clone)]
pub struct ActionCard {
    /// Card identifier
    pub id: ActionCardId,
    /// Icon to display (emoji or unicode)
    pub icon: &'static str,
    /// Main title
    pub title: &'static str,
    /// Keyboard shortcut
    pub shortcut: &'static str,
    /// Description text
    pub description: &'static str,
    /// Optional badge count (e.g., active sessions)
    pub badge: Option<usize>,
    /// Whether the card is disabled
    pub disabled: bool,
}

/// Card identifiers for the home screen - matches HomeTile
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCardId {
    Agents,    // Agent selection
    Catalog,   // Browse catalog/marketplace
    Config,    // Settings & presets
    Sessions,  // Session manager
    Stats,     // Analytics & usage
    Help,      // Docs & guides
}

impl ActionCardId {
    /// Get all cards in display order (2 rows x 3 cols)
    pub fn all() -> &'static [ActionCardId] {
        &[
            // Row 1
            Self::Agents,
            Self::Catalog,
            Self::Config,
            // Row 2
            Self::Sessions,
            Self::Stats,
            Self::Help,
        ]
    }

    /// Get the card configuration for this ID
    pub fn to_card(&self) -> ActionCard {
        match self {
            Self::Agents => ActionCard {
                id: *self,
                icon: "🤖",
                title: "Agents",
                shortcut: "a",
                description: "Select & Configure",
                badge: None,
                disabled: false,
            },
            Self::Catalog => ActionCard {
                id: *self,
                icon: "📦",
                title: "Catalog",
                shortcut: "c",
                description: "Browse & Bootstrap",
                badge: None,
                disabled: false,
            },
            Self::Config => ActionCard {
                id: *self,
                icon: "⚙️",
                title: "Config",
                shortcut: "C",
                description: "Settings & Presets",
                badge: None,
                disabled: false,
            },
            Self::Sessions => ActionCard {
                id: *self,
                icon: "🚀",
                title: "Sessions",
                shortcut: "s",
                description: "Manage Active",
                badge: None,
                disabled: false,
            },
            Self::Stats => ActionCard {
                id: *self,
                icon: "📊",
                title: "Stats",
                shortcut: "i",
                description: "Usage & Analytics",
                badge: None,
                disabled: false,
            },
            Self::Help => ActionCard {
                id: *self,
                icon: "❓",
                title: "Help",
                shortcut: "?",
                description: "Docs & Guides",
                badge: None,
                disabled: false,
            },
        }
    }

    /// Convert grid position (row, col) to card ID
    pub fn from_position(row: usize, col: usize) -> Option<Self> {
        let index = row * 3 + col;
        Self::all().get(index).copied()
    }

    /// Get grid position for this card ID
    pub fn position(&self) -> (usize, usize) {
        let index = Self::all().iter().position(|&id| id == *self).unwrap_or(0);
        (index / 3, index % 3)
    }
}

/// Action card grid state
#[derive(Debug)]
pub struct ActionCardGridState {
    /// Currently selected position (row, col)
    pub selected_position: (usize, usize),
    /// Whether the grid is focused
    pub is_focused: bool,
    /// Card configurations (can be updated with badges, etc.)
    pub cards: Vec<ActionCard>,
}

impl ActionCardGridState {
    pub fn new() -> Self {
        let cards = ActionCardId::all()
            .iter()
            .map(|id| id.to_card())
            .collect();

        Self {
            selected_position: (0, 0),
            is_focused: false,
            cards,
        }
    }

    /// Get the currently selected card ID
    pub fn selected_card(&self) -> Option<ActionCardId> {
        ActionCardId::from_position(self.selected_position.0, self.selected_position.1)
    }

    /// Move selection in the grid
    pub fn move_selection(&mut self, direction: Direction) {
        let (row, col) = self.selected_position;
        match direction {
            Direction::Vertical => {
                // Down
                if row < 1 {
                    self.selected_position.0 += 1;
                }
            }
            Direction::Horizontal => {
                // Right
                if col < 2 {
                    self.selected_position.1 += 1;
                }
            }
        }
    }

    pub fn move_up(&mut self) {
        if self.selected_position.0 > 0 {
            self.selected_position.0 -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected_position.0 < 1 {
            self.selected_position.0 += 1;
        }
    }

    pub fn move_left(&mut self) {
        if self.selected_position.1 > 0 {
            self.selected_position.1 -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.selected_position.1 < 2 {
            self.selected_position.1 += 1;
        }
    }

    /// Update badge count for a specific card
    pub fn set_badge(&mut self, card_id: ActionCardId, count: Option<usize>) {
        if let Some(card) = self.cards.iter_mut().find(|c| c.id == card_id) {
            card.badge = count;
        }
    }
}

impl Default for ActionCardGridState {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a single action card
pub fn render_action_card(
    frame: &mut Frame,
    area: Rect,
    card: &ActionCard,
    is_selected: bool,
    is_focused: bool,
) {
    let (border_color, bg_color) = if card.disabled {
        (MUTED_GRAY, PANEL_BG)
    } else if is_selected && is_focused {
        (SELECTION_GREEN, LIST_HIGHLIGHT_BG)
    } else if is_selected {
        (CORNFLOWER_BLUE, LIST_HIGHLIGHT_BG)
    } else {
        (CORNFLOWER_BLUE, PANEL_BG)
    };

    // Build title with shortcut
    let mut title_spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(card.icon, Style::default().fg(GOLD)),
        Span::styled(" ", Style::default()),
        Span::styled(
            card.title,
            Style::default()
                .fg(if card.disabled { MUTED_GRAY } else { GOLD })
                .add_modifier(Modifier::BOLD),
        ),
    ];

    // Add badge if present
    if let Some(count) = card.badge {
        title_spans.push(Span::styled(" (", Style::default().fg(MUTED_GRAY)));
        title_spans.push(Span::styled(
            count.to_string(),
            Style::default().fg(SELECTION_GREEN),
        ));
        title_spans.push(Span::styled(")", Style::default().fg(MUTED_GRAY)));
    }

    // Add shortcut
    title_spans.push(Span::styled(" [", Style::default().fg(MUTED_GRAY)));
    title_spans.push(Span::styled(
        card.shortcut,
        Style::default()
            .fg(if card.disabled { MUTED_GRAY } else { SOFT_WHITE })
            .add_modifier(Modifier::BOLD),
    ));
    title_spans.push(Span::styled("]", Style::default().fg(MUTED_GRAY)));
    title_spans.push(Span::styled(" ", Style::default()));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_color))
        .title(Line::from(title_spans));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Render description inside the card
    if inner.height >= 2 {
        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Top padding
                Constraint::Length(1), // Description
                Constraint::Min(0),    // Bottom padding
            ])
            .split(inner);

        let description = Paragraph::new(Line::from(vec![Span::styled(
            card.description,
            Style::default().fg(if card.disabled { MUTED_GRAY } else { SOFT_WHITE }),
        )]))
        .alignment(Alignment::Center);

        frame.render_widget(description, content_layout[1]);
    }
}

/// Render the full action card grid (2x3)
pub fn render_action_card_grid(
    frame: &mut Frame,
    area: Rect,
    state: &ActionCardGridState,
) {
    // Split into 2 rows
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // For each row, split into 3 columns
    for (row_idx, row_area) in rows.iter().enumerate() {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(34),
                Constraint::Percentage(33),
            ])
            .split(*row_area);

        for (col_idx, col_area) in cols.iter().enumerate() {
            let card_idx = row_idx * 3 + col_idx;
            if let Some(card) = state.cards.get(card_idx) {
                let is_selected =
                    state.selected_position == (row_idx, col_idx);
                render_action_card(frame, *col_area, card, is_selected, state.is_focused);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_card_grid_navigation() {
        let mut state = ActionCardGridState::new();
        assert_eq!(state.selected_position, (0, 0));

        state.move_right();
        assert_eq!(state.selected_position, (0, 1));

        state.move_down();
        assert_eq!(state.selected_position, (1, 1));

        state.move_left();
        assert_eq!(state.selected_position, (1, 0));

        state.move_up();
        assert_eq!(state.selected_position, (0, 0));
    }

    #[test]
    fn test_card_id_from_position() {
        assert_eq!(ActionCardId::from_position(0, 0), Some(ActionCardId::Agents));
        assert_eq!(ActionCardId::from_position(1, 2), Some(ActionCardId::Help));
        assert_eq!(ActionCardId::from_position(2, 0), None);
    }

    #[test]
    fn test_set_badge() {
        let mut state = ActionCardGridState::new();
        state.set_badge(ActionCardId::Sessions, Some(5));

        let card = state.cards.iter().find(|c| c.id == ActionCardId::Sessions);
        assert_eq!(card.unwrap().badge, Some(5));
    }
}
