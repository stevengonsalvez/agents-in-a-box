// ABOUTME: Confirmation dialog component for displaying yes/no prompts with keyboard navigation

use crate::app::state::{AppState, ConfirmationDialog};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

/// Borders (2) + one message row + two button rows: the smallest box that can
/// show a confirmation and the keys that answer it.
const MIN_DIALOG_HEIGHT: u16 = 5;

pub struct ConfirmationDialogComponent;

impl ConfirmationDialogComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(dialog) = &state.confirmation_dialog {
            // Too small for a bordered box. The dialog still owns the keyboard,
            // so draw the buttons alone rather than leaving an invisible modal
            // that swallows every keypress.
            if area.width < 10 || area.height < MIN_DIALOG_HEIGHT {
                render_compact(frame, area, dialog);
                return;
            }
            // Calculate dialog size (center it)
            let dialog_width = 60.min(area.width - 4);
            let (dialog_height, warning_rows) = dialog_layout(dialog, dialog_width, area.height);

            let dialog_area = Rect {
                x: (area.width - dialog_width) / 2,
                y: (area.height - dialog_height) / 2,
                width: dialog_width,
                height: dialog_height,
            };

            // Clear ONLY the dialog area, not the entire screen
            // This prevents ghost/duplicate UI elements from appearing
            frame.render_widget(Clear, dialog_area);

            // Render dialog background
            let block = Block::default()
                .title(dialog.title.clone())
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black));

            frame.render_widget(block, dialog_area);

            // Create inner layout
            let inner_area = Rect {
                x: dialog_area.x + 1,
                y: dialog_area.y + 1,
                width: dialog_area.width - 2,
                height: dialog_area.height - 2,
            };

            // Build constraints based on whether warning is present
            let constraints = if warning_rows > 0 {
                vec![
                    Constraint::Length(warning_rows), // Warning
                    Constraint::Min(1),               // Message
                    Constraint::Length(2),            // Buttons
                ]
            } else {
                vec![
                    Constraint::Min(1),    // Message
                    Constraint::Length(2), // Buttons
                ]
            };

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(inner_area);

            // Determine which chunk indices to use based on warning presence
            let warning_text = dialog.warning.as_ref().filter(|_| warning_rows > 0);
            let (message_chunk, button_chunk) = if let Some(warning_text) = warning_text {
                // Render warning with yellow/orange highlight
                let warning = Paragraph::new(warning_text.as_str())
                    .style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true });

                frame.render_widget(warning, chunks[0]);
                (1, 2)
            } else {
                (0, 1)
            };

            // Render message
            let message = Paragraph::new(dialog.message.clone())
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::White));

            frame.render_widget(message, chunks[message_chunk]);

            render_buttons(frame, chunks[button_chunk], dialog);
        }
    }
}

/// Draw the dialog's buttons. Tri-option mode (e.g. Stop / Delete / Cancel)
/// takes precedence; binary mode is used by all legacy callsites.
fn render_buttons(frame: &mut Frame, area: Rect, dialog: &ConfirmationDialog) {
    if let Some(options) = dialog.options.as_ref() {
        let n = options.len().max(1);
        let pct = (100u16 / n as u16).max(1);
        let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Percentage(pct)).collect();
        let button_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);

        for (i, opt) in options.iter().enumerate() {
            let style = if i == dialog.selected_index {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };
            let label = format!("[ {} ]", opt.label);
            let button = Paragraph::new(label).style(style).alignment(Alignment::Center);
            if let Some(area) = button_chunks.get(i) {
                frame.render_widget(button, *area);
            }
        }
    } else {
        let button_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        // Yes button
        let yes_style = if dialog.selected_option {
            Style::default().fg(Color::Black).bg(Color::White)
        } else {
            Style::default().fg(Color::White)
        };

        let yes_button = Paragraph::new("Yes").style(yes_style).alignment(Alignment::Center);

        frame.render_widget(yes_button, button_chunks[0]);

        // No button
        let no_style = if !dialog.selected_option {
            Style::default().fg(Color::Black).bg(Color::White)
        } else {
            Style::default().fg(Color::White)
        };

        let no_button = Paragraph::new("No").style(no_style).alignment(Alignment::Center);

        frame.render_widget(no_button, button_chunks[1]);
    }
}

