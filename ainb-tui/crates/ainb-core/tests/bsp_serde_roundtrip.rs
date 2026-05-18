use ainb::ui::bsp::{LayoutNode, LayoutSnapshot};

fn depth_three_tree() -> LayoutNode {
    LayoutNode::split_h(
        0.3,
        LayoutNode::pane("sidebar").focused(true),
        LayoutNode::split_v(
            0.6,
            LayoutNode::pane("top"),
            LayoutNode::split_h(
                0.5,
                LayoutNode::pane("bottom-left"),
                LayoutNode::pane("bottom-right"),
            ),
        ),
    )
}

#[test]
fn layout_node_roundtrips_through_json() {
    let tree = depth_three_tree();
    let json = serde_json::to_string(&tree).expect("serialize");
    let back: LayoutNode = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, tree);
}

#[test]
fn snapshot_roundtrips_byte_identical_on_second_encode() {
    let snap = LayoutSnapshot {
        version: 1,
        root: depth_three_tree(),
        min_cols: 30,
        min_rows: 10,
    };
    let json1 = serde_json::to_string(&snap).expect("ser1");
    let back: LayoutSnapshot = serde_json::from_str(&json1).expect("de");
    let json2 = serde_json::to_string(&back).expect("ser2");
    assert_eq!(json1, json2);
    assert_eq!(snap, back);
}

#[test]
fn snapshot_exposes_version_min_cols_min_rows() {
    let snap = LayoutSnapshot {
        version: 1,
        root: LayoutNode::pane("only"),
        min_cols: 30,
        min_rows: 10,
    };
    assert_eq!(snap.version, 1);
    assert_eq!(snap.min_cols, 30);
    assert_eq!(snap.min_rows, 10);
}

#[test]
fn snapshot_decodes_with_only_root_field() {
    // a legacy / hand-rolled JSON with just `root` should decode using
    // built-in defaults (version=1, min_cols=30, min_rows=10).
    let json = r#"{"root":{"kind":"pane","id":"only"}}"#;
    let snap: LayoutSnapshot = serde_json::from_str(json).expect("decode partial");
    assert_eq!(snap.version, 1);
    assert_eq!(snap.min_cols, 30);
    assert_eq!(snap.min_rows, 10);
    match snap.root {
        LayoutNode::Pane { id, focused } => {
            assert_eq!(id, "only");
            assert!(!focused);
        }
        _ => panic!("expected Pane"),
    }
}
