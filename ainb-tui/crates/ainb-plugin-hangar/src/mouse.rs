//! 63l.2 — the card-board mouse framework: a render-time hit-map, a
//! [`MouseState`] drag FSM, and the [`MouseIntent`]s every pointer gesture folds
//! into.
//!
//! ## The shape of the problem
//!
//! The host captures the terminal mouse (ainb-core `EnableMouseCapture`) and
//! forwards each event to the focused plugin as a viewport-relative
//! [`MouseEvent`](ainb_plugin_sdk::MouseEvent) via `plugin/handle_mouse`. The
//! hangar plugin consumed none of it before this bead. This module is the pure,
//! IO-free core that turns those events into board actions:
//!
//! ```text
//!   ┌─────────────┐   build    ┌──────────┐  hit_test  ┌────────────┐
//!   │ render(buf) │──────────▶ │  HitMap  │ ─────────▶ │  MouseFsm  │
//!   │ BoardLayout │  per frame │ Rect→Tgt │            │  handle()  │
//!   └─────────────┘            └──────────┘            └─────┬──────┘
//!                                                            │ MouseIntent
//!                                                            ▼
//!                                          drained next render/glue tick (P2 RPC)
//! ```
//!
//! ## Why a hit-map, not re-derived geometry
//!
//! The render is the single source of truth for *where things landed*. Rather
//! than re-deriving the column/card geometry in the mouse handler (which would
//! drift the instant the render changed), the render records a [`HitMap`] — a
//! flat `Vec<(Rect, Target)>` — and the handler hit-tests against it. The hit-map
//! is rebuilt each frame and lives in the screen's reducer state.
//!
//! ## Non-blocking by construction
//!
//! `plugin/handle_mouse` runs INLINE on the SDK reader loop (same as
//! `handle_key`), so the handler MUST NOT block on a socket. This module never
//! touches IO: it folds the event into the FSM and returns an intent. The plugin
//! glue stashes the intent and drains it on the next `render` (a spawned task,
//! where host IO is safe), exactly as the deferred key-path actions already do.
//! This bead emits the intents; P2 binds the mutating ones to daemon RPCs.

use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_sdk::{MouseButton, MouseEvent, MouseKind};

use crate::widgets::card_board::BoardLayout;
pub use crate::widgets::card_board::Rect;

/// What a cell of the board resolves to when the pointer lands on it.
///
/// Built from the [`BoardLayout`](crate::widgets::card_board::BoardLayout) the
/// render produced (cards, headers, the `+` create affordance, the column body
/// as a drop zone) plus the top tab bar. The hit-test resolves a `(col, row)` to
/// the most specific target covering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A card, tagged with the issue id it renders (the value a click opens).
    Card(String),
    /// A column header — clicking it focuses the column.
    ColumnHeader(IssueLifecycle),
    /// The `+` create affordance in a column header — clicking it starts a
    /// new-issue flow seeded with the column's status.
    NewButton(IssueLifecycle),
    /// A column's body region — the drop zone a dragged card lands in. The card
    /// rects sit *inside* this rect, so the hit-test resolves a card first and a
    /// bare drop-zone only on the column background.
    DropZone(IssueLifecycle),
    /// A filter-chip-bar entry, tagged with the 0-based chip index it selects
    /// (`All` / `Members` / `Agents` / `Mine`). Sits on the chip row above the
    /// board, so it never overlaps a card/column target.
    Tab(usize),
}

/// The render-time hit-map: the on-screen geometry the render painted.
///
/// Each painted rectangle is tagged with what it resolves to. Rebuilt each frame
/// and stored in the screen state; the mouse handler hit-tests against it so it
/// can never drift from the paint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HitMap {
    /// The painted rectangles, in push order. The hit-test resolves overlaps by
    /// [`Target`] *priority* (most specific wins), not by push order, so the
    /// caller is free to push in any convenient order.
    entries: Vec<(Rect, Target)>,
}

impl HitMap {
    /// Record one painted `(rect, target)` pair.
    pub fn push(&mut self, rect: Rect, target: Target) {
        self.entries.push((rect, target));
    }

