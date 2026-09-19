// ABOUTME: The inbox screen (D3-prime): the daemon's notification rows as the
// `inbox` section holds them, newest first, unread rows marked, the host's
// own reasons when there are none, and what the fold cut. Paint only: the
// rows, their bound and their scrub are the section's, and the one write is a
// keymap row the reducer resolves.

use ainb_app::app::sections::InboxSection;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use crate::app::AppState;
use crate::app::screens::{Screen, ids};
use crate::app::ui_state::UiState;

const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const WARNING_ORANGE: Color = Color::Rgb(255, 165, 0);

/// The screen the registry holds for `ids::INBOX`; it owns nothing, the
/// section is the state.
#[derive(Default)]
pub struct InboxScreen;

impl Screen for InboxScreen {
    fn id(&self) -> &str {
        ids::INBOX
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState, _ui: &mut UiState) {
        render(frame, area, state.inbox.get());
    }
}

/// Draw the inbox section into `area`.
pub fn render(frame: &mut Frame, area: Rect, section: &InboxSection) {
    let outer = Block::default()
        .title(Line::from(vec![
            Span::styled(" 📥 ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "Inbox",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} unread", section.unread),
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    frame.render_widget(
        Paragraph::new(body_lines(section)).wrap(Wrap { trim: false }),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(footer_line()), chunks[1]);
}

/// The rows and the section's own notes, in the order a reader wants them:
/// why there is nothing first, then the rows, then what was cut.
fn body_lines(section: &InboxSection) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(reason) = &section.absent {
        lines.push(Line::from(Span::styled(
            format!("inbox unavailable: {reason}"),
            Style::default().fg(WARNING_ORANGE),
        )));
        return lines;
    }
    if let Some(reason) = &section.unreachable {
        lines.push(Line::from(Span::styled(
            format!("daemon unreachable: {reason} (showing the last read)"),
            Style::default().fg(WARNING_ORANGE),
        )));
    }
    if section.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing in the inbox",
            Style::default().fg(MUTED_GRAY),
        )));
    }
    for row in &section.entries {
        let unread = row.read_at.is_none();
        let (marker, marker_style, text_style) = if unread {
            (
                "● ",
                Style::default().fg(GOLD),
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "○ ",
                Style::default().fg(MUTED_GRAY),
                Style::default().fg(MUTED_GRAY),
            )
        };
        lines.push(Line::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(
                format!("{:<8}", row.kind),
                Style::default().fg(CORNFLOWER_BLUE),
            ),
            Span::styled(row.summary.clone(), text_style),
        ]));
    }
    if section.rows_cut > 0 || section.summaries_cut > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "{} more rows not shown, {} summaries cut",
                section.rows_cut, section.summaries_cut
            ),
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
    }
    lines
}

fn footer_line() -> Line<'static> {
    let key =
        |k: &'static str| Span::styled(k, Style::default().fg(GOLD).add_modifier(Modifier::BOLD));
    let desc = |d: &'static str| Span::styled(d, Style::default().fg(MUTED_GRAY));
    Line::from(vec![
        key("r"),
        desc(" mark all read  "),
        key("esc"),
        desc(" back"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::events::InboxEntryRow;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn row(n: usize, summary: &str, read: bool) -> InboxEntryRow {
        InboxEntryRow {
            id: format!("01J0{n}"),
            kind: "issue".into(),
            event: "issue_created".into(),
            subject_id: format!("issue-{n}"),
            summary: summary.into(),
            recipient: "member:me".into(),
            created_at: n as i64,
            read_at: read.then_some(99),
        }
    }

    fn lines(section: &InboxSection, w: u16, h: u16) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), section)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect()
    }

    fn text(section: &InboxSection) -> String {
        lines(section, 80, 20).join("\n")
    }

    #[test]
    fn rows_draw_newest_first_with_unread_marked() {
        let section = InboxSection {
            entries: vec![
                row(2, "New issue: two", false),
                row(1, "New issue: one", true),
            ],
            unread: 1,
            ..InboxSection::default()
        };
        let drawn = lines(&section, 80, 20);
        let two = drawn.iter().position(|l| l.contains("New issue: two")).expect("row two");
        let one = drawn.iter().position(|l| l.contains("New issue: one")).expect("row one");
        assert!(two < one, "the daemon's order is kept");
        assert!(
            drawn[two].contains('●'),
            "an unread row carries the marker: {}",
            drawn[two]
        );
        assert!(
            drawn[one].contains('○'),
            "a read row does not: {}",
            drawn[one]
        );
        assert!(
            drawn[0].contains("1 unread"),
            "the title carries the section's count"
        );
        assert!(drawn.iter().any(|l| l.contains("mark all read")));
    }

    #[test]
    fn an_absent_section_says_why_and_draws_no_rows() {
        let mut section = InboxSection::default();
        section.mark_absent("connect: no daemon");
        let drawn = text(&section);
        assert!(
            drawn.contains("inbox unavailable: connect: no daemon"),
            "{drawn}"
        );
        assert!(!drawn.contains("nothing in the inbox"));
        assert!(drawn.contains("0 unread"));
    }

    #[test]
    fn an_unreachable_daemon_keeps_the_rows_and_says_so() {
        let mut section = InboxSection {
            entries: vec![row(1, "New issue: one", false)],
            unread: 1,
            received_at_ms: 5,
            ..InboxSection::default()
        };
        section.mark_read_failed("io: broken pipe");
        let drawn = text(&section);
        assert!(
            drawn.contains("daemon unreachable: io: broken pipe"),
            "{drawn}"
        );
        assert!(
            drawn.contains("New issue: one"),
            "the last read stays on screen"
        );
    }

    #[test]
    fn the_cut_counters_draw_when_something_was_cut() {
        let section = InboxSection {
            entries: vec![row(1, "x", false)],
            rows_cut: 3,
            summaries_cut: 1,
            ..InboxSection::default()
        };
        assert!(text(&section).contains("3 more rows not shown, 1 summaries cut"));
        let none = InboxSection {
            entries: vec![row(1, "x", false)],
            ..InboxSection::default()
        };
        assert!(!text(&none).contains("not shown"));
    }

    #[test]
    fn an_empty_inbox_says_so() {
        let drawn = text(&InboxSection::default());
        assert!(drawn.contains("nothing in the inbox"), "{drawn}");
    }
}
