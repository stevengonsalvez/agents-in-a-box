use ainb::ui::bsp::LayoutNode;
use ratatui::layout::Rect;

#[test]
fn horizontal_split_walks_left_right() {
    let tree = LayoutNode::split_h(0.4, LayoutNode::pane("a"), LayoutNode::pane("b"));
    let area = Rect { x: 0, y: 0, width: 100, height: 40 };
    let leaves = tree.walk(area);
    assert_eq!(leaves.len(), 2);
    assert_eq!(leaves[0].0, "a");
    assert_eq!(leaves[0].1, Rect { x: 0, y: 0, width: 40, height: 40 });
    assert_eq!(leaves[1].0, "b");
    assert_eq!(leaves[1].1, Rect { x: 40, y: 0, width: 60, height: 40 });
}

#[test]
fn vertical_split_walks_top_bottom() {
    let tree = LayoutNode::split_v(
        0.4,
        LayoutNode::pane("top"),
        LayoutNode::pane("bottom"),
    );
    let area = Rect { x: 0, y: 0, width: 50, height: 100 };
    let leaves = tree.walk(area);
    assert_eq!(leaves.len(), 2);
    assert_eq!(leaves[0].0, "top");
    assert_eq!(leaves[0].1, Rect { x: 0, y: 0, width: 50, height: 40 });
    assert_eq!(leaves[1].0, "bottom");
    assert_eq!(leaves[1].1, Rect { x: 0, y: 40, width: 50, height: 60 });
}

#[test]
fn three_deep_tree_walks_in_pre_order() {
    let tree = LayoutNode::split_h(
        0.5,
        LayoutNode::pane("left"),
        LayoutNode::split_v(
            0.5,
            LayoutNode::pane("top-right"),
            LayoutNode::pane("bottom-right"),
        ),
    );
    let area = Rect { x: 0, y: 0, width: 100, height: 40 };
    let leaves = tree.walk(area);
    assert_eq!(leaves.len(), 3);
    assert_eq!(leaves[0].0, "left");
    assert_eq!(leaves[0].1, Rect { x: 0, y: 0, width: 50, height: 40 });
    assert_eq!(leaves[1].0, "top-right");
    assert_eq!(leaves[1].1, Rect { x: 50, y: 0, width: 50, height: 20 });
    assert_eq!(leaves[2].0, "bottom-right");
    assert_eq!(leaves[2].1, Rect { x: 50, y: 20, width: 50, height: 20 });
}

#[test]
fn single_pane_walks_full_area() {
    let tree = LayoutNode::pane("only");
    let area = Rect { x: 0, y: 0, width: 50, height: 25 };
    let leaves = tree.walk(area);
    assert_eq!(leaves, vec![("only".to_string(), area)]);
}

#[test]
fn walk_uses_area_offsets_not_zero_origin() {
    let tree = LayoutNode::split_h(0.5, LayoutNode::pane("a"), LayoutNode::pane("b"));
    let area = Rect { x: 10, y: 5, width: 100, height: 40 };
    let leaves = tree.walk(area);
    assert_eq!(leaves[0].1, Rect { x: 10, y: 5, width: 50, height: 40 });
    assert_eq!(leaves[1].1, Rect { x: 60, y: 5, width: 50, height: 40 });
}

#[test]
fn walk_saturates_on_u16_overflow_instead_of_panicking() {
    // area.x near u16::MAX + non-trivial left_w would panic without saturation
    let tree = LayoutNode::split_h(0.5, LayoutNode::pane("a"), LayoutNode::pane("b"));
    let area = Rect { x: u16::MAX - 10, y: 0, width: 100, height: 40 };
    let leaves = tree.walk(area);
    // both leaves yield a Rect; right.x saturates at u16::MAX
    assert_eq!(leaves[1].1.x, u16::MAX);
}
