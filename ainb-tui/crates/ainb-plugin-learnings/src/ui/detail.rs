//! Detail / read pane — the full view of a single [`LearningRecord`].
//!
//! Opened with `Enter` on a selected list row (Browse today, Search/Graph in
//! later phases — the pane is tab-agnostic, it just renders whatever record it
//! was handed). Renders, top to bottom:
//!
//! - a gold title (the record `title`) + the `key_insight` strapline,
//! - the full markdown `body_md`, wrapped to the pane width,
//! - an **entities** list (name · type — from the `.entities.yaml` sidecar),
//! - the typed **relationships** as `source --<type>--> target` lines (also
//!   from the sidecar, NOT the graphml),
//! - a **provenance** line: `source_tool · source_path · conf <confidence>`.
//!
//! Close key is `Backspace` (the host-forwarded "pop") — `Esc` is reserved by
//! the host (it pops the whole plugin screen to home and is never forwarded;
//! see `ainb-core/src/app/screens/builtin.rs::is_host_reserved_key`). The pane
//! ALSO accepts `Esc` defensively for any caller that injects keys directly
//! (the testkit), so both close it.
//!
//! All width math goes through `chars()` — never byte slicing — so a record
//! with multibyte body / entity text can't panic on a char boundary (the Rust
//! UTF-8 truncate trap).

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap};

use ainb_plugin_sdk::KeyCode;

use super::{CORNFLOWER_BLUE, DARK_BG, GOLD, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE};
use crate::data::LearningRecord;

/// Open-state for the Detail pane: the record currently being read. `None`
/// means the pane is closed and the underlying tab body shows through.
///
/// Kept as a cloned [`LearningRecord`] (not an index) so the pane is decoupled
/// from the list it was opened from — a filter change or tab switch behind it
/// can't dangle the reference, and any tab can open the pane the same way.
#[derive(Debug, Default)]
pub struct DetailState {
    /// The record under view, or `None` when the pane is closed.
    record: Option<LearningRecord>,
}

impl DetailState {
    /// `true` while the pane is open (rendering over the tab body).
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.record.is_some()
    }

    /// Open the pane on `record` (clones it so the pane outlives the list it
    /// came from).
    pub fn open(&mut self, record: &LearningRecord) {
        self.record = Some(record.clone());
    }

    /// Close the pane. Idempotent — closing an already-closed pane is a no-op.
    pub fn close(&mut self) {
        self.record = None;
    }

    /// Route a key while the pane is open. Returns `true` when the key was
    /// consumed (so the caller bumps its render generation and does NOT fall
    /// through to the underlying tab).
    ///
    /// `Backspace` (host-forwarded close) and `Esc` (direct-injection close)
    /// both close the pane. Every other key is swallowed while open so the
    /// list behind it can't move under the reader.
    pub fn handle_key(&mut self, code: &KeyCode) -> bool {
        if !self.is_open() {
            return false;
        }
        match code {
            KeyCode::Backspace | KeyCode::Esc => {
                self.close();
                true
            }
            // Swallow everything else while reading — the underlying list must
            // not scroll/filter behind the open pane.
            _ => true,
        }
    }
}

/// Render the open Detail pane into `area`. No-op when the pane is closed or
/// the area is degenerate.
pub fn render(buf: &mut RBuffer, area: RRect, state: &DetailState) {
    let Some(record) = state.record.as_ref() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(detail_title(record))
        .style(Style::default().bg(DARK_BG));
    let inner = outer.inner(area);
    outer.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Reserve the bottom row for the help bar; everything else is the body.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    render_body(buf, rows[0], record);
    render_help_bar(buf, rows[1]);
}

/// The pane title: the record `title` in gold-bold, wrapped in the `Detail`
/// chrome so a glance distinguishes it from the Browse panel.
fn detail_title(record: &LearningRecord) -> Span<'static> {
    Span::styled(
        format!(" 📖 {} ", truncate_chars(&record.title, 80)),
        Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
    )
}

/// Render the scrolling body region: key_insight strapline, the wrapped
/// markdown body, the entities list, the typed relationships, then provenance.
fn render_body(buf: &mut RBuffer, area: RRect, record: &LearningRecord) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // key_insight strapline (one wrapped paragraph handled by ratatui `Wrap`).
    if !record.key_insight.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("key_insight: ", Style::default().fg(MUTED_GRAY)),
            Span::styled(record.key_insight.clone(), Style::default().fg(SOFT_WHITE)),
        ]));
        lines.push(Line::from(""));
    }

    // Full markdown body, line by line. `Wrap { trim: false }` on the
    // Paragraph soft-wraps long lines to the pane width — no manual byte math.
    for raw in record.body_md.lines() {
        lines.push(Line::from(Span::styled(
            raw.to_string(),
            Style::default().fg(SOFT_WHITE),
        )));
    }

    // Entities section.
    if !record.entities.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_header("entities"));
        for ent in &record.entities {
            // `name · type` — name in soft-white, type muted. The exact entity
            // name is asserted by the P6 tests, so it's rendered verbatim.
            let mut spans = vec![
                Span::styled("  • ", Style::default().fg(MUTED_GRAY)),
                Span::styled(ent.name.clone(), Style::default().fg(SOFT_WHITE)),
            ];
            if !ent.entity_type.is_empty() {
                spans.push(Span::styled(
                    format!("  ({})", ent.entity_type),
                    Style::default().fg(MUTED_GRAY),
                ));
            }
            lines.push(Line::from(spans));
        }
    }

    // Relationships section: `source --<type>--> target` per the design mock.
    if !record.relationships.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_header("relationships"));
        for rel in &record.relationships {
            let rel_type = if rel.rel_type.is_empty() {
                "relates_to"
            } else {
                rel.rel_type.as_str()
            };
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(rel.source.clone(), Style::default().fg(SOFT_WHITE)),
                // The typed arrow `--<type>-->` is the token the tests lock
                // (e.g. `--solves-->`). Rendered as one contiguous span so it
                // survives the row-join intact.
                Span::styled(
                    format!(" --{rel_type}--> "),
                    Style::default().fg(SELECTION_GREEN),
                ),
                Span::styled(rel.target.clone(), Style::default().fg(SOFT_WHITE)),
            ]));
        }
    }

    // Provenance line: `provenance: <source_tool> · <source_path> · conf <c>`.
    lines.push(Line::from(""));
    lines.push(provenance_line(record));

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(DARK_BG));
    para.render(area, buf);
}

