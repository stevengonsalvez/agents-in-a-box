// P3 visual-parity test: the default BSP root walked over a Rect produces
// rect math byte-identical to the legacy ratatui Constraint::Percentage(40)
// + Percentage(60) horizontal split used in `components/layout.rs:92`.
//
// Full-render visual-parity (capture legacy frame into a WireBuffer, render
// new BSP path, byte-diff) requires the layout.rs wiring landed under a
// follow-up bead. The math parity test below proves the foundation: when
// the new path eventually replaces the legacy split, the rects are the
// same and the components paint in the same locations.

use ainb::app::AppState;
use ainb::ui::bsp::{LayoutNode, LayoutSnapshot};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

fn legacy_40_60(area: Rect) -> [Rect; 2] {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);
    [chunks[0], chunks[1]]
}

#[test]
fn default_root_matches_legacy_40_60_split() {
    let area = Rect::new(0, 0, 100, 40);
    let bsp_leaves = LayoutNode::default_root().walk(area);
    let legacy = legacy_40_60(area);

    assert_eq!(bsp_leaves.len(), 2);
    assert_eq!(bsp_leaves[0].0, "session_list");
    assert_eq!(bsp_leaves[1].0, "live_logs_stream");
    assert_eq!(bsp_leaves[0].1, legacy[0], "session_list rect mismatch");
    assert_eq!(bsp_leaves[1].1, legacy[1], "live_logs_stream rect mismatch");
}

#[test]
fn default_root_parity_holds_across_common_terminal_sizes() {
    let sizes = [(80, 24), (120, 40), (200, 60), (30, 10)];
    for (w, h) in sizes {
        let area = Rect::new(0, 0, w, h);
        let bsp = LayoutNode::default_root().walk(area);
        let legacy = legacy_40_60(area);
        assert_eq!(bsp[0].1, legacy[0], "session_list rect mismatch at {w}x{h}");
        assert_eq!(bsp[1].1, legacy[1], "live_logs_stream rect mismatch at {w}x{h}");
    }
}

#[test]
fn app_state_bsp_field_defaults_to_none() {
    let state = AppState::default();
    assert!(state.bsp.is_none(), "BSP must default to None to preserve legacy path");
}

#[test]
fn app_state_accepts_a_layout_snapshot_when_set() {
    let mut state = AppState::default();
    state.bsp = Some(LayoutSnapshot {
        version: 1,
        root: LayoutNode::default_root(),
        min_cols: 30,
        min_rows: 10,
    });
    assert!(state.bsp.is_some());
    let snap = state.bsp.as_ref().unwrap();
    assert_eq!(snap.version, 1);
    assert_eq!(snap.min_cols, 30);
}

#[test]
fn default_root_leaves_session_list_focused() {
    // The default BSP root marks the left tile (session_list) as focused
    // to match the legacy implicit "session list is the primary pane"
    // behaviour.
    let root = LayoutNode::default_root();
    match &root {
        LayoutNode::Split { left, right, .. } => {
            match left.as_ref() {
                LayoutNode::Pane { id, focused } => {
                    assert_eq!(id, "session_list");
                    assert!(*focused, "session_list should start focused");
                }
                _ => panic!("expected Pane on left"),
            }
            match right.as_ref() {
                LayoutNode::Pane { id, focused } => {
                    assert_eq!(id, "live_logs_stream");
                    assert!(!*focused);
                }
                _ => panic!("expected Pane on right"),
            }
        }
        _ => panic!("expected Split at root"),
    }
}
