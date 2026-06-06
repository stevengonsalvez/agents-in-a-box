---
name: tui-screen
description: Create TUI screens, components, and views for the agents-in-a-box terminal UI. Use when building new ratatui components, adding panels, creating list views, or styling any terminal interface element. Provides color palette, component patterns, and quality checklist.
---

# TUI Screen Creation Skill

This skill guides creation of **premium, consistent TUI components** for agents-in-a-box.

## Core Principles

All TUI components must follow these styling standards for visual consistency:

### 1. Color Palette (MANDATORY)

```rust
// Primary colors
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);  // Borders, section titles
const GOLD: Color = Color::Rgb(255, 215, 0);               // CTAs, emphasis, titles
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);  // Active selections
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);     // Warnings

// Backgrounds
const DARK_BG: Color = Color::Rgb(25, 25, 35);             // Main background
const PANEL_BG: Color = Color::Rgb(30, 30, 40);            // Panel backgrounds
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);   // Selection background

// Text
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);       // Primary text
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);       // Secondary text
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);      // Secondary borders

// Status
const PROGRESS_CYAN: Color = Color::Rgb(100, 200, 230);    // Loading/progress
```

### 2. Required Imports

```rust
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, BorderType, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
```

## Component Patterns

### Standard Panel Block
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)  // ALWAYS rounded
    .border_style(Style::default().fg(CORNFLOWER_BLUE))
    .style(Style::default().bg(DARK_BG))
    .title(Line::from(vec![
        Span::styled(" 📁 ", Style::default().fg(GOLD)),
        Span::styled("Title", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    ]))
```

### Active/Focused Panel
```rust
.border_style(Style::default().fg(SELECTION_GREEN))
```

### Title with Count Badge
```rust
.title(Line::from(vec![
    Span::styled(" 📁 ", Style::default().fg(GOLD)),
    Span::styled("Items ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    Span::styled(format!("({})", count), Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD)),
]))
```

### Bottom Help Bar (Keyboard Hints)
```rust
.title_bottom(Line::from(vec![
    Span::styled(" Enter", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    Span::styled(" select ", Style::default().fg(MUTED_GRAY)),
    Span::styled("│", Style::default().fg(SUBDUED_BORDER)),
    Span::styled(" Esc", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    Span::styled(" back ", Style::default().fg(MUTED_GRAY)),
]))
```

### Selected List Item
```rust
let mut spans = vec![];
if is_selected {
    spans.push(Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)));
} else {
    spans.push(Span::raw("  "));
}
spans.push(Span::styled(&item_text,
    if is_selected {
        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_WHITE)
    }
));

let base_style = if is_selected {
    Style::default().bg(LIST_HIGHLIGHT_BG)
} else {
    Style::default()
};

ListItem::new(Line::from(spans)).style(base_style)
```

### Empty State
```rust
Paragraph::new(vec![
    Line::from(Span::styled("✨ No items found", Style::default().fg(MUTED_GRAY))),
    Line::from(""),
    Line::from(Span::styled("Helpful hint here", Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC))),
])
```

### Input Field (Active)
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(SELECTION_GREEN))
    .style(Style::default().bg(Color::Rgb(35, 35, 45)))

// Block cursor
Span::styled("█", Style::default().fg(SELECTION_GREEN))
```

### Status Bar
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(SUBDUED_BORDER))
    .style(Style::default().bg(PANEL_BG))
```

### Status Indicator Pattern

Use for displaying configuration/connection status inline with settings:

```rust
// Status value format: "Type (details)" or "Type"
// Examples:
// - "API Key (sk-ant-xxxx••••••••)" - configured with masked details
// - "System Auth (Pro/Max Plan)" - configured, no secrets
// - "Not configured" - unconfigured state

// Status colors based on state
let (status_text, status_style) = if is_configured {
    (
        format!("{} ({})", config_type, masked_value),
        Style::default().fg(SELECTION_GREEN)
    )
} else {
    (
        "Not configured".to_string(),
        Style::default().fg(MUTED_GRAY)
    )
};