/// Last-resort rendering for an area too small to hold a bordered dialog: the
/// title on one row (if there is one to spare) and the buttons underneath, so
/// the modal that is holding the keyboard is at least visible and answerable.
fn render_compact(frame: &mut Frame, area: Rect, dialog: &ConfirmationDialog) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    let button_row = area.height.min(2);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(button_row)])
        .split(area);
    if chunks[0].height > 0 {
        let title = Paragraph::new(dialog.title.clone())
            .style(Style::default().fg(Color::White).bg(Color::Black))
            .wrap(Wrap { trim: true });
        frame.render_widget(title, chunks[0]);
    }
    render_buttons(frame, chunks[1], dialog);
}

/// Rows the warning block occupies, 0 when the dialog has no warning.
///
/// At least 3 so short warnings keep the banner they have always had, and at
/// most 6 so one long warning cannot push the buttons off screen. The height
/// calculation uses this same number, so the box always reserves exactly what
/// the layout hands out.
fn warning_row_count(dialog: &ConfirmationDialog, dialog_width: u16) -> u16 {
    dialog.warning.as_ref().map_or(0, |w| {
        wrapped_line_count(w, dialog_width.saturating_sub(2)).clamp(3, 6)
    })
}

/// Dialog height: the historic fixed size (8, or 11 with a warning) unless the
/// wrapped body needs more rows, in which case the box grows rather than
/// clipping. The bulk Stop/Delete dialog names the sessions it affects, so it
/// is routinely taller than the fixed size allowed.
/// `(box height, rows the warning banner gets)`, computed together so the two
/// cannot disagree. The box is the historic fixed size (8, or 11 with a
/// warning) unless the wrapped body needs more rows, in which case it grows
/// rather than clipping; the bulk Stop/Delete dialog names the sessions it
/// affects, so it is routinely taller than the fixed size allowed. When the box
/// cannot grow far enough, the banner yields first: the message keeps a row and
/// the buttons keep both of theirs, so the answer is never invisible.
fn dialog_layout(dialog: &ConfirmationDialog, dialog_width: u16, max_height: u16) -> (u16, u16) {
    let message_rows = wrapped_line_count(&dialog.message, dialog_width.saturating_sub(2));
    let wanted_warning = warning_row_count(dialog, dialog_width);
    let base = if dialog.warning.is_some() { 11 } else { 8 };
    // 2 borders + warning block + message + 2 button rows, saturating because
    // `wrapped_line_count` is allowed to return u16::MAX for a pathological
    // message.
    let needed = wanted_warning.saturating_add(message_rows).saturating_add(4);
    let height = base.max(needed).min(max_height);
    let warning_rows = wanted_warning.min(height.saturating_sub(MIN_DIALOG_HEIGHT));
    (height, warning_rows)
}

