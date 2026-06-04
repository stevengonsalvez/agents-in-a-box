//! Test-only helpers for assertions over a [`WireBuffer`].

use ainb_plugin_sdk::{Cell, Coord, WireBuffer};

/// Pull all painted cells onto a row-by-row grid, collapse each row
/// into a single line, and `trim_end()` each one so test assertions
/// can `contains(...)` the painted text without juggling coordinates
/// or padding.
///
/// Returns rows from top (`y = 0`) to bottom; the last entry is row
/// `height - 1`. Empty `x` slots are filled with a single space then
/// trimmed from the right.
///
/// **Caveat**: A row that was *deliberately* painted with a trailing
/// `Cell::new(" ")` will look identical to an un-painted row tail
/// because of the `trim_end`. Empty-state painting in cfx.4 never
/// emits trailing-space cells, so this is invisible; cfx.5 painters
/// that draw spaced-out tab bars must NOT use this helper for
/// per-row width assertions.
pub(crate) fn flatten_rows_trimmed(buf: &WireBuffer) -> Vec<String> {
    let height = buf.height as usize;
    let width = buf.width as usize;
    let mut grid: Vec<Vec<String>> = vec![vec![String::from(" "); width]; height];

    for (coord, cell) in &buf.cells {
        let Coord { x, y } = *coord;
        if (y as usize) < height && (x as usize) < width {
            grid[y as usize][x as usize] = cell.symbol.clone();
        }
    }

    grid.into_iter().map(|row| row.join("").trim_end().to_string()).collect()
}

/// Convenience: does any row of the buffer contain `needle`?
pub(crate) fn buffer_contains(buf: &WireBuffer, needle: &str) -> bool {
    flatten_rows_trimmed(buf).iter().any(|line| line.contains(needle))
}

/// Convenience: total non-blank cell count. Useful for "did we paint
/// anything?" sanity checks without coupling to exact positions.
pub(crate) fn painted_cell_count(buf: &WireBuffer) -> usize {
    buf.cells
        .iter()
        .filter(|(_, c): &&(Coord, Cell)| !c.symbol.trim().is_empty())
        .count()
}
