// P4: when the host re-renders at a different area, composite_snapshot
// asks the lookup for a buffer sized to the new tile.

use ainb::ui::bsp::{LayoutNode, LayoutSnapshot};
use ainb::ui::bsp_render::composite_snapshot;
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn solid(w: u16, h: u16, g: &str) -> WireBuffer {
    let mut b = WireBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            b.push(Coord::new(x, y), Cell::new(g));
        }
    }
    b
}

#[test]
fn composite_passes_current_tile_rect_to_lookup() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::pane("plugin"),
        min_cols: 1,
        min_rows: 1,
    };

    // first frame: 40x20
    let mut captured_rects = Vec::new();
    let area1 = Rect::new(0, 0, 40, 20);
    let mut target1 = Buffer::empty(area1);
    composite_snapshot(&mut target1, &snapshot, area1, |_id, tile| {
        captured_rects.push(tile);
        Some(solid(tile.width, tile.height, "1"))
    });

    // second frame: 60x30 (host resized the viewport)
    let area2 = Rect::new(0, 0, 60, 30);
    let mut target2 = Buffer::empty(area2);
    composite_snapshot(&mut target2, &snapshot, area2, |_id, tile| {
        captured_rects.push(tile);
        Some(solid(tile.width, tile.height, "2"))
    });

    assert_eq!(captured_rects.len(), 2);
    assert_eq!(captured_rects[0], area1);
    assert_eq!(captured_rects[1], area2);
    // and the painted buffer at frame 2 fills the full new area
    assert_eq!(target2.get(59, 29).symbol(), "2");
}
