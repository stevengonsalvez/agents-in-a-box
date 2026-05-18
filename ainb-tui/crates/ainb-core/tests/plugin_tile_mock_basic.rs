// P4 minimum cut: composite_snapshot walks a BSP tree and paints each
// leaf's WireBuffer (via a caller-supplied lookup) into the leaf's tile.
//
// Mock plugins are simulated inline (no MockPlugin builder needed for the
// composition primitive). The L3 plugin_runtime test bed lives in a
// follow-up bead.

use ainb::ui::bsp::{LayoutNode, LayoutSnapshot};
use ainb::ui::bsp_render::composite_snapshot;
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn solid_buffer(w: u16, h: u16, glyph: &str) -> WireBuffer {
    let mut buf = WireBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            buf.push(Coord::new(x, y), Cell::new(glyph));
        }
    }
    buf
}

#[test]
fn composite_paints_one_pane_full_area() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::pane("only"),
        min_cols: 30,
        min_rows: 10,
    };
    let area = Rect::new(0, 0, 30, 10);
    let mut target = Buffer::empty(area);

    let painted = composite_snapshot(&mut target, &snapshot, area, |_id, tile| {
        Some(solid_buffer(tile.width, tile.height, "A"))
    });

    assert_eq!(painted, 1);
    assert_eq!(target.get(0, 0).symbol(), "A");
    assert_eq!(target.get(29, 9).symbol(), "A");
}

#[test]
fn composite_paints_two_panes_at_correct_offsets() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::split_h(0.4, LayoutNode::pane("a"), LayoutNode::pane("b")),
        min_cols: 1,
        min_rows: 1,
    };
    let area = Rect::new(0, 0, 100, 40);
    let mut target = Buffer::empty(area);

    composite_snapshot(&mut target, &snapshot, area, |id, tile| {
        let glyph = if id == "a" { "L" } else { "R" };
        Some(solid_buffer(tile.width, tile.height, glyph))
    });

    // left tile = 40 wide → cells [0..40] painted "L"
    assert_eq!(target.get(0, 0).symbol(), "L");
    assert_eq!(target.get(39, 0).symbol(), "L");
    // right tile = 60 wide → cells [40..100] painted "R"
    assert_eq!(target.get(40, 0).symbol(), "R");
    assert_eq!(target.get(99, 0).symbol(), "R");
}

#[test]
fn composite_skips_leaves_with_no_buffer() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::split_h(0.5, LayoutNode::pane("ready"), LayoutNode::pane("pending")),
        min_cols: 1,
        min_rows: 1,
    };
    let area = Rect::new(0, 0, 20, 5);
    let mut target = Buffer::empty(area);
    // pre-paint the pending tile with a placeholder
    for x in 10..20u16 {
        target.get_mut(x, 0).set_symbol("·");
    }

    let painted = composite_snapshot(&mut target, &snapshot, area, |id, tile| {
        if id == "ready" {
            Some(solid_buffer(tile.width, tile.height, "R"))
        } else {
            None
        }
    });

    assert_eq!(painted, 1, "only the ready leaf was painted");
    assert_eq!(target.get(0, 0).symbol(), "R");
    assert_eq!(target.get(10, 0).symbol(), "·", "pending tile placeholder preserved");
}

#[test]
fn composite_three_deep_tree_paints_each_leaf_in_its_tile() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::split_h(
            0.5,
            LayoutNode::pane("left"),
            LayoutNode::split_v(
                0.5,
                LayoutNode::pane("top-right"),
                LayoutNode::pane("bottom-right"),
            ),
        ),
        min_cols: 1,
        min_rows: 1,
    };
    let area = Rect::new(0, 0, 100, 40);
    let mut target = Buffer::empty(area);

    composite_snapshot(&mut target, &snapshot, area, |id, tile| {
        let glyph = match id.as_str() {
            "left" => "L",
            "top-right" => "T",
            "bottom-right" => "B",
            _ => "?",
        };
        Some(solid_buffer(tile.width, tile.height, glyph))
    });

    // left tile fills (0..50, 0..40)
    assert_eq!(target.get(0, 0).symbol(), "L");
    assert_eq!(target.get(49, 39).symbol(), "L");
    // top-right tile fills (50..100, 0..20)
    assert_eq!(target.get(50, 0).symbol(), "T");
    assert_eq!(target.get(99, 19).symbol(), "T");
    // bottom-right tile fills (50..100, 20..40)
    assert_eq!(target.get(50, 20).symbol(), "B");
    assert_eq!(target.get(99, 39).symbol(), "B");
}
