// ABOUTME: The inbox screen (D3-prime): the daemon's notification rows as the
// `inbox` section holds them, newest first, unread rows marked, the host's
// own reasons when there are none, and what the fold cut. Paint only: the
// rows, their bound and their scrub are the section's, and the one write is a
// keymap row the reducer resolves.

use ainb_app::app::sections::{INBOX_SUMMARY_CUT_MARKER, InboxSection};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

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

/// Draw the inbox section into `area`, its rows from the section's `scroll`
/// down, one row per line and clipped at the width, so the footer can say
/// which rows are on screen.
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

    let notes = note_lines(section);
    let room = usize::from(chunks[0].height).saturating_sub(notes.len());
    let scroll = section.scroll.min(section.entries.len().saturating_sub(1));
    let shown = section.entries.len().min(scroll.saturating_add(room));
    // What is left of the line after the mark, the kind and the age.
    let text_width = usize::from(chunks[0].width).saturating_sub(ROW_PREFIX_WIDTH);
    let mut lines = notes;
    lines.extend(
        section.entries[scroll..shown]
            .iter()
            .map(|row| row_line(row, section.received_at_ms, text_width)),
    );
    frame.render_widget(Paragraph::new(lines), chunks[0]);
    frame.render_widget(
        Paragraph::new(footer_line(scroll, shown, section.entries.len())),
        chunks[1],
    );
}

/// The section's own notes, above the rows: why there is nothing first, then
/// what was cut.
fn note_lines(section: &InboxSection) -> Vec<Line<'static>> {
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
    // What was cut comes before the rows: a screen too short for a hundred
    // rows still says what it does not show.
    if let Some(cut) = cut_line(section) {
        lines.push(Line::from(Span::styled(
            cut,
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
    }
    if section.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing in the inbox",
            Style::default().fg(MUTED_GRAY),
        )));
    }
    lines
}

/// What the fold cut, in the words the desktop's inbox uses for the same
/// counters, so one facts list reads both renderers.
fn cut_line(section: &InboxSection) -> Option<String> {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut lost = Vec::new();
    if section.rows_cut > 0 {
        lost.push(format!(
            "{} not sent",
            plural(section.rows_cut, "older entry", "older entries")
        ));
    }
    if section.summaries_cut > 0 {
        lost.push(format!(
            "{} shortened",
            plural(section.summaries_cut, "summary", "summaries")
        ));
    }
    (!lost.is_empty()).then(|| lost.join(", "))
}

/// The columns before the summary: the read mark (2), the kind (8) and the
/// age (5).
const ROW_PREFIX_WIDTH: usize = 15;

