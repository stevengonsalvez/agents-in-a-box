use ainb::ui::bsp::{LayoutNode, SplitDir};

#[test]
fn pane_focused_builder_marks_pane_focused() {
    let pane = LayoutNode::pane("session_list").focused(true);
    match pane {
        LayoutNode::Pane { id, focused } => {
            assert_eq!(id, "session_list");
            assert!(focused);
        }
        _ => panic!("expected Pane"),
    }
}

#[test]
fn pane_constructor_defaults_to_unfocused() {
    let pane = LayoutNode::pane("git_view");
    match pane {
        LayoutNode::Pane { id, focused } => {
            assert_eq!(id, "git_view");
            assert!(!focused);
        }
        _ => panic!("expected Pane"),
    }
}

#[test]
fn split_h_in_range_ratio_is_preserved() {
    let n = LayoutNode::split_h(0.4, LayoutNode::pane("a"), LayoutNode::pane("b"));
    assert_eq!(n.ratio(), Some(0.4));
}

#[test]
fn split_h_clamps_ratio_above_one() {
    let n = LayoutNode::split_h(2.0, LayoutNode::pane("a"), LayoutNode::pane("b"));
    assert_eq!(n.ratio(), Some(1.0));
}

#[test]
fn split_h_clamps_ratio_below_zero() {
    let n = LayoutNode::split_h(-0.5, LayoutNode::pane("a"), LayoutNode::pane("b"));
    assert_eq!(n.ratio(), Some(0.0));
}

#[test]
fn split_h_records_horizontal_dir() {
    let n = LayoutNode::split_h(0.5, LayoutNode::pane("a"), LayoutNode::pane("b"));
    match n {
        LayoutNode::Split { dir, .. } => assert_eq!(dir, SplitDir::Horizontal),
        _ => panic!("expected Split"),
    }
}

#[test]
fn split_v_records_vertical_dir() {
    let n = LayoutNode::split_v(0.5, LayoutNode::pane("a"), LayoutNode::pane("b"));
    match n {
        LayoutNode::Split { dir, .. } => assert_eq!(dir, SplitDir::Vertical),
        _ => panic!("expected Split"),
    }
}

#[test]
fn pane_ratio_is_none() {
    let p = LayoutNode::pane("only");
    assert_eq!(p.ratio(), None);
}

#[test]
fn split_h_nan_ratio_becomes_half() {
    let n = LayoutNode::split_h(f32::NAN, LayoutNode::pane("a"), LayoutNode::pane("b"));
    assert_eq!(n.ratio(), Some(0.5));
    // and serde survives the roundtrip
    let json = serde_json::to_string(&n).expect("serde encode after NaN sanitize");
    let back: LayoutNode = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, n);
}

#[test]
fn split_h_infinity_ratio_becomes_half() {
    let n = LayoutNode::split_h(f32::INFINITY, LayoutNode::pane("a"), LayoutNode::pane("b"));
    assert_eq!(n.ratio(), Some(0.5));
}

#[test]
fn split_v_nan_ratio_becomes_half() {
    let n = LayoutNode::split_v(f32::NAN, LayoutNode::pane("a"), LayoutNode::pane("b"));
    assert_eq!(n.ratio(), Some(0.5));
}
