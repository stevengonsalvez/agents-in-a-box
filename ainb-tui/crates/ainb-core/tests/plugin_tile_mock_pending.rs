// P4: leaves whose lookup returns None are left untouched on the target
// buffer — the caller can pre-paint a "loading…" placeholder before
// dispatch and composite_snapshot will not overwrite it.

use ainb::ui::bsp::{LayoutNode, LayoutSnapshot};
use ainb::ui::bsp_render::composite_snapshot;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[test]
fn pending_tile_keeps_pre_painted_placeholder() {
    let snapshot = LayoutSnapshot {
        version: 1,
        root: LayoutNode::pane("slow_plugin"),
        min_cols: 1,
        min_rows: 1,
    };
    let area = Rect::new(0, 0, 10, 3);
    let mut target = Buffer::empty(area);
    // host paints "loading…" before dispatching to plugins
    for x in 0..10u16 {
        target.get_mut(x, 0).set_symbol("…");
    }

    let painted = composite_snapshot(&mut target, &snapshot, area, |_id, _tile| None);

    assert_eq!(painted, 0, "no leaves should report painted when lookup returns None");
    // placeholder still there
    for x in 0..10u16 {
        assert_eq!(target.get(x, 0).symbol(), "…");
    }
}
