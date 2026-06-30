//! P8.5 — the dual-dimension throughput sparkline widget.
//!
//! A normal sparkline encodes a single series as cell height. The daemon-health
//! pane needs **two** independent signals per second: how many tasks finished
//! (throughput) and how many of those failed (failure rate). This widget encodes
//! both at once, per the P8.5 UX research:
//!
//!   * **height** of a column = total throughput that second (`completed +
//!     failed`), scaled into the available rows;
//!   * **red proportion** of that column = the failure rate (`failed / total`):
//!     the bottom (success) portion is painted [`SPARK_GREEN`], the top (failure)
//!     portion [`SPARK_RED`].
//!
//! Both signals are recoverable independently from a rendered column: the total
//! height reads off the throughput, and the green/red split reads off the failure
//! rate — neither aliases the other. A second with no terminal tasks renders an
//! empty (zero-height) column.
//!
//! ## Geometry
//!
//! The widget paints one column per [`ThroughputSample`] across `[x0, x0 + n)`,
//! `n` columns wide and `rows` rows tall (`top ..= top + rows - 1`), bottom-
//! anchored. Heights scale linearly against the window's peak total so the tallest
//! second always reaches the full `rows` height (an all-zero window paints
//! nothing). Each filled cell uses a partial-block glyph for sub-row resolution at
//! the top of a column, the way ratatui's `Sparkline` does, so adjacent columns of
//! slightly different totals are visually distinguishable.

use ainb_hangar_proto::settings::ThroughputSample;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Success (bottom) portion accent — green.
pub const SPARK_GREEN: Color = Color::rgb(100, 200, 100);
/// Failure (top) portion accent — red.
pub const SPARK_RED: Color = Color::rgb(220, 120, 100);

/// The eight vertical partial-block glyphs (1/8 .. 8/8 of a cell), index `0..8`.
/// `BARS[7]` is the full block; `BARS[0]` is the one-eighth bar.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Render the dual-dim sparkline for `samples` into `buf`.
///
/// Paints one column per sample starting at column `x0`, `rows` rows tall with
/// its baseline on row `top + rows - 1` (bottom-anchored). The column height is
/// the sample's total throughput (`completed + failed`) scaled against the
/// window peak; within that height the failure fraction (`failed / total`) is
/// painted [`SPARK_RED`] from the top down and the remainder [`SPARK_GREEN`].
///
/// Returns nothing; an empty `samples` slice or a zero-throughput window paints
/// no cells (the caller draws an axis / label around it).
pub fn render_sparkline_dual(
    buf: &mut WireBuffer,
    x0: u16,
    top: u16,
    rows: u16,
    samples: &[ThroughputSample],
) {
    if rows == 0 || samples.is_empty() {
        return;
    }
    // Peak total across the window drives the linear height scale (the tallest
    // second reaches the full height). An all-zero window paints nothing.
    let peak_total = samples.iter().map(sample_total).max().unwrap_or(0);
    if peak_total == 0 {
        return;
    }

    // Eighth-resolution full height: `rows` cells × 8 sub-steps each.
    let full_eighths = u32::from(rows) * 8;
    for (i, sample) in samples.iter().enumerate() {
        let x = x0.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        render_column(buf, x, top, rows, full_eighths, peak_total, sample);
    }
}

/// One sample's total terminal-task count (`completed + failed`).
const fn sample_total(s: &ThroughputSample) -> u32 {
    s.completed.saturating_add(s.failed)
}

/// Render one bottom-anchored column for `sample`.
///
/// The total throughput sets the column height in eighth-of-a-cell units; the
/// failure fraction sets how many of those eighths (from the top) are painted
/// red. Each whole cell is a full block; the topmost partial cell uses the
/// matching [`BARS`] glyph. A cell's colour is red when its midpoint falls inside
/// the top failure band, else green.
fn render_column(
    buf: &mut WireBuffer,
    x: u16,
    top: u16,
    rows: u16,
    full_eighths: u32,
    peak_total: u32,
    sample: &ThroughputSample,
) {
    let total = sample_total(sample);
    if total == 0 {
        return;
    }
    // Height in eighths, scaled against the window peak (rounded up so any
    // non-zero second paints at least one eighth — never invisible). All operands
    // are small bounded counts, so the u64 widening cannot overflow.
    let height_eighths =
        u32::try_from((u64::from(total) * u64::from(full_eighths)).div_ceil(u64::from(peak_total)))
            .unwrap_or(full_eighths);
    let height_eighths = height_eighths.clamp(1, full_eighths);

    // Failure band: the top `failed/total` fraction of the column height, in
    // eighths (rounded so a single failure in a busy second still shows red).
    let fail_eighths = u32::try_from(
        (u64::from(sample.failed) * u64::from(height_eighths) + u64::from(total) / 2)
            / u64::from(total),
    )
    .unwrap_or(0);
    let success_eighths = height_eighths.saturating_sub(fail_eighths);

    let baseline = top + rows - 1;
    let full_cells = height_eighths / 8;
    let partial = height_eighths % 8;

    // Paint full cells from the baseline upward.
    for cell_idx in 0..full_cells {
        let row = baseline.saturating_sub(u16::try_from(cell_idx).unwrap_or(0));
        // Eighths spanned by this cell: [cell_idx*8, cell_idx*8 + 8); its midpoint
        // is `cell_idx*8 + 4`.
        let cell_mid_eighth = cell_idx * 8 + 4;
        let color = band_color(cell_mid_eighth, success_eighths);
        put_cell(buf, x, row, BARS[7], color);
    }
    // The topmost partial cell, if any: it spans `[full_cells*8, full_cells*8 +
    // partial)`, so its midpoint is `full_cells*8 + partial/2`.
    if partial > 0 {
        let row = baseline.saturating_sub(u16::try_from(full_cells).unwrap_or(0));
        let glyph = BARS[(partial - 1) as usize];
        let cell_mid_eighth = full_cells * 8 + partial / 2;
        let color = band_color(cell_mid_eighth, success_eighths);
        put_cell(buf, x, row, glyph, color);
    }
}

