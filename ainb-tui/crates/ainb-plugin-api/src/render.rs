//! Wire-format render types.
//!
//! ratatui's `Buffer` doesn't derive `Serialize`, so plugins build a
//! [`WireBuffer`] (msgpack-friendly) and the host converts it to a real ratatui
//! `Buffer` before painting. This decouples the plugin ABI from the host's
//! ratatui version and keeps `ainb-plugin-api` ratatui-free so it builds cleanly
//! on `wasm32-wasip1`.

use serde::{Deserialize, Serialize};

/// Where the plugin is being asked to paint.
///
/// Wire-encoded as a `u8` so it can also travel through the `ainb_render_buffer`
/// host-fn as an `i32` arg without a serde dance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum RenderTarget {
    /// Full-screen owned by a plugin-defined screen.
    Screen = 0,
    /// Sidebar widget slot.
    Sidebar = 1,
    /// Statusline segment.
    Statusline = 2,
}

impl RenderTarget {
    #[must_use]
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Screen),
            1 => Some(Self::Sidebar),
            2 => Some(Self::Statusline),
            _ => None,
        }
    }
}

/// Sub-rect handed to the plugin to paint. Mirrors `ratatui::layout::Rect` but
/// is `repr(C)` and Serializable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    #[must_use]
    pub fn area(&self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }
}

/// One painted cell. Colours are 8-bit ANSI indices to keep wire size small;
/// the host can up-sample to truecolor before drawing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCell {
    /// Single grapheme. Stored as `String` to handle multi-byte glyphs.
    pub symbol: String,
    /// 8-bit ANSI fg index, 0xFF = inherit.
    pub fg: u8,
    /// 8-bit ANSI bg index, 0xFF = inherit.
    pub bg: u8,
    /// Bitset of bold(1) / dim(2) / italic(4) / underline(8) / reversed(16).
    pub modifiers: u8,
}

impl Default for WireCell {
    fn default() -> Self {
        Self {
            symbol: " ".to_string(),
            fg: 0xFF,
            bg: 0xFF,
            modifiers: 0,
        }
    }
}

/// Plugin-painted buffer for one render target.
///
/// `cells` is row-major, length must equal `width * height`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireBuffer {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<WireCell>,
}

impl WireBuffer {
    /// Build an empty buffer of the requested size, filled with default cells.
    #[must_use]
    pub fn empty(width: u16, height: u16) -> Self {
        let n = usize::from(width) * usize::from(height);
        Self {
            width,
            height,
            cells: vec![WireCell::default(); n],
        }
    }

    /// True iff `cells.len() == width * height`.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.cells.len() == usize::from(self.width) * usize::from(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_target_roundtrip() {
        for &t in &[RenderTarget::Screen, RenderTarget::Sidebar, RenderTarget::Statusline] {
            assert_eq!(RenderTarget::from_i32(t as i32), Some(t));
        }
        assert_eq!(RenderTarget::from_i32(99), None);
    }

    #[test]
    fn empty_buffer_is_consistent() {
        let b = WireBuffer::empty(80, 24);
        assert!(b.is_consistent());
        assert_eq!(b.cells.len(), 80 * 24);
    }

    #[test]
    fn buffer_msgpack_roundtrip() {
        let mut b = WireBuffer::empty(2, 1);
        b.cells[0] = WireCell {
            symbol: "█".to_string(),
            fg: 12,
            bg: 0,
            modifiers: 1,
        };
        let bytes = rmp_serde::to_vec_named(&b).unwrap();
        let back: WireBuffer = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn rect_area_no_overflow_at_max_dims() {
        let r = Rect { x: 0, y: 0, width: u16::MAX, height: u16::MAX };
        // u32 area must not overflow.
        assert!(r.area() > 0);
    }
}