/// Rows `text` needs at `width`, emulating the greedy word wrap ratatui applies
/// with `Wrap { trim: true }` (which never splits a word that fits on a line).
///
/// Deliberately counts chars rather than display columns, so a run of
/// double-width (CJK) glyphs is under-counted; the extra row added per paragraph
/// absorbs that and the odd wide emoji. Measure display width here if that stops
/// being enough.
fn wrapped_line_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let mut rows: usize = 0;
    for paragraph in text.lines() {
        let mut paragraph_rows = 1usize;
        let mut used = 0usize;
        for word in paragraph.split_whitespace() {
            let len = word.chars().count();
            if used == 0 {
                used = len;
            } else if used + 1 + len <= width {
                used += 1 + len;
            } else {
                paragraph_rows += 1;
                used = len;
            }
            // A word longer than the line spills onto further rows.
            while used > width {
                paragraph_rows += 1;
                used -= width;
            }
        }
        rows += paragraph_rows + 1; // one row of slack, see above
    }
    u16::try_from(rows.max(1)).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::ConfirmAction;

    fn dialog(message: &str, warning: Option<&str>) -> ConfirmationDialog {
        ConfirmationDialog {
            title: "T".to_string(),
            message: message.to_string(),
            confirm_action: ConfirmAction::Cancel,
            selected_option: false,
            warning: warning.map(str::to_string),
            options: None,
            selected_index: 0,
        }
    }

    /// Short dialogs keep the size they have always had.
    #[test]
    fn short_dialogs_keep_their_historic_size() {
        assert_eq!(dialog_layout(&dialog("Are you sure?", None), 60, 40).0, 8);
        assert_eq!(
            dialog_layout(
                &dialog("Are you sure?", Some("⚠️ 1 uncommitted file(s)")),
                60,
                40
            )
            .0,
            11
        );
    }

    /// The height must reserve exactly the rows the layout hands to the warning,
    /// or the message is clipped by the difference.
    #[test]
    fn height_reserves_the_rows_the_layout_gives_the_warning() {
        let long_message = "12 session(s): alpha, beta, gamma, and 9 more\n\
             Stop keeps every worktree and resumes later. Delete removes 12 worktree(s).";
        let d = dialog(
            long_message,
            Some("⚠️ 4 uncommitted file(s) in 2 session(s): alpha (3)"),
        );
        let (height, warning_rows) = dialog_layout(&d, 60, 40);
        let message_rows = wrapped_line_count(&d.message, 58);
        // 2 borders + warning + message + 2 button rows must all fit.
        assert!(
            height >= 4 + warning_rows + message_rows,
            "height {height} clips a {message_rows}-row message under a {warning_rows}-row warning"
        );
    }

    /// Word wrap never splits a word that fits, so the estimate must not fall
    /// below the row count a greedy wrap actually produces.
    #[test]
    fn wrapped_line_count_accounts_for_word_wrapping() {
        // Three 10-char words at width 20 wrap to 2 rows, not ceil(32/20) = 2
        // by character count alone; the slack row keeps the estimate safe.
        assert!(wrapped_line_count("abcdefghij abcdefghij abcdefghij", 20) >= 2);
        // A word longer than the line still spills across rows.
        assert!(wrapped_line_count(&"x".repeat(45), 20) >= 3);
        // Explicit newlines each start a new row.
        assert!(wrapped_line_count("a\nb\nc", 60) >= 3);
    }

    /// A terminal too small for borders, a message and buttons must not
    /// underflow the inner-area arithmetic.
    #[test]
    fn tiny_terminals_are_clamped_not_underflowed() {
        let d = dialog("Are you sure?", None);
        for height in MIN_DIALOG_HEIGHT..=12u16 {
            let (computed, _) = dialog_layout(&d, 60, height);
            assert!(
                computed >= 2,
                "height {computed} would underflow inner_area"
            );
            assert!(computed <= height, "dialog must fit the area");
        }
    }

    /// A short terminal must not squeeze the button row to nothing: the warning
    /// banner yields first, because a destructive dialog whose Stop / Delete /
    /// Cancel row is invisible is worse than one with no banner.
    #[test]
    fn a_short_dialog_drops_the_warning_before_the_buttons() {
        let d = dialog(
            "12 session(s): alpha, beta, gamma, and 9 more",
            Some("⚠️ 40 uncommitted file(s) in 12 session(s): alpha (12), beta (9), gamma (7)"),
        );
        for height in MIN_DIALOG_HEIGHT..=14u16 {
            let (box_height, warning_rows) = dialog_layout(&d, 60, height);
            let inner = box_height - 2;
            assert!(
                warning_rows + 2 < inner,
                "at height {height} the warning ({warning_rows}) leaves no room for the \
                 message and buttons in {inner} inner rows"
            );
        }
    }

    /// A pathological message must not overflow the height arithmetic.
    #[test]
    fn a_huge_message_saturates_instead_of_overflowing() {
        let huge = "line\n".repeat(70_000);
        let d = dialog(&huge, Some("⚠️ warning"));
        assert_eq!(
            dialog_layout(&d, 60, 40).0,
            40,
            "clamped to the area, no panic"
        );
    }
}
