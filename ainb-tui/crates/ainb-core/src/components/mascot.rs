// ABOUTME: Animated mascot component "Boxy" for the AINB home screen

pub use ainb_app::components::mascot::*;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const SHADOW_GRAY: Color = Color::Rgb(60, 60, 80);

/// ASCII art frames for the Boxy mascot
/// Each frame is a vector of string slices representing lines

/// Render the mascot with proper coloring
pub fn render_mascot(frame: &mut Frame, area: Rect, mascot: &MascotAnimation) {
    let mascot_lines = mascot.get_current_frame();

    let styled_lines: Vec<Line> = mascot_lines
        .iter()
        .map(|line| {
            let spans: Vec<Span> = line
                .chars()
                .map(|ch| {
                    let style = match ch {
                        // Eyes - golden when open
                        '◉' => Style::default().fg(GOLD),
                        // Closed eyes and mouth
                        '─' | '◡' => Style::default().fg(SOFT_WHITE),
                        // Shadow underneath
                        '░' => Style::default().fg(SHADOW_GRAY),
                        // Box outline - cornflower blue
                        '╭' | '╮' | '╯' | '╰' | '│' | '╱' | '╲' => {
                            Style::default().fg(CORNFLOWER_BLUE)
                        }
                        // Everything else
                        _ => Style::default().fg(SOFT_WHITE),
                    };
                    Span::styled(ch.to_string(), style)
                })
                .collect();

            Line::from(spans)
        })
        .collect();

    let mascot_widget = Paragraph::new(styled_lines).alignment(Alignment::Left);

    frame.render_widget(mascot_widget, area);
}

/// Render the mascot centered in the given area
pub fn render_mascot_centered(frame: &mut Frame, area: Rect, mascot: &MascotAnimation) {
    let mascot_width = mascot.width();
    let mascot_height = mascot.height();

    // Calculate centered position
    let x = area.x + (area.width.saturating_sub(mascot_width)) / 2;
    let y = area.y + (area.height.saturating_sub(mascot_height)) / 2;

    let centered_area = Rect {
        x,
        y,
        width: mascot_width.min(area.width),
        height: mascot_height.min(area.height),
    };

    render_mascot(frame, centered_area, mascot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mascot_animation_creation() {
        let mascot = MascotAnimation::new();
        assert_eq!(mascot.current_frame, MascotFrame::Neutral);
        assert!(!mascot.is_mini);
    }

    #[test]
    fn test_mascot_dimensions() {
        let mut mascot = MascotAnimation::new();

        // Full size
        assert_eq!(mascot.width(), 20);
        assert_eq!(mascot.height(), 8);

        // Mini size
        mascot.set_mini(true);
        assert_eq!(mascot.width(), 6);
        assert_eq!(mascot.height(), 3);
    }

    #[test]
    fn test_trigger_happy() {
        let mut mascot = MascotAnimation::new();
        mascot.trigger_happy();
        assert_eq!(mascot.current_frame, MascotFrame::Happy);
    }

    #[test]
    fn test_frame_content() {
        let mascot = MascotAnimation::new();
        let frame = mascot.get_current_frame();
        assert_eq!(frame.len(), 8);
        assert!(frame[2].contains('◉')); // Eyes in neutral frame
    }
}
