//! Cell-based render output transported over the wire.
//!
//! `WireBuffer` is a typed, sparse cell grid the plugin produces in
//! response to `plugin/render`. The host paints each cell into its
//! ratatui buffer at the (`x`,`y`) coordinate. Sparse — cells the
//! plugin doesn't write are left untouched on the host side, which
//! lets layered overlays (modal over a screen) compose without the
//! plugin re-emitting the whole grid.
//!
//! ## Determinism
//!
//! `cells` is `Vec<(Coord, Cell)>`, **not** a ``HashMap``. Vec
//! preserves insertion order, so the same logical render produces
//! byte-identical JSON every time. Required for the byte-equality
//! tripwire (CTS axis A5) — see
//! `reference_msgpack_byte_determinism_vec_over_hashmap` in shared
//! memory: `HashMap` iteration is randomised by default, breaking
//! golden-byte assertions.

use serde::{Deserialize, Serialize};

/// Cell position inside a [`WireBuffer`]. Origin is top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Coord {
    /// Column (0-based, left to right).
    pub x: u16,
    /// Row (0-based, top to bottom).
    pub y: u16,
}

impl Coord {
    /// Construct a coordinate at (`x`, `y`).
    #[must_use]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// 24-bit RGB colour. We don't carry palette indices over the wire —
/// the host downgrades to 256-colour or 16-colour as the terminal
/// allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    /// Red channel (0..=255).
    pub r: u8,
    /// Green channel (0..=255).
    pub g: u8,
    /// Blue channel (0..=255).
    pub b: u8,
}

impl Color {
    /// Construct an RGB colour.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// One painted cell. The `modifier` field is a bitfield of style
/// flags (bold, italic, ...) — host-side mapping lives in the
/// runtime crate, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Visible glyph(s) in this cell. May be a multi-codepoint
    /// grapheme cluster; host re-measures width on paint.
    pub symbol: String,
    /// Foreground colour, or `None` for terminal default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    /// Background colour, or `None` for terminal default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    /// Style modifier bitfield. Bit semantics are stable between
    /// SDK and runtime; canonical mapping lives in the runtime
    /// crate's ratatui interop layer.
    #[serde(default)]
    pub modifier: u16,
}

impl Cell {
    /// Construct a cell with the given symbol and default attributes.
    #[must_use]
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            fg: None,
            bg: None,
            modifier: 0,
        }
    }
}

/// Sparse cell-grid render output.
///
/// `width` and `height` describe the viewport the plugin rendered
/// for. Cells outside `0..width × 0..height` are technically legal
/// but the host clamps them silently; emit them only at your own
/// risk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireBuffer {
    /// Viewport width in cells.
    pub width: u16,
    /// Viewport height in cells.
    pub height: u16,
    /// Painted cells. Order preserved for byte-determinism — see
    /// the module-level note.
    pub cells: Vec<(Coord, Cell)>,
}

impl WireBuffer {
    /// Create an empty buffer for the given viewport.
    #[must_use]
    pub const fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: Vec::new(),
        }
    }

    /// Append a painted cell. Does not deduplicate; later writes at
    /// the same coordinate win when the host paints in order.
    pub fn push(&mut self, coord: Coord, cell: Cell) {
        self.cells.push((coord, cell));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> WireBuffer {
        let mut b = WireBuffer::new(10, 3);
        b.push(
            Coord::new(0, 0),
            Cell {
                symbol: "T".into(),
                fg: Some(Color::rgb(255, 215, 0)),
                bg: None,
                modifier: 0b0000_0001,
            },
        );
        b.push(Coord::new(1, 0), Cell::new("E"));
        b.push(Coord::new(2, 0), Cell::new("S"));
        b.push(Coord::new(3, 0), Cell::new("T"));
        b
    }

    #[test]
    fn round_trip() {
        let b = fixture();
        let s = serde_json::to_string(&b).unwrap();
        let back: WireBuffer = serde_json::from_str(&s).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn byte_stable() {
        let a = serde_json::to_vec(&fixture()).unwrap();
        let b = serde_json::to_vec(&fixture()).unwrap();
        assert_eq!(
            a, b,
            "WireBuffer JSON must be byte-stable across serialisations \
             (driven by Vec ordering, not `HashMap` iteration)"
        );
    }

    #[test]
    fn empty_default_attrs_are_omitted() {
        let cell = Cell::new("X");
        let s = serde_json::to_string(&cell).unwrap();
        // fg and bg are None and skipped; modifier=0 still serialises.
        assert!(!s.contains("\"fg\""));
        assert!(!s.contains("\"bg\""));
        assert!(s.contains("\"symbol\":\"X\""));
    }

    #[test]
    fn cells_ordering_preserved() {
        // Build the same logical content twice in different push
        // orders — confirm bytes differ. This proves the ordering
        // is structural, not a side-effect of hash randomisation.
        let mut a = WireBuffer::new(4, 1);
        a.push(Coord::new(0, 0), Cell::new("A"));
        a.push(Coord::new(1, 0), Cell::new("B"));

        let mut b = WireBuffer::new(4, 1);
        b.push(Coord::new(1, 0), Cell::new("B"));
        b.push(Coord::new(0, 0), Cell::new("A"));

        let a_bytes = serde_json::to_vec(&a).unwrap();
        let b_bytes = serde_json::to_vec(&b).unwrap();
        assert_ne!(a_bytes, b_bytes);
    }
}
