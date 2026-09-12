//! Canonical, crate-independent screen dump.
//!
//! Every emulator backend lowers its own grid into `Canon`, and `Canon::render`
//! produces the text that the feed-vs-pty comparison diffs. Trailing default
//! blank cells are dropped ("ANSI normalisation") so that a crate that pads a
//! row to the full width and one that stops at the last written cell compare
//! equal.

use std::fmt::Write as _;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Cell {
    /// Grapheme content. Empty means an unwritten / blank cell.
    pub text: String,
    /// Column count this cell occupies (1 or 2).
    pub width: u8,
    /// This cell is the trailing half of a double-width grapheme.
    pub spacer: bool,
    pub fg: String,
    pub bg: String,
    /// Sorted, comma-free attribute tags, e.g. "b" "b,u" "rev".
    pub attrs: String,
    /// OSC 8 target, empty when unset or unsupported by the backend.
    pub link: String,
}

impl Cell {
    fn is_default(&self) -> bool {
        (self.text.is_empty() || self.text == " ")
            && !self.spacer
            && self.fg == "-"
            && self.bg == "-"
            && self.attrs.is_empty()
            && self.link.is_empty()
    }

    fn style_key(&self) -> String {
        format!("fg={} bg={} at={} link={}", self.fg, self.bg, self.attrs, self.link)
    }

    fn style_is_default(&self) -> bool {
        self.fg == "-" && self.bg == "-" && self.attrs.is_empty() && self.link.is_empty()
    }
}

#[derive(Default)]
pub struct Canon {
    pub cols: usize,
    pub rows: usize,
    pub alt: bool,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,
    pub title: String,
    /// Backend's view of the mouse reporting mode, or "n/a" when not exposed.
    pub mouse: String,
    pub grid: Vec<Vec<Cell>>,
}

impl Canon {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "size {}x{}", self.cols, self.rows);
        let _ = writeln!(out, "alt {}", self.alt as u8);
        let _ = writeln!(
            out,
            "cursor r={} c={} vis={}",
            self.cursor_row, self.cursor_col, self.cursor_visible as u8
        );
        let _ = writeln!(out, "title {:?}", self.title);
        let _ = writeln!(out, "mouse {}", self.mouse);
        for (r, row) in self.grid.iter().enumerate() {
            let mut last = row.len();
            while last > 0 && row[last - 1].is_default() {
                last -= 1;
            }
            let live = &row[..last];
            let mut text = String::new();
            for c in live {
                if c.spacer {
                    continue;
                }
                if c.text.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(&c.text);
                }
            }
            let _ = writeln!(out, "r{:03} t |{}|", r, text);
            // Style runs, emitted only where a run differs from the default style.
            let mut i = 0usize;
            while i < live.len() {
                let key = live[i].style_key();
                let mut j = i + 1;
                while j < live.len() && live[j].style_key() == key {
                    j += 1;
                }
                if !live[i].style_is_default() {
                    let _ = writeln!(out, "r{:03} a {}..{} {}", r, i, j - 1, key);
                }
                i = j;
            }
            // Double-width cells are load-bearing for layout, so record them.
            let wides: Vec<String> = live
                .iter()
                .enumerate()
                .filter(|(_, c)| c.width == 2 && !c.spacer)
                .map(|(i, _)| i.to_string())
                .collect();
            if !wides.is_empty() {
                let _ = writeln!(out, "r{:03} w {}", r, wides.join(","));
            }
        }
        out
    }
}