    /// Build the board portion of a hit-map from the [`BoardLayout`] a card-board
    /// render produced (63l.1). Each painted column contributes a
    /// [`Target::ColumnHeader`] (its header row), a [`Target::NewButton`] (the `+`
    /// create affordance, when it fit), a [`Target::DropZone`] (the column body
    /// below the header), and a [`Target::Card`] per visible card.
    ///
    /// The column's lifecycle status is read off its 0-based `index` via
    /// [`IssueLifecycle::ALL`] — the same left-to-right order the board paints in.
    /// An out-of-range index (a board with more columns than the canonical five)
    /// is skipped rather than panicking. Cards are pushed LAST so they win the
    /// priority tie against the drop zone they sit inside (see [`hit_test`]).
    ///
    /// The filter-chip bar is added separately by the caller (it is chrome, not
    /// part of the board layout) via [`HitMap::push`] with a [`Target::Tab`].
    ///
    /// [`hit_test`]: HitMap::hit_test
    #[must_use]
    pub fn from_board_layout(layout: &BoardLayout) -> Self {
        let mut map = Self::default();
        for col in &layout.columns {
            let Some(status) = IssueLifecycle::ALL.get(col.index).copied() else {
                continue;
            };
            // Column header row spans the column width.
            map.push(
                Rect::new(col.rect.x, col.header_y, col.rect.w, 1),
                Target::ColumnHeader(status),
            );
            // The `+` create affordance, when the column was wide enough to paint
            // it (a 1×1 cell on the header row).
            if let Some(plus_x) = col.create_affordance_x {
                map.push(
                    Rect::new(plus_x, col.header_y, 1, 1),
                    Target::NewButton(status),
                );
            }
            // The column body below the header is the drop zone (spans to the
            // column bottom). Cards sit inside it and win by priority.
            let body_top = col.header_y.saturating_add(1);
            let body_h = col.rect.bottom().saturating_sub(body_top);
            map.push(
                Rect::new(col.rect.x, body_top, col.rect.w, body_h),
                Target::DropZone(status),
            );
            // Cards last so they outrank the drop zone on the priority tie.
            for card in &col.cards {
                map.push(card.rect, Target::Card(card.issue_id.clone()));
            }
        }
        map
    }

    /// Resolve `(col, row)` to the most specific [`Target`] covering it, or
    /// `None` when the point hits no recorded rectangle.
    ///
    /// Overlapping rects are resolved by [`target_priority`]: a `Card` (or a
    /// `NewButton`, which sits inside a header row) wins over the column-spanning
    /// `DropZone`/`ColumnHeader` it overlaps, so a click on a card never resolves
    /// to the column background beneath it.
    #[must_use]
    pub fn hit_test(&self, col: u16, row: u16) -> Option<&Target> {
        self.entries
            .iter()
            .filter(|(rect, _)| rect.contains(col, row))
            .max_by_key(|(_, target)| target_priority(target))
            .map(|(_, target)| target)
    }

    /// The lifecycle status of the column whose [`Target::DropZone`] /
    /// [`Target::ColumnHeader`] / [`Target::NewButton`] covers `(col, row)`, or
    /// `None` off every column. The column membership a drag/scroll resolves
    /// against — independent of whether a card sits under the point.
    #[must_use]
    pub fn column_at(&self, col: u16, row: u16) -> Option<IssueLifecycle> {
        self.entries.iter().find_map(|(rect, target)| {
            (rect.contains(col, row)).then(|| column_status(target)).flatten()
        })
    }

    /// The card [`Rect`] tagged with the card under `(col, row)`, if any — the
    /// origin rect a press records so a drag can tell when it has left the card.
    #[must_use]
    pub fn card_rect_at(&self, col: u16, row: u16) -> Option<Rect> {
        self.entries.iter().find_map(|(rect, target)| {
            (matches!(target, Target::Card(_)) && rect.contains(col, row)).then_some(*rect)
        })
    }

    /// The column body [`Rect`] tagged with `status`, if the map recorded one.
    /// Used to resolve a same-column reorder's drop row relative to the body top.
    #[must_use]
    fn drop_zone_rect(&self, status: IssueLifecycle) -> Option<Rect> {
        self.entries.iter().find_map(|(rect, target)| {
            matches!(target, Target::DropZone(s) if *s == status).then_some(*rect)
        })
    }

