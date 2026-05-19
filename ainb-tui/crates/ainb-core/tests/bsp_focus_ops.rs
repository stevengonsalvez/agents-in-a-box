use ainb::ui::bsp::{LayoutNode, SplitDir};

fn collect_focused(node: &LayoutNode, out: &mut Vec<String>) {
    match node {
        LayoutNode::Pane { id, focused } => {
            if *focused {
                out.push(id.clone());
            }
        }
        LayoutNode::Split { left, right, .. } => {
            collect_focused(left, out);
            collect_focused(right, out);
        }
    }
}

fn focused_ids(node: &LayoutNode) -> Vec<String> {
    let mut v = vec![];
    collect_focused(node, &mut v);
    v
}

#[test]
fn cycle_focus_next_moves_through_leaves_in_pre_order() {
    let mut tree = LayoutNode::split_h(
        0.5,
        LayoutNode::pane("a").focused(true),
        LayoutNode::split_v(0.5, LayoutNode::pane("b"), LayoutNode::pane("c")),
    );

    assert_eq!(focused_ids(&tree), vec!["a".to_string()]);

    tree.cycle_focus_next();
    assert_eq!(focused_ids(&tree), vec!["b".to_string()]);

    tree.cycle_focus_next();
    assert_eq!(focused_ids(&tree), vec!["c".to_string()]);

    // wraps
    tree.cycle_focus_next();
    assert_eq!(focused_ids(&tree), vec!["a".to_string()]);
}

#[test]
fn cycle_focus_prev_walks_backwards_through_leaves() {
    let mut tree = LayoutNode::split_h(
        0.5,
        LayoutNode::pane("a").focused(true),
        LayoutNode::pane("b"),
    );
    tree.cycle_focus_prev();
    assert_eq!(focused_ids(&tree), vec!["b".to_string()]);

    tree.cycle_focus_prev();
    assert_eq!(focused_ids(&tree), vec!["a".to_string()]);
}

#[test]
fn cycle_focus_when_nothing_focused_focuses_first_leaf() {
    let mut tree = LayoutNode::split_h(0.5, LayoutNode::pane("a"), LayoutNode::pane("b"));
    tree.cycle_focus_next();
    assert_eq!(focused_ids(&tree), vec!["a".to_string()]);
}

#[test]
fn close_focused_promotes_sibling_to_parent_slot() {
    let mut tree = LayoutNode::split_h(
        0.4,
        LayoutNode::pane("a").focused(true),
        LayoutNode::pane("b"),
    );
    let closed = tree.close_focused();
    assert!(closed);

    // After close, sibling "b" is the new root, and it is focused.
    match &tree {
        LayoutNode::Pane { id, focused } => {
            assert_eq!(id, "b");
            assert!(*focused, "sibling should inherit focus");
        }
        _ => panic!("expected root to be Pane after close"),
    }
}

#[test]
fn close_focused_on_root_only_leaf_is_noop() {
    let mut tree = LayoutNode::pane("only").focused(true);
    let closed = tree.close_focused();
    assert!(!closed, "should not close root-only leaf");
    match &tree {
        LayoutNode::Pane { id, focused } => {
            assert_eq!(id, "only");
            assert!(*focused);
        }
        _ => panic!("expected root unchanged"),
    }
}

#[test]
fn close_focused_in_deeper_tree_keeps_sibling_subtree_shape() {
    let mut tree = LayoutNode::split_h(
        0.5,
        LayoutNode::pane("a").focused(true),
        LayoutNode::split_v(0.5, LayoutNode::pane("b"), LayoutNode::pane("c")),
    );
    let closed = tree.close_focused();
    assert!(closed);
    // After close, root is the right subtree (split_v containing b/c)
    match &tree {
        LayoutNode::Split { dir, .. } => assert_eq!(*dir, SplitDir::Vertical),
        _ => panic!("expected Split (the promoted right subtree)"),
    }
    // Focus should land on the first leaf of the promoted subtree.
    assert_eq!(focused_ids(&tree), vec!["b".to_string()]);
}

#[test]
fn split_focused_replaces_focused_leaf_with_split() {
    let mut tree = LayoutNode::pane("only").focused(true);
    tree.split_focused(SplitDir::Horizontal, 0.5, "new".to_string());
    match &tree {
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            assert_eq!(*dir, SplitDir::Horizontal);
            assert!((*ratio - 0.5).abs() < f32::EPSILON);
            match (left.as_ref(), right.as_ref()) {
                (
                    LayoutNode::Pane {
                        id: lid,
                        focused: lf,
                    },
                    LayoutNode::Pane {
                        id: rid,
                        focused: rf,
                    },
                ) => {
                    assert_eq!(lid, "only");
                    assert!(*lf, "original pane keeps focus by default");
                    assert_eq!(rid, "new");
                    assert!(!*rf);
                }
                _ => panic!("expected two Panes under Split"),
            }
        }
        _ => panic!("expected Split after split_focused"),
    }
}