// Render as setting row
Line::from(vec![
    Span::styled("▶ ", Style::default().fg(SELECTION_GREEN)),
    Span::styled("Setting Name", Style::default().fg(SOFT_WHITE)),
    Span::styled(": ", Style::default().fg(MUTED_GRAY)),
    Span::styled(status_text, status_style),
])
```

### Status Badge Pattern

For showing status badges next to items (e.g., "Coming Soon", "Beta", "Current"):

```rust
// Status badge colors
const COMING_SOON_GRAY: Color = Color::Rgb(80, 80, 100);
const BETA_CYAN: Color = Color::Rgb(100, 200, 230);
const CURRENT_GREEN: Color = Color::Rgb(100, 200, 100);

// Badge styling
fn status_badge(text: &str, badge_type: BadgeType) -> Span {
    let (color, modifier) = match badge_type {
        BadgeType::ComingSoon => (COMING_SOON_GRAY, Modifier::ITALIC),
        BadgeType::Beta => (BETA_CYAN, Modifier::empty()),
        BadgeType::Current => (CURRENT_GREEN, Modifier::empty()),
        BadgeType::Disabled => (MUTED_GRAY, Modifier::ITALIC),
    };
    Span::styled(
        format!("[{}]", text),
        Style::default().fg(color).add_modifier(modifier)
    )
}

// Usage in list item
let mut spans = vec![
    Span::styled("Item Name", Style::default().fg(SOFT_WHITE)),
    Span::styled(" ", Style::default()),
];
if is_coming_soon {
    spans.push(status_badge("Coming Soon", BadgeType::ComingSoon));
}
if is_current {
    spans.push(Span::styled(" ", Style::default()));
    spans.push(status_badge("Current", BadgeType::Current));
}
```

### Configuration Status Panel

For showing multiple status items in a summary view:

```rust
// Layout: status indicator | label | value
fn render_status_row(label: &str, value: &str, is_ok: bool) -> Line<'static> {
    let indicator = if is_ok {
        Span::styled("✓ ", Style::default().fg(SELECTION_GREEN))
    } else {
        Span::styled("○ ", Style::default().fg(MUTED_GRAY))
    };

    Line::from(vec![
        indicator,
        Span::styled(label, Style::default().fg(SOFT_WHITE)),
        Span::styled(": ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            value.to_string(),
            if is_ok {
                Style::default().fg(SOFT_WHITE)
            } else {
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC)
            }
        ),
    ])
}

// Example usage
let rows = vec![
    render_status_row("Claude Auth", "API Key (sk-ant-••••)", true),
    render_status_row("GitHub", "System Default", true),
    render_status_row("AWS Bedrock", "Not configured", false),
];
```

## Icon Reference

| Element | Icon | Color |
|---------|------|-------|
| Folder closed | 📁 | CORNFLOWER_BLUE |
| Folder open | 📂 | CORNFLOWER_BLUE |
| File generic | 📄 | SOFT_WHITE |
| File Rust | 🦀 | - |
| File Python | 🐍 | - |
| File Markdown | 📝 | - |
| Config | ⚙️ | - |
| Success/Check | ✓ | SELECTION_GREEN |
| Running | 🟢 | Green |
| Stopped | 🔴 | Red |
| Warning | ⚠️ | WARNING_ORANGE |
| Loading | 🔄 | WARNING_ORANGE |
| Push ready | 🚀 | SELECTION_GREEN |
| Edit | ✏️ | GOLD |
| Stats | 📊 | GOLD |
| Sparkle | ✨ | GOLD |

## Tree Characters

```rust
const EXPANDED: &str = "▼";
const COLLAPSED: &str = "▶";
const TREE_BRANCH: &str = "├─";
const TREE_LAST: &str = "└─";
const TREE_VERTICAL: &str = "│";
```

## Layout Guidelines

| Element | Constraint |
|---------|------------|
| Tab bar | `Length(3)` |
| Status bar | `Length(3)` |
| Content area | `Min(0)` |
| Modal width | 80% of terminal |
| Modal height | 70% of terminal |
| Split panes | 40/60 or 50/50 |

## Component Structure Template

```rust
pub struct MyComponentState {
    pub selected_index: usize,
    pub items: Vec<MyItem>,
    pub scroll_offset: usize,
    pub is_active: bool,
}

impl MyComponentState {
    pub fn new() -> Self { /* ... */ }
    pub fn next_item(&mut self) { /* ... */ }
    pub fn previous_item(&mut self) { /* ... */ }
}

pub struct MyComponent;