    /// Whether the map is empty (no rectangles recorded this frame).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The lifecycle status a column-scoped [`Target`] carries, or `None` for the
/// card / tab targets (which are not themselves a column).
const fn column_status(target: &Target) -> Option<IssueLifecycle> {
    match target {
        Target::ColumnHeader(s) | Target::NewButton(s) | Target::DropZone(s) => Some(*s),
        Target::Card(_) | Target::Tab(_) => None,
    }
}

/// Hit-test resolution priority — higher wins when rects overlap.
///
/// A `Card` is the most specific (it sits inside a column's drop zone); a
/// `NewButton` sits inside the header row, so it beats the `ColumnHeader`; the
/// column-spanning `DropZone` and `ColumnHeader` are the backgrounds, resolved
/// only when nothing more specific covers the point; a `Tab` lives on its own
/// row and never overlaps the body.
const fn target_priority(target: &Target) -> u8 {
    match target {
        Target::Card(_) => 4,
        Target::NewButton(_) => 3,
        Target::ColumnHeader(_) => 2,
        Target::Tab(_) => 1,
        Target::DropZone(_) => 0,
    }
}

/// The drag state machine the pointer folds through.
///
/// `Idle → Pressed` on a left-button-down over a card; `Pressed → Dragging` once
/// the pointer leaves the origin card's rect; the release resolves `Dragging`
/// into a move / reorder (or a cancel) and returns to `Idle`. A press that
/// releases without ever leaving the origin card is a *click*, not a drag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MouseState {
    /// No button held — the resting state.
    #[default]
    Idle,
    /// A left-button-down landed on a card; the pointer has not yet left it.
    /// Holds the card id, its origin rect (the boundary the drag must cross to
    /// lift), and the column it was pressed in (so the release can tell a
    /// same-column reorder from a cross-column move).
    Pressed {
        /// The pressed card's issue id.
        card: String,
        /// The pressed card's on-screen rect (the "origin" the drag leaves).
        origin: Rect,
        /// The column the card was pressed in.
        origin_status: Option<IssueLifecycle>,
    },
    /// The pointer has left the origin card with the button held — a live drag.
    /// `over` is the column the pointer currently hovers (the highlight target),
    /// or `None` over board background; `origin_status` is carried from `Pressed`
    /// so the drop can be classified at release.
    Dragging {
        /// The dragged card's issue id (the "lifted" card the render shows).
        card: String,
        /// The column the pointer currently hovers, or `None` off-board.
        over: Option<IssueLifecycle>,
        /// The column the card was lifted from (carried from `Pressed`).
        origin_status: Option<IssueLifecycle>,
    },
}

/// A board action one pointer gesture resolves to.
///
/// This bead emits the intents; the plugin glue stashes them and (in P2) binds
/// the mutating ones to daemon RPCs. The read-only ones (`Select`, `Hover`,
/// `DragHover`, `ScrollColumn`, `PanColumns`, `FocusColumn`) drive local board
/// state (selection, scroll, hover highlight) without a daemon round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseIntent {
    /// Select the card under a left-button-down (the press half of a click).
    Select(String),
    /// Open the card under a click (left-down then left-up with no drag).
    ClickOpen(String),
    /// Move a dragged card to a different column's status (a cross-column drop).
    MoveCard {
        /// The dragged card's issue id.
        issue_id: String,
        /// The destination column's lifecycle status.
        to_status: IssueLifecycle,
    },
    /// Reorder a dragged card within its own column to `to_index` (a same-column
    /// drop). The index is the pointer's row offset into the column body.
    ReorderCard {
        /// The dragged card's issue id.
        issue_id: String,
        /// The destination row index within the origin column.
        to_index: usize,
    },
    /// A live-drag hover: the dragged card is over `over`'s column — the render
    /// highlights the hovered drop zone and shows the lifted card.
    DragHover {
        /// The dragged card's issue id.
        card: String,
        /// The column the pointer currently hovers.
        over: IssueLifecycle,
    },
    /// Open the right-click context menu for a card, anchored at `at` (the click
    /// cell). The overlay itself is P3; this bead emits the intent + anchor.
    OpenContextMenu {
        /// The right-clicked card's issue id.
        issue_id: String,
        /// The `(col, row)` anchor the menu opens at.
        at: (u16, u16),
    },
    /// Scroll one column vertically by `delta` rows (`+1` down, `-1` up) — a
    /// wheel scroll over that column's body.
    ScrollColumn {
        /// The scrolled column's status.
        status: IssueLifecycle,
        /// Scroll delta: `+1` for down, `-1` for up.
        delta: i32,
    },
    /// Pan the columns horizontally by `delta` (`+1` right, `-1` left) — a
    /// horizontal wheel gesture anywhere on the board.
    PanColumns {
        /// Pan delta: `+1` for right, `-1` for left.
        delta: i32,
    },
    /// Hover highlight: the pointer (no button) is over a card, or `None` to
    /// clear the highlight when it moves onto empty space.
    Hover(Option<String>),
    /// Start a new-issue flow seeded with a column's status (a `+` button click).
    NewIssue(IssueLifecycle),
    /// Focus a column (a column-header / column-background click).
    FocusColumn(IssueLifecycle),
    /// Select a filter chip by its 0-based index (a chip-bar click): the plugin
    /// glue maps it onto `IssueListEvent::SetFilter(FilterChip::all()[idx])`.
    SwitchTab(usize),
}

