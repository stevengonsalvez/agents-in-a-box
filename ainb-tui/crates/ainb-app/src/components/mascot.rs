// ABOUTME: Renderer-agnostic half of the `mascot` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::mascot`, which re-exports this module.

use std::time::{Duration, Instant};

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
const MASCOT_MINI: &[&str] = &["╭─◉◉─╮", "│ ── │", "╰────╯"];

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
    pub current_frame: MascotFrame,
    last_update: Instant,
    frame_duration: Duration,
    blink_timer: Instant,
    blink_interval: Duration,
    pub is_mini: bool,
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
            self.blink_interval =
                Duration::from_millis(3000 + (now.elapsed().as_millis() % 3000) as u64);
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
