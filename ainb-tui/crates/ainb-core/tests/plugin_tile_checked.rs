// P4: composite_snapshot_checked refuses to paint when any tile drops
// below the snapshot's declared minimum size (BspError::TileTooSmall).

use ainb::ui::bsp::{BspError, LayoutNode, LayoutSnapshot};
use ainb::ui::bsp_render::composite_snapshot_checked;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[test]
fn checked_passes_when_all_tiles_meet_minimum() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::split_h(0.4, LayoutNode::pane("a"), LayoutNode::pane("b")),
        min_cols: 30,
        min_rows: 10,
    };
    let area = Rect::new(0, 0, 100, 40);
    let mut target = Buffer::empty(area);

    let result = composite_snapshot_checked(&mut target, &snapshot, area, |_, _| None);
    assert!(result.is_ok());
}

#[test]
fn checked_errors_when_tile_below_minimum_width() {
    // 0.2 of width=100 = 20-wide tile; min_cols=30 → fails
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::split_h(0.2, LayoutNode::pane("a"), LayoutNode::pane("b")),
        min_cols: 30,
        min_rows: 10,
    };
    let area = Rect::new(0, 0, 100, 40);
    let mut target = Buffer::empty(area);

    let result = composite_snapshot_checked(&mut target, &snapshot, area, |_, _| None);
    assert!(matches!(result, Err(BspError::TileTooSmall { .. })));
}