/// The mouse framework's folding core: holds the [`MouseState`] and maps each
/// forwarded [`MouseEvent`] (hit-tested against the current [`HitMap`]) to a
/// [`MouseIntent`].
///
/// Pure and IO-free — [`MouseFsm::handle`] is total, never blocks, and never
/// allocates beyond the returned intent. The plugin's `handle_mouse` owns one
/// of these and drains the returned intents on the next render.
#[derive(Debug, Clone, Default)]
pub struct MouseFsm {
    state: MouseState,
}

impl MouseFsm {
    /// The current drag state (for the render to draw the lifted card + the
    /// highlighted drop zone, and for tests to assert the transition).
    #[must_use]
    pub const fn state(&self) -> &MouseState {
        &self.state
    }

    /// Fold one forwarded [`MouseEvent`] against `map`, advancing the FSM and
    /// returning the [`MouseIntent`] it resolves to (or `None` when the gesture
    /// has no board action — e.g. a drag with the button held but no target).
    pub fn handle(&mut self, map: &HitMap, ev: MouseEvent) -> Option<MouseIntent> {
        match ev.kind {
            MouseKind::Down { button } => self.on_down(map, button, ev.col, ev.row),
            MouseKind::Up { button } => self.on_up(map, button, ev.col, ev.row),
            MouseKind::Drag { button } => self.on_drag(map, button, ev.col, ev.row),
            MouseKind::Moved => Some(Self::on_moved(map, ev.col, ev.row)),
            MouseKind::ScrollUp => Self::on_scroll_vertical(map, ev.col, ev.row, -1),
            MouseKind::ScrollDown => Self::on_scroll_vertical(map, ev.col, ev.row, 1),
            MouseKind::ScrollLeft => Some(MouseIntent::PanColumns { delta: -1 }),
            MouseKind::ScrollRight => Some(MouseIntent::PanColumns { delta: 1 }),
        }
    }

    /// A button went down. Left over a card arms a press (selects it); right over
    /// a card opens the context menu; left over a header/new-button/tab fires the
    /// matching intent. A down anywhere else is a no-op.
    fn on_down(
        &mut self,
        map: &HitMap,
        button: MouseButton,
        col: u16,
        row: u16,
    ) -> Option<MouseIntent> {
        let target = map.hit_test(col, row)?.clone();
        match (button, &target) {
            // Right-click a card → context menu (does NOT start a drag).
            (MouseButton::Right, Target::Card(id)) => Some(MouseIntent::OpenContextMenu {
                issue_id: id.clone(),
                at: (col, row),
            }),
            // Left-press a card → select + arm the press (a possible drag/click).
            (MouseButton::Left, Target::Card(id)) => {
                let origin =
                    map.card_rect_at(col, row).unwrap_or_else(|| Rect::new(col, row, 1, 1));
                self.state = MouseState::Pressed {
                    card: id.clone(),
                    origin,
                    origin_status: map.column_at(col, row),
                };
                Some(MouseIntent::Select(id.clone()))
            }
            // Left-click the affordances.
            (MouseButton::Left, Target::NewButton(s)) => Some(MouseIntent::NewIssue(*s)),
            (MouseButton::Left, Target::Tab(v)) => Some(MouseIntent::SwitchTab(*v)),
            // Left-click a column header OR its bare background → focus the column
            // (no drag origin — only a card press arms a drag).
            (MouseButton::Left, Target::ColumnHeader(s) | Target::DropZone(s)) => {
                Some(MouseIntent::FocusColumn(*s))
            }
            _ => None,
        }
    }

    /// The drag fold: while a left press is armed, a drag that leaves the origin
    /// card promotes to `Dragging`; a drag already in `Dragging` updates the
    /// hovered column. A drag with no armed press (button held but started off a
    /// card) is a no-op.
    fn on_drag(
        &mut self,
        map: &HitMap,
        button: MouseButton,
        col: u16,
        row: u16,
    ) -> Option<MouseIntent> {
        if button != MouseButton::Left {
            return None;
        }
        let (card, origin_status) = match &self.state {
            MouseState::Pressed {
                card,
                origin,
                origin_status,
            } => {
                // A jitter that stays inside the origin card never lifts it.
                if origin.contains(col, row) {
                    return None;
                }
                (card.clone(), *origin_status)
            }
            MouseState::Dragging {
                card,
                origin_status,
                ..
            } => (card.clone(), *origin_status),
            MouseState::Idle => return None,
        };
        let over = map.column_at(col, row);
        self.state = MouseState::Dragging {
            card: card.clone(),
            over,
            origin_status,
        };
        over.map(|over| MouseIntent::DragHover { card, over })
    }

