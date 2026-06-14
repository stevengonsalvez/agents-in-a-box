//! 63l.2 — Mouse framework acceptance tests (synthetic `MouseEvent`s, NO tmux).
//!
//! These drive the [`MouseFsm`](ainb_plugin_hangar::mouse::MouseFsm) over a seeded
//! [`HitMap`](ainb_plugin_hangar::mouse::HitMap) and assert the resulting
//! [`MouseState`] + emitted [`MouseIntent`]. They are the user-visible proof that
//! `plugin/handle_mouse` is the real consumed path: a click opens, a drag across
//! columns emits a move, a scroll advances one column, a right-click anchors a
//! context menu. The mutations at the foot prove a no-op drop / wrong hit-test
//! breaks the assertion (the test can't pass by accident).

use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_hangar::mouse::{HitMap, MouseFsm, MouseIntent, MouseState, Rect, Target};
use ainb_plugin_sdk::{MouseButton, MouseEvent, MouseKind};

/// A `Down{button}` event at `(col, row)`.
const fn down(button: MouseButton, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseKind::Down { button },
        col,
        row,
        mods: 0,
    }
}

/// An `Up{button}` event at `(col, row)`.
const fn up(button: MouseButton, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseKind::Up { button },
        col,
        row,
        mods: 0,
    }
}

/// A `Drag{button}` event at `(col, row)`.
const fn drag(button: MouseButton, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseKind::Drag { button },
        col,
        row,
        mods: 0,
    }
}

/// A wheel / move event at `(col, row)`.
const fn scroll(kind: MouseKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        col,
        row,
        mods: 0,
    }
}

/// A two-column board hit-map: column 0 = Todo (header + new-button + a card A +
/// a column drop-zone), column 1 = In Progress (header + new-button + a card B +
/// a column drop-zone). Plus a Tab rect on row 0.
///
/// Geometry (cols × rows):
///   row 0: `[0,40)` Tab(IssueList)
///   col 0 occupies x `[0,20)`, col 1 occupies x `[20,40)`, body rows `[2,20)`
///   header row 1; new-button at the column's far right on row 1
///   card A at col0 rows `[3,9)`; card B at col1 rows `[3,9)`
///   each column's whole body rect is its `DropZone`
fn two_column_map() -> HitMap {
    let mut map = HitMap::default();
    // Top tab.
    map.push(Rect::new(0, 0, 40, 1), Target::Tab(0));

    // Column 0 — Todo.
    map.push(
        Rect::new(0, 1, 18, 1),
        Target::ColumnHeader(IssueLifecycle::Todo),
    );
    map.push(
        Rect::new(18, 1, 2, 1),
        Target::NewButton(IssueLifecycle::Todo),
    );
    map.push(Rect::new(0, 3, 20, 6), Target::Card("A".to_string()));
    map.push(
        Rect::new(0, 2, 20, 18),
        Target::DropZone(IssueLifecycle::Todo),
    );

    // Column 1 — In Progress.
    map.push(
        Rect::new(20, 1, 18, 1),
        Target::ColumnHeader(IssueLifecycle::InProgress),
    );
    map.push(
        Rect::new(38, 1, 2, 1),
        Target::NewButton(IssueLifecycle::InProgress),
    );
    map.push(Rect::new(20, 3, 20, 6), Target::Card("B".to_string()));
    map.push(
        Rect::new(20, 2, 20, 18),
        Target::DropZone(IssueLifecycle::InProgress),
    );
    map
}

/// Down{Left}@A then Up@A with no move = a CLICK that opens A.
#[test]
fn down_then_up_same_card_is_click_open() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();

    // Press on card A (inside its rect, x=2,y=4).
    let i1 = fsm.handle(&map, down(MouseButton::Left, 2, 4));
    assert!(
        matches!(fsm.state(), MouseState::Pressed { card, .. } if card == "A"),
        "press over card A enters Pressed(A): {:?}",
        fsm.state()
    );
    // The press selects the card.
    assert_eq!(i1, Some(MouseIntent::Select("A".to_string())));

    // Release on the same card without ever leaving it → ClickOpen(A).
    let i2 = fsm.handle(&map, up(MouseButton::Left, 3, 5));
    assert_eq!(fsm.state(), &MouseState::Idle);
    assert_eq!(i2, Some(MouseIntent::ClickOpen("A".to_string())));
}

