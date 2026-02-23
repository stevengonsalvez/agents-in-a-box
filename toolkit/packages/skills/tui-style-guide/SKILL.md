---
name: tui-style-guide
description: TUI style guide for consistent terminal interface design
user-invocable: true
---

# TUI Style Guide

Premium styling patterns for agents-in-a-box TUI components.

## Color Palette

### Primary Colors
```rust
// Core accent colors
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);  // Primary accent, borders, section titles
const GOLD: Color = Color::Rgb(255, 215, 0);               // Important CTAs, emphasis, highlights
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);  // Active selections, success states
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);     // Warnings, alternate modes

// Background colors
const DARK_BG: Color = Color::Rgb(25, 25, 35);             // Main UI background
const INPUT_BG: Color = Color::Rgb(35, 35, 45);            // Input field backgrounds
const PANEL_BG: Color = Color::Rgb(30, 30, 40);            // Panel/nested container backgrounds
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);   // List item hover/selection

// Text colors
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);       // Primary text
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);       // Secondary text, hints
const MEDIUM_GRAY: Color = Color::Rgb(180, 180, 180);      // Tertiary text
const DARK_GRAY: Color = Color::Rgb(100, 100, 100);        // Disabled/faded text

// Accent/status colors
const PROGRESS_CYAN: Color = Color::Rgb(100, 200, 230);    // Loading/processing
const LIGHT_BLUE: Color = Color::Rgb(100, 200, 255);       // Info highlights
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);      // Separator lines, secondary borders
```

## Border Styles

### Standard Panel Border
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(CORNFLOWER_BLUE))
    .style(Style::default().bg(DARK_BG))
```

### Active/Focused Border
```rust
.border_style(Style::default().fg(SELECTION_GREEN))
```

### Muted/Secondary Border
```rust
.border_style(Style::default().fg(SUBDUED_BORDER))
```

## Title Patterns

### Primary Title with Icon
```rust
.title(Line::from(vec![
    Span::styled(" folder_icon ", Style::default().fg(GOLD)),
    Span::styled("Section Title",
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
]))
```

### Title with Count Badge
```rust
.title(Line::from(vec![
    Span::styled(" Section ", Style::default().fg(SOFT_WHITE)),
    Span::styled(
        format!("({})", count),
        Style::default().fg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD)
    )
]))
```

## List Item Patterns

### Selected Item
```rust
// Prefix
Span::styled("  > ", Style::default().fg(SELECTION_GREEN))
// Text
Span::styled(&item_text,
    Style::default()
        .fg(SELECTION_GREEN)
        .add_modifier(Modifier::BOLD))
```

### Unselected Item
```rust
// Prefix (spacing to align with selected)
Span::raw("    ")
// Text
Span::styled(&item_text, Style::default().fg(SOFT_WHITE))
```

### List Highlight Style
```rust
.highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG))
.highlight_symbol("> ")
```

## Status Indicators

### Icon Patterns
| Status | Icon | Color |
|--------|------|-------|
| Running | Green circle | Green |
| Stopped | Red circle | Red |
| Idle | Yellow circle | Yellow |
| Error | X mark | Red |
| Success | Checkmark | Green |
| Warning | Warning sign | Orange |
| Info | Info sign | Cyan |
| Loading | Refresh | Cyan |

### Status Badge Style
```rust
Span::styled(
    format!("[{}]", status_symbol),
    Style::default()
        .fg(status_color)
        .add_modifier(Modifier::BOLD)
)
```

## File/Folder Icons

```rust
fn get_file_icon(filename: &str) -> &'static str {
    match filename {
        f if f.ends_with(".rs") => "rust",
        f if f.ends_with(".py") => "python",
        f if f.ends_with(".js") || f.ends_with(".jsx") => "js",
        f if f.ends_with(".ts") || f.ends_with(".tsx") => "ts",
        f if f.ends_with(".md") => "markdown",
        f if f.ends_with(".json") => "json",
        f if f.ends_with(".toml") || f.ends_with(".yaml") || f.ends_with(".yml") => "config",
        f if f.ends_with(".sh") || f.ends_with(".bash") => "shell",
        f if f.ends_with(".html") => "html",
        f if f.ends_with(".css") || f.ends_with(".scss") => "css",
        f if f.ends_with(".go") => "go",
        f if f.ends_with(".java") => "java",
        _ => "file",
    }
}

