//! Composite a BSP tree of plugin tiles into a single ratatui [`Buffer`].
//!
//! `composite_snapshot` walks the [`LayoutSnapshot`] tree, asks the caller
//! for each leaf's [`WireBuffer`] via a `lookup` callback, and paints the
//! buffer into the leaf's tile rect using [`paint_wire_buffer`]. Leaves
//! whose lookup returns `None` are left untouched — the caller can pre-fill
//! them with a "loading…" placeholder or skip them.
//!
//! This is the pure-function compositing primitive. The runtime side
//! (plugin_runtime's `last_rendered` lookup + `request_render` push) is
//! wired in a follow-up bead; this module's contract is "given a tree
//! and a lookup, paint the buffer".

use ainb_plugin_protocol::wire_buffer::WireBuffer;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::ui::bsp::{LayoutSnapshot, LeafId};
use crate::ui::wire_paint::paint_wire_buffer;

/// Walk `snapshot.root` over `area`, and for each leaf call `lookup(leaf_id,
/// tile_rect)`. If lookup returns `Some(buffer)`, paint that buffer into
/// the tile rect.
///
/// Returns the count of leaves successfully painted (i.e. lookup returned
/// `Some`). Useful for caller-side render-budget metrics and for tests.
pub fn composite_snapshot<F>(
    target: &mut Buffer,
    snapshot: &LayoutSnapshot,
    area: Rect,
    mut lookup: F,
) -> usize
where
    F: FnMut(&LeafId, Rect) -> Option<WireBuffer>,
{
    let mut painted = 0;
    for (leaf_id, tile) in snapshot.root.walk(area) {
        if let Some(buf) = lookup(&leaf_id, tile) {
            paint_wire_buffer(target, &buf, tile);
            painted += 1;
        }
    }
    painted
}

/// Like [`composite_snapshot`], but enforces a minimum tile size via
/// [`LayoutSnapshot::min_cols`] / `min_rows`. Returns `Err(painted_count)`
/// when any tile would be smaller than the snapshot's minimum.
pub fn composite_snapshot_checked<F>(
    target: &mut Buffer,
    snapshot: &LayoutSnapshot,
    area: Rect,
    lookup: F,
) -> Result<usize, crate::ui::bsp::BspError>
where
    F: FnMut(&LeafId, Rect) -> Option<WireBuffer>,
{
    // First validate via walk_checked; if it errors, return immediately.
    snapshot.root.walk_checked(area, snapshot.min_cols, snapshot.min_rows)?;
    Ok(composite_snapshot(target, snapshot, area, lookup))
}