    /// The release fold: resolve the gesture and return to `Idle`.
    ///
    /// - From `Pressed` (never dragged): a CLICK → [`MouseIntent::ClickOpen`].
    /// - From `Dragging` over a *different* column: a MOVE →
    ///   [`MouseIntent::MoveCard`].
    /// - From `Dragging` over the *origin* column: a REORDER →
    ///   [`MouseIntent::ReorderCard`] at the pointer's row offset.
    /// - From `Dragging` over board background: a cancel (no intent).
    ///
    /// The DROP column is read from the RELEASE point, never from wherever the
    /// drag wandered — so a drag that returns to the origin column reorders.
    fn on_up(
        &mut self,
        map: &HitMap,
        button: MouseButton,
        col: u16,
        row: u16,
    ) -> Option<MouseIntent> {
        let prior = std::mem::take(&mut self.state);
        if button != MouseButton::Left {
            return None;
        }
        match prior {
            // A press that never became a drag is a click → open the card.
            MouseState::Pressed { card, .. } => Some(MouseIntent::ClickOpen(card)),
            MouseState::Dragging {
                card,
                origin_status,
                ..
            } => map.column_at(col, row).map(|drop| {
                // Same column (or an unknown origin, the conservative default) →
                // reorder; a known different column → cross-move.
                if origin_status.is_none_or(|origin| origin == drop) {
                    let to_index = reorder_index(map, drop, row);
                    MouseIntent::ReorderCard {
                        issue_id: card,
                        to_index,
                    }
                } else {
                    MouseIntent::MoveCard {
                        issue_id: card,
                        to_status: drop,
                    }
                }
            }),
            MouseState::Idle => None,
        }
    }

    /// A pointer move with no button: a hover highlight over a card, or a clear
    /// (`Hover(None)`) over empty space. Stateless — never touches the drag FSM.
    fn on_moved(map: &HitMap, col: u16, row: u16) -> MouseIntent {
        let card = match map.hit_test(col, row) {
            Some(Target::Card(id)) => Some(id.clone()),
            _ => None,
        };
        MouseIntent::Hover(card)
    }

    /// A vertical wheel scroll: scroll the column under the pointer by `delta`
    /// rows. A scroll off any column is a no-op.
    fn on_scroll_vertical(map: &HitMap, col: u16, row: u16, delta: i32) -> Option<MouseIntent> {
        map.column_at(col, row)
            .map(|status| MouseIntent::ScrollColumn { status, delta })
    }
}