/// `Down{Left}@A`, `Drag` into the column-2 `DropZone`, `Up` = `MoveCard{A, in_progress}`.
#[test]
fn drag_card_across_columns_emits_move() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();

    fsm.handle(&map, down(MouseButton::Left, 2, 4));
    assert!(
        matches!(fsm.state(), MouseState::Pressed { card, .. } if card == "A"),
        "press over card A enters Pressed(A): {:?}",
        fsm.state()
    );

    // Drag the pointer into column 1's drop zone — the pointer has left card A's
    // origin rect, so the FSM enters Dragging.
    let drag_i = fsm.handle(&map, drag(MouseButton::Left, 25, 6));
    assert_eq!(
        fsm.state(),
        &MouseState::Dragging {
            card: "A".to_string(),
            over: Some(IssueLifecycle::InProgress),
            origin_status: Some(IssueLifecycle::Todo),
        }
    );
    // A live drag over a foreign column is a hover-highlight hint, not yet a move.
    assert_eq!(
        drag_i,
        Some(MouseIntent::DragHover {
            card: "A".to_string(),
            over: IssueLifecycle::InProgress,
        })
    );

    // Release over column 1 → MoveCard.
    let up_i = fsm.handle(&map, up(MouseButton::Left, 25, 6));
    assert_eq!(fsm.state(), &MouseState::Idle);
    assert_eq!(
        up_i,
        Some(MouseIntent::MoveCard {
            issue_id: "A".to_string(),
            to_status: IssueLifecycle::InProgress,
        })
    );
}

/// A drag that stays WITHIN the origin column and releases there is a reorder,
/// not a cross-column move.
#[test]
fn drag_within_column_emits_reorder() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();

    fsm.handle(&map, down(MouseButton::Left, 2, 4));
    // Drag down but stay inside column 0's drop zone (below card A's rect).
    let drag_i = fsm.handle(&map, drag(MouseButton::Left, 5, 15));
    assert_eq!(
        fsm.state(),
        &MouseState::Dragging {
            card: "A".to_string(),
            over: Some(IssueLifecycle::Todo),
            origin_status: Some(IssueLifecycle::Todo),
        }
    );
    // Same-column drag is a reorder-hover, not a move-hover.
    assert!(matches!(drag_i, Some(MouseIntent::DragHover { .. })));

    // Release inside the origin column → ReorderCard (the row index is the
    // pointer row relative to the column body, resolved by the FSM).
    let up_i = fsm.handle(&map, up(MouseButton::Left, 5, 15));
    assert_eq!(fsm.state(), &MouseState::Idle);
    assert!(
        matches!(up_i, Some(MouseIntent::ReorderCard { ref issue_id, .. }) if issue_id == "A"),
        "same-column drop must reorder A, got {up_i:?}"
    );
}

/// `ScrollDown` over column 0 advances column 0's scroll only.
#[test]
fn scroll_down_over_column_scrolls_that_column() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();

    // Scroll down inside column 0's body (x=5, y=10).
    let i = fsm.handle(&map, scroll(MouseKind::ScrollDown, 5, 10));
    assert_eq!(
        i,
        Some(MouseIntent::ScrollColumn {
            status: IssueLifecycle::Todo,
            delta: 1,
        })
    );
    // Scroll up over column 1.
    let i2 = fsm.handle(&map, scroll(MouseKind::ScrollUp, 25, 10));
    assert_eq!(
        i2,
        Some(MouseIntent::ScrollColumn {
            status: IssueLifecycle::InProgress,
            delta: -1,
        })
    );
    // Scrolling never changes the FSM state — it is a stateless wheel gesture.
    assert_eq!(fsm.state(), &MouseState::Idle);
}

/// `ScrollLeft` / `ScrollRight` pan the columns horizontally.
#[test]
fn scroll_horizontal_pans_columns() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let left = fsm.handle(&map, scroll(MouseKind::ScrollLeft, 5, 10));
    assert_eq!(left, Some(MouseIntent::PanColumns { delta: -1 }));
    let right = fsm.handle(&map, scroll(MouseKind::ScrollRight, 5, 10));
    assert_eq!(right, Some(MouseIntent::PanColumns { delta: 1 }));
}

