//! 63l.6 — the list-screen mouse reducer: a lifecycle-free pointer fold over the
//! shared [`card_board::BoardLayout`](crate::widgets::card_board::BoardLayout) so
//! the Kanban / Autopilots / Skills screens are mouse-driven the same way the
//! issue board is.
//!
//! ## Why a second, simpler fold
//!
//! The issue board's [`MouseFsm`](crate::mouse::MouseFsm) is keyed on
//! [`IssueLifecycle`](ainb_hangar_proto::lifecycle::IssueLifecycle): its
//! [`Target`](crate::mouse::Target)s carry a lifecycle status because the issue
//! board supports a real cross-column drag that rewrites an issue's lifecycle
//! `state`. The other three list screens have **no user-driven cross-column
//! move**:
//!
//! - **Kanban** task lifecycle (`queued → running → done`) is DAEMON-driven, not
//!   user-draggable — a drag must never fabricate a fake state mutation.
//! - **Autopilots** / **Skills** are flat single-column lists — there is no second
//!   column to drag into.
//!
//! So binding those screens onto the lifecycle-typed FSM would be wrong (it would
//! invent a drag semantics none of them have). Instead this module folds the
//! SAME render-time geometry the card-board already produces
//! ([`BoardLayout::card_at`](crate::widgets::card_board::BoardLayout::card_at) /
//! [`column_at`](crate::widgets::card_board::BoardLayout::column_at)) into a small
//! id-keyed [`BoardMouseIntent`]: a click OPENS the card, a right-click anchors a
//! context menu, a wheel scrolls the column under the pointer, a move hovers. No
//! drag, no lifecycle, no fabricated mutation.
//!
//! ## Non-blocking, like every pointer path
//!
//! `plugin/handle_mouse` runs INLINE on the SDK reader loop, so this fold is pure
//! and IO-free: it resolves the event against the layout and returns an intent the
//! plugin glue stashes and drains on the next render (where host IO is safe),
//! exactly as the issue-board mouse path does.

use crate::widgets::card_board::BoardLayout;

/// A list-screen pointer gesture resolved against the render-time board layout.
///
/// Id-keyed (not lifecycle-keyed): `ClickOpen` / `OpenContextMenu` carry the
/// card's id (the value the screen opens / acts on), and `ScrollColumn` carries
/// the 0-based board column index (the screen maps it to its own column). The
/// plugin glue binds each to the screen's EXISTING action — there is no new wire
/// method and no fabricated mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardMouseIntent {
    /// Open the card under a left-click (down then up with no drag). Carries the
    /// card's id (task id / autopilot id / skill slug).
    ClickOpen(String),
    /// Open the right-click context menu for a card, anchored at `at`.
    OpenContextMenu {
        /// The right-clicked card's id.
        id: String,
        /// The `(col, row)` anchor the menu opens at.
        at: (u16, u16),
    },
    /// Scroll the board column under the pointer by `delta` rows (`+1` down,
    /// `-1` up) — a wheel scroll over that column's body.
    ScrollColumn {
        /// The 0-based board column index under the pointer.
        column: usize,
        /// Scroll delta: `+1` for down, `-1` for up.
        delta: i32,
    },
    /// Hover highlight: the pointer (no button) is over a card, or `None` to clear
    /// the highlight when it moves onto empty space.
    Hover(Option<String>),
}

/// Fold one forwarded [`MouseEvent`](ainb_plugin_sdk::MouseEvent) against the
/// render-time `layout`, returning the [`BoardMouseIntent`] it resolves to (or
/// `None` for a gesture with no board action).
///
/// Stateless — there is no drag FSM, because the list screens have no
/// user-driven cross-column move. A `Down{Left}` is treated as the press half of
/// a click and resolves nothing on its own; the `Up{Left}` that follows over a
/// card is the click that opens it. (The screens have no selection-on-press
/// distinct from open-on-click, so the press is inert and the release opens —
/// this keeps a stray drag from opening a card the pointer merely passed over.)
#[must_use]
pub fn fold_board_mouse(
    layout: &BoardLayout,
    ev: ainb_plugin_sdk::MouseEvent,
) -> Option<BoardMouseIntent> {
    use ainb_plugin_sdk::{MouseButton, MouseKind};
    match ev.kind {
        // A left-press is the start of a click; it opens nothing on its own (the
        // release does), so a press that turns into a drag-off never opens a card.
        MouseKind::Down {
            button: MouseButton::Left,
        } => None,
        // A right-press over a card anchors the context menu at the click.
        MouseKind::Down {
            button: MouseButton::Right,
        } => layout
            .card_at(ev.col, ev.row)
            .map(|card| BoardMouseIntent::OpenContextMenu {
                id: card.issue_id.clone(),
                at: (ev.col, ev.row),
            }),
        // A left-release over a card opens it (the click).
        MouseKind::Up {
            button: MouseButton::Left,
        } => layout
            .card_at(ev.col, ev.row)
            .map(|card| BoardMouseIntent::ClickOpen(card.issue_id.clone())),
        // A wheel scroll over a column nudges that column's scroll offset.
        MouseKind::ScrollUp => scroll(layout, ev.col, ev.row, -1),
        MouseKind::ScrollDown => scroll(layout, ev.col, ev.row, 1),
        // A pointer move (no button) hovers the card under it, or clears off-card.
        MouseKind::Moved => Some(BoardMouseIntent::Hover(
            layout.card_at(ev.col, ev.row).map(|c| c.issue_id.clone()),
        )),
        // No board action: middle/other buttons, drags (no user drag on these
        // screens), horizontal wheel.
        _ => None,
    }
}

