use ainb::ui::bsp::{BspError, LayoutNode};
use ratatui::layout::Rect;

#[test]
fn walk_checked_passes_when_all_tiles_meet_min() {
    let tree = LayoutNode::split_h(0.4, LayoutNode::pane("a"), LayoutNode::pane("b"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 40,
    };
    let result = tree.walk_checked(area, 30, 10);
    assert!(result.is_ok(), "expected Ok, got {:?}", result);
    assert_eq!(result.unwrap().len(), 2);
}

#[test]
fn walk_checked_errors_when_width_below_min() {
    // 0.2 of 100 = 20-wide tile, min is 30 → error
    let tree = LayoutNode::split_h(0.2, LayoutNode::pane("a"), LayoutNode::pane("b"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 40,
    };
    let result = tree.walk_checked(area, 30, 10);
    assert!(matches!(result, Err(BspError::TileTooSmall { .. })));
}

#[test]
fn walk_checked_errors_when_height_below_min() {
    // 0.2 of 40 = 8-tall tile, min is 10 → error
    let tree = LayoutNode::split_v(0.2, LayoutNode::pane("a"), LayoutNode::pane("b"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 40,
    };
    let result = tree.walk_checked(area, 30, 10);
    assert!(matches!(result, Err(BspError::TileTooSmall { .. })));
}

#[test]
fn walk_checked_single_pane_passes_min() {
    let tree = LayoutNode::pane("only");
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let result = tree.walk_checked(area, 30, 10);
    assert!(result.is_ok());
}

#[test]
fn walk_checked_single_pane_below_min_errors() {
    let tree = LayoutNode::pane("only");
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 10,
    };
    let result = tree.walk_checked(area, 30, 10);
    assert!(matches!(result, Err(BspError::TileTooSmall { .. })));
}