/// The colour of a cell whose *midpoint* sits at `cell_mid_eighth` eighths above
/// the baseline: green while the midpoint is inside the bottom success band
/// (`< success_eighths`), red once the midpoint is into the top failure band.
///
/// Colouring by midpoint (rather than top edge) means a cell that is *mostly*
/// success reads green and one that is *mostly* failure reads red — the visible
/// green/red split tracks the true `success/fail` proportion rather than always
/// rounding the boundary cell up to red.
const fn band_color(cell_mid_eighth: u32, success_eighths: u32) -> Color {
    if cell_mid_eighth < success_eighths {
        SPARK_GREEN
    } else {
        SPARK_RED
    }
}

/// Write a single coloured glyph at `(x, row)`.
fn put_cell(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, completed: u32, failed: u32) -> ThroughputSample {
        ThroughputSample {
            ts,
            completed,
            failed,
        }
    }

    /// A sample whose `completed` tracks its (small, in-range) index.
    fn at(i: u16, completed: u32, failed: u32) -> ThroughputSample {
        sample(i64::from(i), completed, failed)
    }

    /// Heights are monotone over a monotone-increasing completed series.
    #[test]
    fn heights_monotone_over_completed() {
        let samples: Vec<_> = (0..60u16).map(|i| at(i, u32::from(i), 0)).collect();
        let mut buf = WireBuffer::new(60, 8);
        render_sparkline_dual(&mut buf, 0, 0, 8, &samples);

        // Column height = number of painted cells in that x column.
        let height_at = |x: u16| buf.cells.iter().filter(|(c, _)| c.x == x).count();
        let mut prev = 0;
        for x in 1..60u16 {
            let h = height_at(x);
            assert!(h >= prev, "height must be monotone at x={x}: {h} < {prev}");
            prev = h;
        }
        // The tallest (last) column reaches near the full 8 rows.
        assert!(height_at(59) >= 7, "peak column should fill the height");
    }

    /// A failure spike at index 30 paints red in that column; a no-failure column
    /// paints only green.
    #[test]
    fn failure_spike_paints_red() {
        let mut samples: Vec<_> = (0..60u16).map(|i| at(i, 10, 0)).collect();
        samples[30] = sample(30, 5, 5); // 50% failure spike.
        let mut buf = WireBuffer::new(60, 8);
        render_sparkline_dual(&mut buf, 0, 0, 8, &samples);

        let has_red_at = |x: u16| {
            buf.cells
                .iter()
                .any(|(c, cell)| c.x == x && cell.fg == Some(SPARK_RED))
        };
        let has_green_at = |x: u16| {
            buf.cells
                .iter()
                .any(|(c, cell)| c.x == x && cell.fg == Some(SPARK_GREEN))
        };
        assert!(has_red_at(30), "the failure-spike column must carry red");
        assert!(
            !has_red_at(10),
            "a no-failure column must carry no red cell"
        );
        assert!(has_green_at(10), "a success column must carry green");
    }

    /// A 50/50 column splits green (bottom) + red (top): both dims are visible
    /// and the success band is anchored at the baseline (lower rows green, upper
    /// rows red) — the dual-dim encoding is verifiable on a single column.
    #[test]
    fn half_failure_column_splits_green_bottom_red_top() {
        // One tall column, 50% failure, alone so it sets its own peak.
        let samples = vec![sample(0, 10, 10)];
        let mut buf = WireBuffer::new(1, 8);
        render_sparkline_dual(&mut buf, 0, 0, 8, &samples);

        // Collect (row, color) for column 0. Lower screen rows = nearer baseline.
        let mut greens: Vec<u16> = buf
            .cells
            .iter()
            .filter(|(c, cell)| c.x == 0 && cell.fg == Some(SPARK_GREEN))
            .map(|(c, _)| c.y)
            .collect();
        let mut reds: Vec<u16> = buf
            .cells
            .iter()
            .filter(|(c, cell)| c.x == 0 && cell.fg == Some(SPARK_RED))
            .map(|(c, _)| c.y)
            .collect();
        assert!(!greens.is_empty(), "50% column must carry green (success)");
        assert!(!reds.is_empty(), "50% column must carry red (failure)");
        greens.sort_unstable();
        reds.sort_unstable();
        // Green sits below red: the largest green row index (nearer the baseline,
        // i.e. higher y) exceeds the largest red row index.
        assert!(
            greens.iter().max() > reds.iter().max(),
            "the success (green) band must be anchored at the baseline, below red"
        );
    }
}