/// A gold section header (`entities` / `relationships`).
fn section_header(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(GOLD).add_modifier(RModifier::BOLD | RModifier::UNDERLINED),
    ))
}

/// The provenance line: source_tool · source_path · confidence. Absent fields
/// render as `—` so the line shape is stable. The `source_path` is rendered
/// verbatim (its basename is the unique provenance token the tests assert).
fn provenance_line(record: &LearningRecord) -> Line<'static> {
    let tool = record.provenance.source_tool.as_deref().unwrap_or("—");
    let path = record.provenance.source_path.as_deref().unwrap_or("—");
    Line::from(vec![
        Span::styled("provenance: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(tool.to_string(), Style::default().fg(SOFT_WHITE)),
        Span::styled(" · ", Style::default().fg(MUTED_GRAY)),
        Span::styled(path.to_string(), Style::default().fg(SOFT_WHITE)),
        Span::styled(" · ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            format!("conf {:.2}", record.confidence),
            Style::default().fg(SOFT_WHITE),
        ),
    ])
}

/// Bottom help bar for the Detail pane. `Backspace back` is the live close key
/// (host-forwarded). `Esc home` is shown muted because the host claims Esc and
/// navigates to home rather than popping the pane — honest about what each key
/// actually does (style-guide help-bar honesty).
fn render_help_bar(buf: &mut RBuffer, area: RRect) {
    let spans = vec![
        Span::raw(" "),
        Span::styled(
            "BkSp",
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(" back", Style::default().fg(MUTED_GRAY)),
        Span::styled("   ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Esc",
            Style::default().fg(MUTED_GRAY).add_modifier(RModifier::DIM),
        ),
        Span::styled(
            " home",
            Style::default().fg(MUTED_GRAY).add_modifier(RModifier::DIM),
        ),
    ];
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Truncate a string to at most `max` characters, appending `…` when clipped.
///
/// Char-based — never byte-slices — so a multibyte title can't panic on a char
/// boundary (the Rust UTF-8 truncate trap). Used for the pane title chrome.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Entity, Provenance, Relationship};

    fn sample_record() -> LearningRecord {
        LearningRecord {
            id: "lrn-x".into(),
            title: "Sample".into(),
            scope: "universal".into(),
            confidence: 0.85,
            category: "process".into(),
            tags: vec![],
            source_tool: Some("claude".into()),
            project: None,
            key_insight: "Insight.".into(),
            body_md: "## Solution\nbody".into(),
            entities: vec![Entity {
                name: "git pull --rebase".into(),
                entity_type: "tool".into(),
                description: String::new(),
            }],
            relationships: vec![Relationship {
                source: "a".into(),
                target: "stale plan execution".into(),
                rel_type: "solves".into(),
                description: String::new(),
                strength: Some(9),
            }],
            provenance: Provenance {
                source_tool: Some("claude".into()),
                source_path: Some("/x/feedback_audit_after_rebase.md".into()),
                ..Provenance::default()
            },
        }
    }

    #[test]
    fn open_then_close_toggles_is_open() {
        let mut st = DetailState::default();
        assert!(!st.is_open());
        st.open(&sample_record());
        assert!(st.is_open());
        // Backspace closes.
        assert!(st.handle_key(&KeyCode::Backspace));
        assert!(!st.is_open());
    }

    #[test]
    fn esc_also_closes_when_open() {
        let mut st = DetailState::default();
        st.open(&sample_record());
        assert!(st.handle_key(&KeyCode::Esc));
        assert!(!st.is_open());
    }

    #[test]
    fn keys_are_swallowed_while_open_but_not_when_closed() {
        let mut st = DetailState::default();
        // Closed: does not claim keys (caller falls through to the tab).
        assert!(!st.handle_key(&KeyCode::Down));
        // Open: claims every key (the list behind must not move).
        st.open(&sample_record());
        assert!(st.handle_key(&KeyCode::Down));
        assert!(st.is_open(), "Down must not close the pane");
    }

    #[test]
    fn truncate_chars_is_char_safe_on_multibyte() {
        // A multibyte string truncated mid-way must not panic and must keep
        // whole chars.
        let s = "🧠".repeat(10);
        let out = truncate_chars(&s, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'));
    }
}
