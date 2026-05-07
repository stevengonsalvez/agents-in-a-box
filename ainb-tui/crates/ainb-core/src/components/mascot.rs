// ABOUTME: Animated mascot component "Boxy" for the AINB home screen

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::time::{Duration, Instant};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const SHADOW_GRAY: Color = Color::Rgb(60, 60, 80);

/// ASCII art frames for the Boxy mascot
/// Each frame is a vector of string slices representing lines

/// Neutral expression - default state
const MASCOT_FRAME_NEUTRAL: &[&str] = &[
    "    ╭──────────╮    ",
    "   ╱│          │╲   ",
    "  ╱ │  ◉    ◉  │ ╲  ",
    " ╱  │    ──    │  ╲ ",
    "│   │          │   │",
    "│   ╰──────────╯   │",
    "╰──────────────────╯",
    "      ░░░░░░░░      ",
];

/// Blinking eyes
const MASCOT_FRAME_BLINK: &[&str] = &[
    "    ╭──────────╮    ",
    "   ╱│          │╲   ",
    "  ╱ │  ─    ─  │ ╲  ",
    " ╱  │    ──    │  ╲ ",
    "│   │          │   │",
    "│   ╰──────────╯   │",
    "╰──────────────────╯",
    "      ░░░░░░░░      ",
];

/// Bounce up position (shifted up by one line)
const MASCOT_FRAME_BOUNCE: &[&str] = &[
    "    ╭──────────╮    ",
    "   ╱│          │╲   ",
    "  ╱ │  ◉    ◉  │ ╲  ",
    " ╱  │    ──    │  ╲ ",
    "│   │          │   │",
    "│   ╰──────────╯   │",
    "╰──────────────────╯",
    "       ░░░░░░       ",
];

/// Happy expression - smile
const MASCOT_FRAME_HAPPY: &[&str] = &[
    "    ╭──────────╮    ",
    "   ╱│          │╲   ",
    "  ╱ │  ◉    ◉  │ ╲  ",
    " ╱  │    ◡◡    │  ╲ ",
    "│   │          │   │",
    "│   ╰──────────╯   │",
    "╰──────────────────╯",
    "      ░░░░░░░░      ",
];

/// Compact mascot for smaller screens (3 lines)
const MASCOT_MINI: &[&str] = &[
    "╭─◉◉─╮",
    "│ ── │",
    "╰────╯",
];

/// Animation frame types
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MascotFrame {
    Neutral,
    Blink,
    Bounce,
    Happy,
}

/// Mascot animation controller
#[derive(Clone, Debug)]
pub struct MascotAnimation {
    current_frame: MascotFrame,
    last_update: Instant,
    frame_duration: Duration,
    blink_timer: Instant,
    blink_interval: Duration,
    is_mini: bool,
}

impl MascotAnimation {
    pub fn new() -> Self {
        Self {
            current_frame: MascotFrame::Neutral,
            last_update: Instant::now(),
            frame_duration: Duration::from_millis(100),
            blink_timer: Instant::now(),
            blink_interval: Duration::from_secs(4),
            is_mini: false,
        }
    }

    /// Set whether to use mini mascot (for compact layouts)
    pub fn set_mini(&mut self, mini: bool) {
        self.is_mini = mini;
    }

    /// Update animation state - call this in the main event loop
    pub fn tick(&mut self) {
        let now = Instant::now();

        // Check if we should blink
        if now.duration_since(self.blink_timer) > self.blink_interval {
            self.current_frame = MascotFrame::Blink;
            self.last_update = now;
            self.blink_timer = now;
            // Randomize next blink interval (3-6 seconds)
            self.blink_interval = Duration::from_millis(3000 + (now.elapsed().as_millis() % 3000) as u64);
        } else if now.duration_since(self.last_update) > self.frame_duration {
            // Return to neutral after blink
            if self.current_frame == MascotFrame::Blink {
                self.current_frame = MascotFrame::Neutral;
            }
            self.last_update = now;
        }
    }

    /// Trigger a happy expression (e.g., on successful action)
    pub fn trigger_happy(&mut self) {
        self.current_frame = MascotFrame::Happy;
        self.last_update = Instant::now();
    }

    /// Trigger a bounce animation
    pub fn trigger_bounce(&mut self) {
        self.current_frame = MascotFrame::Bounce;
        self.last_update = Instant::now();
    }

    /// Get the current frame's ASCII art lines
    pub fn get_current_frame(&self) -> &'static [&'static str] {
        if self.is_mini {
            return MASCOT_MINI;
        }

        match self.current_frame {
            MascotFrame::Neutral => MASCOT_FRAME_NEUTRAL,
            MascotFrame::Blink => MASCOT_FRAME_BLINK,
            MascotFrame::Bounce => MASCOT_FRAME_BOUNCE,
            MascotFrame::Happy => MASCOT_FRAME_HAPPY,
        }
    }

    /// Get the height of the current mascot (for layout calculations)
    pub fn height(&self) -> u16 {
        if self.is_mini { 3 } else { 8 }
    }

    /// Get the width of the current mascot
    pub fn width(&self) -> u16 {
        if self.is_mini { 6 } else { 20 }
    }
}

impl Default for MascotAnimation {
    fn default() -> Self {
        Self::new()
    }
}

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