impl MyComponent {
    pub fn render(frame: &mut Frame, area: Rect, state: &MyComponentState) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Header
                Constraint::Min(0),      // Content
                Constraint::Length(3),  // Status
            ])
            .split(area);

        Self::render_header(frame, chunks[0], state);
        Self::render_content(frame, chunks[1], state);
        Self::render_status_bar(frame, chunks[2], state);
    }
}
```

## Modal Popup Patterns (MANDATORY for Config/Settings)

All configuration and settings screens MUST use popup-based interactions rather than inline editing:

### Popup Types

| Setting Type | Popup Behavior |
|--------------|----------------|
| Choice/Enum | Selection list with ▶ indicator, Up/Down navigation |
| Text/String | Text input field with cursor |
| Boolean | Enabled/Disabled toggle selection |
| Number | Numeric input field |

### Popup Structure

```rust
// Popup overlay pattern
use ratatui::widgets::Clear;

// 1. Calculate centered position
let popup_width = 50u16.min(area.width - 4);
let popup_height = 12u16.min(area.height - 4);
let popup_x = (area.width - popup_width) / 2;
let popup_y = (area.height - popup_height) / 2;
let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

// 2. Clear background
frame.render_widget(Clear, popup_area);

// 3. Draw popup block
let block = Block::default()
    .title(Span::styled(
        format!(" {} ", title),
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    ))
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(CORNFLOWER_BLUE))
    .style(Style::default().bg(PANEL_BG));
```

### Choice Selection Popup

```rust
// Selection indicator
let indicator = if is_selected { "▶ " } else { "  " };
let style = if is_selected {
    Style::default()
        .fg(SELECTION_GREEN)
        .bg(LIST_HIGHLIGHT_BG)
        .add_modifier(Modifier::BOLD)
} else {
    Style::default().fg(SOFT_WHITE)
};

Line::from(vec![
    Span::styled(indicator, Style::default().fg(SELECTION_GREEN)),
    Span::styled(option_text, style),
])
```

### Boolean Toggle Popup

```rust
// Enabled/Disabled options
Line::from(vec![
    Span::styled(if value { "▶ " } else { "  " }, Style::default().fg(SELECTION_GREEN)),
    Span::styled("✓ Enabled", if value { selected_style } else { normal_style }),
]),
Line::from(vec![
    Span::styled(if !value { "▶ " } else { "  " }, Style::default().fg(SELECTION_GREEN)),
    Span::styled("✗ Disabled", if !value { selected_style } else { normal_style }),
]),
```

### Text Input Popup

```rust
// Input field with cursor
let input_block = Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(GOLD))
    .style(Style::default().bg(LIST_HIGHLIGHT_BG));

// Cursor indicator
let display = format!("{}|", value);
```

### Popup Help Bar

```rust
// Navigation hints at bottom
let help_items = vec![
    ("↑↓", "select"),   // For choice/boolean
    ("Enter", "confirm"),
    ("Esc", "cancel"),
];

// Or for text input
let help_items = vec![
    ("Enter", "save"),
    ("Esc", "cancel"),
];
```

### Reference Implementation

See `src/components/config_popup.rs` for complete popup component with:
- `ConfigPopupType` enum (Choice, TextInput, Boolean, NumberInput)
- `ConfigPopupState` for state management
- Full render implementation

## Quality Checklist

Before completing any TUI component:

- [ ] All panels use `BorderType::Rounded`
- [ ] All borders use `CORNFLOWER_BLUE` (or `SELECTION_GREEN` when focused)
- [ ] All backgrounds use `DARK_BG` or `PANEL_BG`
- [ ] Titles use gold icon + gold bold text
- [ ] Lists use `▶` indicator with `SELECTION_GREEN`
- [ ] Selected items have `LIST_HIGHLIGHT_BG` background
- [ ] Help bar uses gold keys + muted descriptions
- [ ] Empty states have icon + italic hint
- [ ] **Config/settings use popup-based editing (not inline)**
- [ ] Events added to `src/app/events.rs`
- [ ] `cargo check` passes

## Reference Components

Study these for patterns:
- `src/components/git_view.rs` - Tabs, lists, tree view, input, markdown
- `src/components/session_detail.rs` - Panel layouts, status indicators
- `src/components/new_session_panel.rs` - Modal dialogs, card selection
