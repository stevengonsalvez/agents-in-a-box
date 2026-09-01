// ABOUTME: Confirmation dialog component for displaying yes/no prompts with keyboard navigation

use crate::app::state::{AppState, ConfirmationDialog};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub struct ConfirmationDialogComponent;

impl ConfirmationDialogComponent {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(dialog) = &state.confirmation_dialog {
            // Below this there is no room for borders, a message and buttons,
            // and the arithmetic below would underflow.
            if area.width < 10 || area.height < 6 {
                return;
            }
            // Calculate dialog size (center it)
            let dialog_width = 60.min(area.width - 4);
            let warning_rows = warning_row_count(dialog, dialog_width);
            let dialog_height = dialog_height(dialog, dialog_width, area.height);

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
            let constraints = if dialog.warning.is_some() {
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
            let (message_chunk, button_chunk) = if let Some(warning_text) = &dialog.warning {
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
fn dialog_height(dialog: &ConfirmationDialog, dialog_width: u16, max_height: u16) -> u16 {
    let message_rows = wrapped_line_count(&dialog.message, dialog_width.saturating_sub(2));
    let base = if dialog.warning.is_some() { 11 } else { 8 };
    // 2 borders + warning block + message + 2 button rows.
    let needed = 4 + warning_row_count(dialog, dialog_width) + message_rows;
    base.max(needed).min(max_height)
}

/// Rows `text` needs at `width`, emulating the greedy word wrap ratatui applies
/// with `Wrap { trim: true }` (which never splits a word that fits on a line).
///
/// ponytail: counts chars, not display columns, so a run of double-width (CJK)
/// glyphs is under-counted; the extra row added per paragraph absorbs that and
/// the odd wide emoji. Measure display width here if that stops being enough.
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
        assert_eq!(dialog_height(&dialog("Are you sure?", None), 60, 40), 8);
        assert_eq!(
            dialog_height(
                &dialog("Are you sure?", Some("⚠️ 1 uncommitted file(s)")),
                60,
                40
            ),
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
        let height = dialog_height(&d, 60, 40);
        let warning_rows = warning_row_count(&d, 60);
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
        for height in 6..=12u16 {
            let computed = dialog_height(&d, 60, height);
            assert!(
                computed >= 2,
                "height {computed} would underflow inner_area"
            );
            assert!(computed <= height, "dialog must fit the area");
        }
    }
}
