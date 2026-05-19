// P6 hit_test truth table: (col, row) -> Option<HitTarget> against a
// stamped Vec<TileRegion>. Covers interior clicks, border-edge clicks,
// and out-of-bounds.

use ainb::ui::bsp_mouse::{BorderEdge, HitTarget, TileRegion, hit_test};
use ratatui::layout::Rect;

fn make_regions() -> Vec<TileRegion> {
    vec![
        TileRegion {
            leaf_id: "left".into(),
            rect: Rect::new(0, 0, 40, 20),
        },
        TileRegion {
            leaf_id: "right".into(),
            rect: Rect::new(40, 0, 60, 20),
        },
    ]
}

#[test]
fn click_inside_left_leaf_returns_interior_left() {
    let h = hit_test(&make_regions(), 10, 10);
    assert_eq!(
        h,
        Some(HitTarget::Interior {
            leaf_id: "left".into()
        })
    );
}

#[test]
fn click_inside_right_leaf_returns_interior_right() {
    let h = hit_test(&make_regions(), 70, 10);
    assert_eq!(
        h,
        Some(HitTarget::Interior {
            leaf_id: "right".into()
        })
    );
}

#[test]
fn click_on_left_leafs_left_border() {
    let h = hit_test(&make_regions(), 0, 10);
    assert_eq!(
        h,
        Some(HitTarget::Border {
            leaf_id: "left".into(),
            edge: BorderEdge::Left,
        })
    );
}

#[test]
fn click_on_left_leafs_right_border() {
    // col=39 is the last column of the left leaf (x=0..40)
    let h = hit_test(&make_regions(), 39, 10);
    assert_eq!(
        h,
        Some(HitTarget::Border {
            leaf_id: "left".into(),
            edge: BorderEdge::Right,
        })
    );
}

#[test]
fn click_on_top_border() {
    let h = hit_test(&make_regions(), 10, 0);
    assert!(matches!(
        h,
        Some(HitTarget::Border {
            edge: BorderEdge::Top,
            ..
        })
    ));
}

#[test]
fn click_on_bottom_border() {
    let h = hit_test(&make_regions(), 10, 19);
    assert!(matches!(
        h,
        Some(HitTarget::Border {
            edge: BorderEdge::Bottom,
            ..
        })
    ));
}

#[test]
fn click_outside_all_tiles_returns_none() {
    assert_eq!(hit_test(&make_regions(), 1000, 1000), None);
    assert_eq!(hit_test(&make_regions(), 200, 10), None);
    assert_eq!(hit_test(&make_regions(), 10, 100), None);
}

#[test]
fn empty_regions_returns_none() {
    assert_eq!(hit_test(&[], 10, 10), None);
}