/// `summary` as one line of `width` cells. A summary the host cut ends in
/// the cut marker, and the marker stays at the visible end when the line is
/// too narrow for the whole summary, so what the host cut is said on screen
/// however wide the screen is; `…` says the screen clipped the rest.
fn fit(summary: &str, width: usize) -> String {
    if summary.chars().count() <= width {
        return summary.to_string();
    }
    let marker = INBOX_SUMMARY_CUT_MARKER;
    if summary.ends_with(marker) && width > marker.chars().count() + 1 {
        let head: String = summary.chars().take(width - marker.chars().count() - 1).collect();
        return format!("{head}…{marker}");
    }
    let head: String = summary.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

/// One row: its read mark, kind, age and summary, fitted to `text_width`.
fn row_line(
    row: &ainb_hangar_proto::events::InboxEntryRow,
    now_ms: i64,
    text_width: usize,
) -> Line<'static> {
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
    Line::from(vec![
        Span::styled(marker, marker_style),
        Span::styled(
            format!("{:<8}", row.kind),
            Style::default().fg(CORNFLOWER_BLUE),
        ),
        Span::styled(
            format!("{:>4} ", age(row.created_at, now_ms)),
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled(fit(&row.summary, text_width), text_style),
    ])
}

/// The row's age as the daemon's clock gives it: `created_at` against the
/// clock the read landed on, both epoch milliseconds, in the largest unit
/// that is at least one. A read that has not landed has no clock, and a
/// stamp past it (skew) is `now`.
fn age(created_at_ms: i64, now_ms: i64) -> String {
    if now_ms <= 0 {
        return "?".to_string();
    }
    let secs = (now_ms - created_at_ms) / 1000;
    match secs {
        i64::MIN..=0 => "now".to_string(),
        1..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The keys, and which rows are on screen: `rows 3-20 of 100`.
fn footer_line(scroll: usize, shown: usize, total: usize) -> Line<'static> {
    let key =
        |k: &'static str| Span::styled(k, Style::default().fg(GOLD).add_modifier(Modifier::BOLD));
    let desc = |d: &'static str| Span::styled(d, Style::default().fg(MUTED_GRAY));
    let range = if total == 0 {
        String::new()
    } else {
        format!("  rows {}-{shown} of {total}", scroll + 1)
    };
    Line::from(vec![
        key("r"),
        desc(" mark all read  "),
        key("j/k"),
        desc(" scroll  "),
        key("esc"),
        desc(" back"),
        Span::styled(range, Style::default().fg(MUTED_GRAY)),
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
        assert!(text(&section).contains("3 older entries not sent, 1 summary shortened"));
        let none = InboxSection {
            entries: vec![row(1, "x", false)],
            ..InboxSection::default()
        };
        assert!(!text(&none).contains("not sent"));
    }

    #[test]
    fn the_age_is_the_daemons_stamp_against_the_reads_clock() {
        assert_eq!(age(0, 0), "?", "no read, no clock");
        assert_eq!(age(5_000, 5_000), "now");
        assert_eq!(age(9_000, 5_000), "now", "skew is not a negative age");
        assert_eq!(age(5_000, 50_000), "45s");
        assert_eq!(age(0, 90_000), "1m");
        assert_eq!(age(0, 7_200_000), "2h");
        assert_eq!(age(0, 3 * 86_400_000), "3d");
        let section = InboxSection {
            entries: vec![row(1, "New issue: one", false)],
            received_at_ms: 1 + 120_000,
            ..InboxSection::default()
        };
        let drawn = text(&section);
        assert!(drawn.contains("issue     2m New issue: one"), "{drawn}");
    }

    #[test]
    fn the_scroll_picks_the_first_row_and_the_footer_says_which_rows_show() {
        let section = InboxSection {
            entries: (1..=10).map(|n| row(n, &format!("Row {n}"), false)).collect(),
            rows_cut: 1,
            ..InboxSection::default()
        };
        // Height 8: two border rows, one footer, one counters note, four rows.
        let at = |scroll: usize| InboxSection {
            scroll,
            ..section.clone()
        };
        let drawn = lines(&at(3), 60, 8);
        let joined = drawn.join("\n");
        assert!(
            !joined.contains("Row 3 "),
            "rows above the scroll are not drawn:\n{joined}"
        );
        assert!(joined.contains("Row 4"), "{joined}");
        assert!(joined.contains("Row 7"), "{joined}");
        assert!(
            !joined.contains("Row 8"),
            "rows past the screen are not drawn:\n{joined}"
        );
        assert!(joined.contains("rows 4-7 of 10"), "{joined}");
        assert!(joined.contains("j/k"), "{joined}");
        let top = lines(&at(0), 60, 8).join("\n");
        assert!(top.contains("rows 1-4 of 10"), "{top}");
        let past = lines(&at(99), 60, 8).join("\n");
        assert!(
            past.contains("Row 10"),
            "a scroll past the end draws the last row:\n{past}"
        );
    }

    #[test]
    fn a_cut_summary_keeps_its_marker_at_the_visible_end_of_a_narrow_line() {
        let long = format!("{}{INBOX_SUMMARY_CUT_MARKER}", "x".repeat(300));
        let section = InboxSection {
            entries: vec![row(1, &long, false)],
            ..InboxSection::default()
        };
        let drawn = lines(&section, 60, 6);
        let line = drawn.iter().find(|l| l.contains("xxx")).expect("the row");
        assert!(line.contains("… [cut]"), "{line}");
        assert!(
            !drawn.join("\n").contains("[cut]x"),
            "the marker is the end"
        );
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("a plain long summary", 8), "a plain…");
    }

    #[test]
    fn an_empty_inbox_says_so() {
        let drawn = text(&InboxSection::default());
        assert!(drawn.contains("nothing in the inbox"), "{drawn}");
    }
}