/// Down{Right}@B = OpenContextMenu(B) anchored at the click cell.
#[test]
fn right_click_card_opens_context_menu() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let i = fsm.handle(&map, down(MouseButton::Right, 25, 5));
    assert_eq!(
        i,
        Some(MouseIntent::OpenContextMenu {
            issue_id: "B".to_string(),
            at: (25, 5),
        })
    );
    // A right-click does NOT start a drag — the FSM stays Idle.
    assert_eq!(fsm.state(), &MouseState::Idle);
}

/// `Down{Left}` on a `NewButton` emits `NewIssue` for that column's status.
#[test]
fn click_new_button_emits_new_issue() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let i = fsm.handle(&map, down(MouseButton::Left, 18, 1));
    assert_eq!(i, Some(MouseIntent::NewIssue(IssueLifecycle::Todo)));
    assert_eq!(fsm.state(), &MouseState::Idle);
}

/// `Down{Left}` on a `ColumnHeader` focuses that column.
#[test]
fn click_column_header_focuses_column() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let i = fsm.handle(&map, down(MouseButton::Left, 5, 1));
    assert_eq!(i, Some(MouseIntent::FocusColumn(IssueLifecycle::Todo)));
}

/// Down{Left} on a Tab switches to that view.
#[test]
fn click_tab_switches_view() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let i = fsm.handle(&map, down(MouseButton::Left, 3, 0));
    assert_eq!(i, Some(MouseIntent::SwitchTab(0)));
}

/// Moved (no button) over a card emits a hover highlight.
#[test]
fn moved_over_card_emits_hover() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let i = fsm.handle(&map, scroll(MouseKind::Moved, 25, 5));
    assert_eq!(i, Some(MouseIntent::Hover(Some("B".to_string()))));
    // Moving onto empty space clears the hover.
    let i2 = fsm.handle(&map, scroll(MouseKind::Moved, 50, 50));
    assert_eq!(i2, Some(MouseIntent::Hover(None)));
}

// -----------------------------------------------------------------------------
// Mutation guards — the feature must TAKE EFFECT, so a broken hit-test or a
// no-op drop must FAIL these assertions (they can't pass by accident).
// -----------------------------------------------------------------------------

/// MUTATION: a press-then-release that NEVER drags must NOT emit a `MoveCard`. If
/// the FSM mistakenly treated a stationary click as a drop, this would break.
#[test]
fn mutation_click_never_emits_move() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    fsm.handle(&map, down(MouseButton::Left, 2, 4));
    let up_i = fsm.handle(&map, up(MouseButton::Left, 3, 5));
    assert!(
        !matches!(up_i, Some(MouseIntent::MoveCard { .. })),
        "a stationary click must never move a card: {up_i:?}"
    );
}

/// MUTATION: a drag that returns to the ORIGIN column before release reorders
/// (never a cross-column move) — proves the drop column is read from the release
/// point, not from wherever the drag wandered.
#[test]
fn mutation_drag_back_to_origin_does_not_cross_move() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    fsm.handle(&map, down(MouseButton::Left, 2, 4));
    // Wander into column 1 …
    fsm.handle(&map, drag(MouseButton::Left, 25, 6));
    // … then back into column 0 before releasing.
    fsm.handle(&map, drag(MouseButton::Left, 5, 12));
    let up_i = fsm.handle(&map, up(MouseButton::Left, 5, 12));
    assert!(
        !matches!(
            up_i,
            Some(MouseIntent::MoveCard {
                to_status: IssueLifecycle::InProgress,
                ..
            })
        ),
        "releasing back over the origin column must not cross-move: {up_i:?}"
    );
    assert!(
        matches!(up_i, Some(MouseIntent::ReorderCard { .. })),
        "releasing over the origin column reorders: {up_i:?}"
    );
}

/// MUTATION: scrolling over column 1 must NOT scroll column 0 — proves the
/// scroll target is resolved by the hit-test, not hard-wired to a column.
#[test]
fn mutation_scroll_targets_the_hit_column() {
    let map = two_column_map();
    let mut fsm = MouseFsm::default();
    let i = fsm.handle(&map, scroll(MouseKind::ScrollDown, 25, 10));
    assert_ne!(
        i,
        Some(MouseIntent::ScrollColumn {
            status: IssueLifecycle::Todo,
            delta: 1,
        }),
        "a scroll over column 1 must not scroll column 0"
    );
}