// Folder icons
const FOLDER_ICON: &str = "folder";
const FOLDER_OPEN_ICON: &str = "folder-open";
```

## Tree Characters

```rust
// Expand/collapse indicators
const EXPANDED: &str = "v";
const COLLAPSED: &str = ">";
const LEAF: &str = ">";

// Tree lines
const TREE_BRANCH: &str = "|-";
const TREE_LAST: &str = "`-";
const TREE_VERTICAL: &str = "|";
```

## Keyboard Help Text

### Standard Format
```rust
Line::from(vec![
    Span::styled("Up/Down", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    Span::styled(" Navigate", Style::default().fg(MUTED_GRAY)),
    Span::styled("  |  ", Style::default().fg(SUBDUED_BORDER)),
    Span::styled("Enter", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
    Span::styled(" Select", Style::default().fg(MUTED_GRAY)),
])
```

## Input Fields

### Active Input
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(SELECTION_GREEN))
    .style(Style::default().bg(INPUT_BG))

// Cursor character
Span::styled("_", Style::default().fg(SELECTION_GREEN))
```

## Mode/Card Selection

### Selected Card
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(SELECTION_GREEN))
    .style(Style::default().bg(Color::Rgb(35, 45, 35)))  // Green tint
```

### Unselected Card
```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(Color::Rgb(70, 70, 90)))
    .style(Style::default().bg(PANEL_BG))
```

## Git Status Colors

```rust
const GIT_ADDED: Color = Color::Green;
const GIT_MODIFIED: Color = Color::Yellow;
const GIT_DELETED: Color = Color::Red;
const GIT_RENAMED: Color = Color::Blue;
const GIT_UNTRACKED: Color = Color::Magenta;
```

## Diff View Colors

```rust
// Line prefixes
const DIFF_ADDITION: Color = Color::Green;      // Lines starting with +
const DIFF_DELETION: Color = Color::Red;        // Lines starting with -
const DIFF_HUNK: Color = Color::Cyan;           // @@ lines
const DIFF_FILE_HEADER: Color = Color::Yellow;  // +++ and --- lines
const DIFF_CONTEXT: Color = Color::White;       // Unchanged lines
```

## Markdown Viewer Colors

```rust
const MD_HEADING1: Style = Style::default()
    .fg(Color::Cyan)
    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
const MD_HEADING2: Style = Style::default()
    .fg(Color::Cyan)
    .add_modifier(Modifier::BOLD);
const MD_HEADING3: Style = Style::default()
    .fg(Color::Blue)
    .add_modifier(Modifier::BOLD);
const MD_CODE_BLOCK: Style = Style::default()
    .fg(Color::Green)
    .bg(Color::Rgb(30, 30, 30));
const MD_CODE_HEADER: Style = Style::default()
    .fg(Color::Yellow)
    .add_modifier(Modifier::BOLD);
const MD_LINK: Style = Style::default()
    .fg(Color::Blue)
    .add_modifier(Modifier::UNDERLINED);
const MD_BLOCKQUOTE: Style = Style::default()
    .fg(Color::Gray)
    .add_modifier(Modifier::ITALIC);
```

## Layout Guidelines

### Centered Modal (Standard)
- Width: 80% of terminal width
- Height: 70% of terminal height

### Split Pane Ratios
- Sessions/Logs: 40/60
- Equal split: 50/50

### Internal Spacing
- Panel margins: 1px
- Content padding: Use `Constraint::Length(N)` for headers/footers
- Flexible content: `Constraint::Min(0)`

## Common Imports

```rust
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, BorderType, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame,
};
```

---

**Usage**: Reference this guide when styling new TUI components to maintain visual consistency across the application.