/// Resolve a wheel scroll at `(col, row)` to the board column under it, or `None`
/// off every column.
fn scroll(layout: &BoardLayout, col: u16, row: u16, delta: i32) -> Option<BoardMouseIntent> {
    layout
        .column_at(col, row)
        .map(|c| BoardMouseIntent::ScrollColumn {
            column: c.index,
            delta,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::card_board::{render_card_board, BoardCard, BoardColumn, PriorityChip};
    use ainb_plugin_sdk::{MouseButton, MouseEvent, MouseKind, WireBuffer};

    fn ev(kind: MouseKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            col,
            row,
            mods: 0,
        }
    }

    /// A two-column board rendered through the real card-board, so the layout (and
    /// thus the hit geometry) is exactly what the screen paints.
    fn two_column_layout(buf: &mut WireBuffer) -> BoardLayout {
        let columns = vec![
            BoardColumn {
                glyph: '○',
                name: "queued".into(),
                cards: vec![BoardCard {
                    issue_id: "task-A".into(),
                    display_id: "#A".into(),
                    title: "build the thing".into(),
                    priority: PriorityChip::None,
                    assignee_initial: Some('a'),
                }],
                scroll_offset: 0,
            },
            BoardColumn {
                glyph: '◔',
                name: "running".into(),
                cards: vec![BoardCard {
                    issue_id: "task-B".into(),
                    display_id: "#B".into(),
                    title: "run the thing".into(),
                    priority: PriorityChip::None,
                    assignee_initial: Some('b'),
                }],
                scroll_offset: 0,
            },
        ];
        render_card_board(buf, 120, 0, 23, &columns, None)
    }

    /// A left-release over a painted card opens it (ClickOpen with the card id).
    #[test]
    fn left_release_over_card_opens_it() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = two_column_layout(&mut buf);
        let card = &layout.columns[1].cards[0].rect;
        let i = fold_board_mouse(
            &layout,
            ev(
                MouseKind::Up {
                    button: MouseButton::Left,
                },
                card.x + 1,
                card.y + 1,
            ),
        );
        assert_eq!(i, Some(BoardMouseIntent::ClickOpen("task-B".to_string())));
    }

    /// A left-PRESS never opens a card — only the release does (so a drag-off
    /// never opens a card the pointer merely passed over).
    #[test]
    fn left_press_opens_nothing() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = two_column_layout(&mut buf);
        let card = &layout.columns[0].cards[0].rect;
        let i = fold_board_mouse(
            &layout,
            ev(
                MouseKind::Down {
                    button: MouseButton::Left,
                },
                card.x + 1,
                card.y + 1,
            ),
        );
        assert_eq!(i, None, "a press opens nothing; the release is the click");
    }

    /// A right-press over a card anchors the context menu at the click cell.
    #[test]
    fn right_press_over_card_opens_context_menu() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = two_column_layout(&mut buf);
        let card = &layout.columns[0].cards[0].rect;
        let (cx, cy) = (card.x + 2, card.y + 1);
        let i = fold_board_mouse(
            &layout,
            ev(
                MouseKind::Down {
                    button: MouseButton::Right,
                },
                cx,
                cy,
            ),
        );
        assert_eq!(
            i,
            Some(BoardMouseIntent::OpenContextMenu {
                id: "task-A".to_string(),
                at: (cx, cy),
            })
        );
    }

    /// A wheel scroll over a column resolves to THAT column's index (not a
    /// hard-wired one).
    #[test]
    fn scroll_targets_the_column_under_the_pointer() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = two_column_layout(&mut buf);
        let col1 = &layout.columns[1].rect;
        let down = fold_board_mouse(
            &layout,
            ev(MouseKind::ScrollDown, col1.x + 1, col1.y + col1.h / 2),
        );
        assert_eq!(
            down,
            Some(BoardMouseIntent::ScrollColumn {
                column: 1,
                delta: 1,
            })
        );
        let col0 = &layout.columns[0].rect;
        let up = fold_board_mouse(
            &layout,
            ev(MouseKind::ScrollUp, col0.x + 1, col0.y + col0.h / 2),
        );
        assert_eq!(
            up,
            Some(BoardMouseIntent::ScrollColumn {
                column: 0,
                delta: -1,
            })
        );
    }

    /// A pointer move over a card hovers it; a move onto empty space clears it.
    #[test]
    fn moved_over_card_hovers_and_clears_off_card() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = two_column_layout(&mut buf);
        let card = &layout.columns[0].cards[0].rect;
        let hover = fold_board_mouse(&layout, ev(MouseKind::Moved, card.x + 1, card.y + 1));
        assert_eq!(
            hover,
            Some(BoardMouseIntent::Hover(Some("task-A".to_string())))
        );
        let clear = fold_board_mouse(&layout, ev(MouseKind::Moved, 119, 23));
        assert_eq!(clear, Some(BoardMouseIntent::Hover(None)));
    }
}