/// The 0-based row index a same-column drop at viewport `row` maps to, relative
/// to the column body top. The drop-zone rect's top is the body origin; the
/// pointer row minus that top is the insert index. Falls back to `0` when the
/// drop zone isn't in the map (a degenerate hit-map). Saturates so a drop above
/// the body top maps to index `0`.
fn reorder_index(map: &HitMap, status: IssueLifecycle, row: u16) -> usize {
    let body_top = map.drop_zone_rect(status).map_or(row, |r| r.y);
    usize::from(row.saturating_sub(body_top))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::card_board::{BoardCard, BoardColumn, PriorityChip, render_card_board};
    use ainb_plugin_sdk::WireBuffer;

    /// The hit-map built from a real card-board render resolves a click on a
    /// painted card to that card, the column header to its status, and the `+`
    /// affordance to a new-issue target — proving the framework consumes the
    /// layout the render actually produced (no re-derived geometry).
    #[test]
    fn from_board_layout_resolves_real_card_geometry() {
        let columns = vec![
            BoardColumn {
                glyph: '☰',
                name: "Backlog".into(),
                cards: vec![BoardCard {
                    issue_id: "HGR-1".into(),
                    display_id: "HGR-1".into(),
                    title: "Triage".into(),
                    priority: PriorityChip::Low,
                    assignee_initial: Some('a'),
                    linked: false,
                }],
                scroll_offset: 0,
            },
            BoardColumn {
                glyph: '○',
                name: "Todo".into(),
                cards: vec![BoardCard {
                    issue_id: "HGR-2".into(),
                    display_id: "HGR-2".into(),
                    title: "Write".into(),
                    priority: PriorityChip::Medium,
                    assignee_initial: Some('b'),
                    linked: false,
                }],
                scroll_offset: 0,
            },
        ];
        let mut buf = WireBuffer::new(120, 24);
        let layout = render_card_board(&mut buf, 120, 0, 23, &columns, None);
        let map = HitMap::from_board_layout(&layout);

        // A point inside the first column's card resolves to HGR-1.
        let card0 = &layout.columns[0].cards[0].rect;
        assert_eq!(
            map.hit_test(card0.x + 1, card0.y + 1),
            Some(&Target::Card("HGR-1".into()))
        );
        // The first column's header row resolves to Backlog (index 0 → Backlog).
        let hdr_y = layout.columns[0].header_y;
        let hdr = map.hit_test(layout.columns[0].rect.x + 1, hdr_y);
        // The header cell may resolve to the header OR the new-button if they
        // overlap; assert it is at least the Backlog column.
        assert_eq!(
            map.column_at(layout.columns[0].rect.x + 1, hdr_y),
            Some(IssueLifecycle::Backlog)
        );
        assert!(matches!(
            hdr,
            Some(
                Target::ColumnHeader(IssueLifecycle::Backlog)
                    | Target::NewButton(IssueLifecycle::Backlog)
            )
        ));
        // The `+` affordance resolves to a NewButton for Backlog.
        if let Some(plus_x) = layout.columns[0].create_affordance_x {
            assert_eq!(
                map.hit_test(plus_x, hdr_y),
                Some(&Target::NewButton(IssueLifecycle::Backlog))
            );
        }
        // The second column resolves to Todo.
        assert_eq!(
            map.column_at(layout.columns[1].rect.x + 1, layout.columns[1].header_y),
            Some(IssueLifecycle::Todo)
        );
    }

    /// A hit-test resolves a card sitting inside a drop zone to the CARD, not the
    /// column background it overlaps (priority ordering).
    #[test]
    fn card_wins_over_overlapping_drop_zone() {
        let mut map = HitMap::default();
        map.push(
            Rect::new(0, 2, 20, 18),
            Target::DropZone(IssueLifecycle::Todo),
        );
        map.push(Rect::new(0, 3, 20, 6), Target::Card("A".into()));
        assert_eq!(map.hit_test(5, 5), Some(&Target::Card("A".into())));
        // A point in the drop zone but outside the card resolves to the zone.
        assert_eq!(
            map.hit_test(5, 15),
            Some(&Target::DropZone(IssueLifecycle::Todo))
        );
    }

    /// A new-button sitting inside the header row resolves to the button, not the
    /// header.
    #[test]
    fn new_button_wins_over_overlapping_header() {
        let mut map = HitMap::default();
        map.push(
            Rect::new(0, 1, 20, 1),
            Target::ColumnHeader(IssueLifecycle::Todo),
        );
        map.push(
            Rect::new(18, 1, 2, 1),
            Target::NewButton(IssueLifecycle::Todo),
        );
        assert_eq!(
            map.hit_test(18, 1),
            Some(&Target::NewButton(IssueLifecycle::Todo))
        );
        assert_eq!(
            map.hit_test(5, 1),
            Some(&Target::ColumnHeader(IssueLifecycle::Todo))
        );
    }

    /// `column_at` resolves the lifecycle of any column-scoped target under the
    /// point, and `None` off every column.
    #[test]
    fn column_at_resolves_column_membership() {
        let mut map = HitMap::default();
        map.push(
            Rect::new(0, 2, 20, 18),
            Target::DropZone(IssueLifecycle::InProgress),
        );
        assert_eq!(map.column_at(5, 5), Some(IssueLifecycle::InProgress));
        assert_eq!(map.column_at(50, 50), None);
    }

    /// The reorder index is the pointer row offset into the column body.
    #[test]
    fn reorder_index_is_row_offset_into_body() {
        let mut map = HitMap::default();
        // Body starts at row 2.
        map.push(
            Rect::new(0, 2, 20, 18),
            Target::DropZone(IssueLifecycle::Todo),
        );
        assert_eq!(reorder_index(&map, IssueLifecycle::Todo, 2), 0);
        assert_eq!(reorder_index(&map, IssueLifecycle::Todo, 9), 7);
        // A row above the body top saturates to 0.
        assert_eq!(reorder_index(&map, IssueLifecycle::Todo, 1), 0);
    }
}
