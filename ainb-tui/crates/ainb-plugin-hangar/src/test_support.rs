//! Helpers shared by the crate's RENDER tests.
//!
//! A [`WireBuffer`] is a sparse `(Coord, Cell)` list in paint order, not a grid,
//! so every render test needs the same reconstruction before it can assert on
//! text. That reconstruction had drifted into five identical private copies
//! (crisp B1 review); it lives here once instead.

use ainb_plugin_sdk::WireBuffer;

/// The full painted text of `buf` in ROW-MAJOR order, so an assertion can search
/// for a label wherever on the screen it landed.
///
/// Rows are concatenated with no separator: this answers "is this text on the
/// screen", not "which line is it on". A test that must pin text to a LINE
/// collects that row itself (`row_text`) rather than searching this.
pub fn painted_text(buf: &WireBuffer) -> String {
    let mut out = String::new();
    for y in 0..buf.height {
        for (coord, cell) in &buf.cells {
            if coord.y == y {
                out.push_str(&cell.symbol);
            }
        }
    }
    out
}
