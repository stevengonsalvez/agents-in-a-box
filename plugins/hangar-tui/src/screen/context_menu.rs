//! 63l.5 — the right-click context-menu overlay: a small floating menu anchored
//! at the click cell, with cascading submenus, whose leaf items dispatch the real
//! daemon RPCs.
//!
//! ## The shape of the feature
//!
//! P0.2's mouse framework emits [`MouseIntent::OpenContextMenu`] on a
//! `Down{Right}` over a card. This module is the overlay that intent raises: a
//! Linear-style action menu on the card. It is **pure** like every other Hangar
//! screen reducer — it folds keys/clicks into a new state plus an optional
//! [`ContextMenuIntent`] the plugin glue binds to an RPC.
//!
//! ```text
//!  Down{Right}            ┌──────────────────┐
//!  on card B ───────────▶ │ Open             │
//!  (P0.2 intent)          │ Move to        ▸ │──┐  ┌──────────────┐
//!                         │ Priority       ▸ │  └─▶│ Backlog      │
//!                         │ Assign         ▸ │     │ Todo         │
//!                         │ Copy id          │     │ In Progress  │── issue_update
//!                         │ Delete  (greyed) │     │ In Review    │   {state}
//!                         └──────────────────┘     │ Done         │
//!                                                  └──────────────┘
//! ```
//!
//! ## Wiring (where the RPCs land)
//!
//! Each leaf folds to a [`ContextMenuIntent`]:
//! - `Move to ▸ <status>` → [`ContextMenuIntent::MoveTo`] → `hangar/issue_update{state}`.
//! - `Priority ▸ <chip>` → [`ContextMenuIntent::SetPriority`] → `hangar/issue_update{priority}`.
//! - `Assign ▸ <actor>` → [`ContextMenuIntent::Assign`] → `hangar/issue_update{assignee}`.
//! - `Open` → [`ContextMenuIntent::Open`] → the keyboard task-open path.
//! - `Copy id` → [`ContextMenuIntent::CopyId`] → a copied-confirmation (no host
//!   clipboard cap exists yet, so the menu shows a transient `copied` note rather
//!   than touching the OS clipboard).
//! - `Delete` is **greyed** — no issue-delete RPC exists in the daemon yet
//!   (`hangar/issue_update` mutates fields; there is no destructive
//!   `hangar/issue_delete`). The item paints muted and a footer note explains it,
//!   rather than dispatching nothing silently or pretending to delete.
//!
//! ## Navigation
//!
//! Keyboard: `↑`/`↓` (and `k`/`j`) move the selection, `→`/`Enter` open a submenu
//! or fire a leaf, `←`/`Esc` close the submenu (or the whole menu at the root).
//! Mouse: a `Down{Left}` on a painted row is hit-tested against the render-time
//! [`ContextMenuHitMap`] and folds to the same reduction a keyboard select would —
//! the paint is the single source of truth for where the rows landed (the same
//! discipline the card-board mouse layer uses).

use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::mouse::Rect;
use crate::widgets::card_board::PriorityChip;

/// Cornflower-blue menu border (the ainb-tui chrome accent the modals share).
const BORDER: Color = Color::rgb(100, 149, 237);
/// Gold menu title (the `HGR-<n>` heading).
const TITLE: Color = Color::rgb(255, 215, 0);
/// Soft-white item text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Highlighted (selected) item text.
const SELECTED: Color = Color::rgb(255, 255, 255);
/// Highlighted-item background fill so the cursor row reads as raised.
const SELECTED_BG: Color = Color::rgb(60, 70, 110);
/// Muted text for the disabled `Delete` item and the footer note.
const DISABLED: Color = Color::rgb(120, 120, 135);
/// A marker accent flagging the issue's CURRENT state / priority in a submenu so
/// the user sees what is set before changing it.
const CURRENT: Color = Color::rgb(150, 200, 150);
/// Dim backdrop fill behind the menu box so it reads as a floating surface.
const BACKDROP: Color = Color::rgb(24, 26, 34);

/// The four priority chips offered in the `Priority ▸` submenu, mapped to the
/// wire `priority` scalar each round-trips through [`PriorityChip::from_priority`]
/// (`0..3`, higher = more urgent). The wire scale has four distinct buckets, so we
/// list the four faithful chips the board itself paints
/// (`Urgent`/`High`/`Medium`/`None`), most-urgent first, so a pick always sends a
/// value the daemon round-trips back to the same chip.
const PRIORITY_CHOICES: [(PriorityChip, i64); 4] = [
    (PriorityChip::Urgent, 3),
    (PriorityChip::High, 2),
    (PriorityChip::Medium, 1),
    (PriorityChip::None, 0),
];

/// The root menu rows, in paint order.
///
/// A dedicated enum (rather than a bare index) so the reducer reads which row is
/// selected without a magic number, and a future reordering of the menu can't
/// silently re-bind an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootItem {
    /// Open the issue's task detail (the keyboard `enter` path).
    Open,
    /// Move-to submenu (the five lifecycle statuses).
    MoveTo,
    /// Priority submenu (the four faithful chips).
    Priority,
    /// Assign submenu (the workspace actors).
    Assign,
    /// Copy the `HGR-<n>` display id (a copied-confirmation note).
    CopyId,
    /// Delete — greyed (no daemon issue-delete RPC exists yet).
    Delete,
}

impl RootItem {
    /// The root rows in paint order.
    const ALL: [Self; 6] = [
        Self::Open,
        Self::MoveTo,
        Self::Priority,
        Self::Assign,
        Self::CopyId,
        Self::Delete,
    ];

    /// The row label (without the `▸` submenu caret, added at paint time).
    const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::MoveTo => "Move to",
            Self::Priority => "Priority",
            Self::Assign => "Assign",
            Self::CopyId => "Copy id",
            Self::Delete => "Delete",
        }
    }

    /// Whether this row opens a cascading submenu (painted with a `▸` caret).
    const fn has_submenu(self) -> bool {
        matches!(self, Self::MoveTo | Self::Priority | Self::Assign)
    }

    /// Whether this row is selectable. `Delete` is disabled (no delete RPC yet).
    const fn is_enabled(self) -> bool {
        !matches!(self, Self::Delete)
    }
}

/// A one-row selection step direction, with index arithmetic that saturates at
/// the list edges (so navigation never under/overflows a `usize` index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Toward index `0`.
    Up,
    /// Toward `max`.
    Down,
}

impl Step {
    /// The next index from `idx` in this direction, or `None` at the edge (`0`
    /// going up, `max` going down). `max` is the last valid index.
    const fn apply(self, idx: usize, max: usize) -> Option<usize> {
        match self {
            Self::Up => idx.checked_sub(1),
            Self::Down if idx < max => Some(idx + 1),
            Self::Down => None,
        }
    }
}

/// Which submenu (if any) is currently open, with its own selection index.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SubMenu {
    /// No submenu open — navigation is on the root list.
    None,
    /// The Move-to submenu is open; selection indexes [`IssueLifecycle::ALL`].
    MoveTo(usize),
    /// The Priority submenu is open; selection indexes [`PRIORITY_CHOICES`].
    Priority(usize),
    /// The Assign submenu is open; selection indexes the cached actor list.
    Assign(usize),
}

/// A leaf action the plugin glue binds to a real daemon RPC after a reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuIntent {
    /// Open the issue's task detail (reuses the keyboard task-open path).
    Open {
        /// The issue id the menu was raised for.
        issue_id: String,
    },
    /// Move the issue to a new lifecycle status — `hangar/issue_update{state}`.
    MoveTo {
        /// The issue id the menu was raised for.
        issue_id: String,
        /// The destination lifecycle status.
        to_status: IssueLifecycle,
    },
    /// Set the issue's priority — `hangar/issue_update{priority}`.
    SetPriority {
        /// The issue id the menu was raised for.
        issue_id: String,
        /// The wire `priority` scalar (`0..3`, higher = more urgent).
        priority: i64,
    },
    /// Assign an actor to the issue — `hangar/issue_update{assignee}`.
    Assign {
        /// The issue id the menu was raised for.
        issue_id: String,
        /// The picked actor in canonical `member:<id>` / `agent:<id>` form.
        actor_ref: String,
    },
    /// Copy the issue's `HGR-<n>` display id (a copied-confirmation; no host
    /// clipboard cap exists yet, so the menu only flashes a `copied` note).
    CopyId {
        /// The `HGR-<n>` display id copied.
        display_id: String,
    },
}

/// A flat actor row the Assign submenu paints.
///
/// The subset of the cached `hangar/agents_list` snapshot the menu needs
/// (`actor_ref` to dispatch, `display_name` to paint). Kept local so the menu
/// never depends on the wire row's full shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuActor {
    /// The canonical `member:<id>` / `agent:<id>` ref dispatched on pick.
    pub actor_ref: String,
    /// The display name painted in the submenu row.
    pub display_name: String,
}

/// A normalized menu navigation key the glue forwards.
///
/// The plugin maps the SDK arrow [`KeyCode`](ainb_plugin_sdk::KeyCode)s and the
/// `h`/`j`/`k`/`l` vim chars onto these before calling
/// [`ContextMenuState::handle_key`], so the reducer has one small input alphabet
/// regardless of how the user navigated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuKey {
    /// `↑` / `k` — move selection up.
    Up,
    /// `↓` / `j` — move selection down.
    Down,
    /// `→` / `l` — open the selected submenu (or fire a leaf).
    Right,
    /// `Enter` — open a submenu or fire the selected leaf.
    Enter,
    /// `←` / `h` — collapse an open submenu (no-op at the root).
    Left,
    /// `Esc` — close the submenu, else the whole menu.
    Esc,
}

/// The render-time hit-map for the menu.
///
/// Each painted, selectable row is tagged with the cursor target a `Down{Left}`
/// resolves to. Built by [`render_context_menu`] and stored on the state so
/// [`ContextMenuState::handle_click`] hit-tests against exactly what was painted
/// (no re-derived geometry).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextMenuHitMap {
    root: Vec<(Rect, usize)>,
    sub: Vec<(Rect, usize)>,
}

impl ContextMenuHitMap {
    /// Record a painted root row at `rect` resolving to root index `i`.
    fn push_root(&mut self, rect: Rect, i: usize) {
        self.root.push((rect, i));
    }

    /// Record a painted submenu row at `rect` resolving to submenu index `i`.
    fn push_sub(&mut self, rect: Rect, i: usize) {
        self.sub.push((rect, i));
    }

    /// The root index whose painted row covers `(col, row)`, if any.
    fn root_at(&self, col: u16, row: u16) -> Option<usize> {
        self.root
            .iter()
            .find_map(|(rect, i)| rect.contains(col, row).then_some(*i))
    }

    /// The submenu index whose painted row covers `(col, row)`, if any.
    fn sub_at(&self, col: u16, row: u16) -> Option<usize> {
        self.sub
            .iter()
            .find_map(|(rect, i)| rect.contains(col, row).then_some(*i))
    }
}

/// The context-menu overlay state.
///
/// Holds the issue the menu was raised for (id + display id + its current
/// state/priority so submenus can mark what is set), the anchor cell, the cached
/// actor list for the Assign submenu, the navigation cursor (root selection +
/// open submenu), a transient `copied` flag, a `closed` flag the glue reads to
/// dismiss the overlay, and the last render's hit-map for mouse clicks. All
/// fields are private; the renderer and tests read through accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMenuState {
    issue_id: String,
    display_id: String,
    current_status: IssueLifecycle,
    current_priority: i64,
    anchor: (u16, u16),
    actors: Vec<MenuActor>,
    root_selected: usize,
    submenu: SubMenu,
    copied: bool,
    closed: bool,
    hit_map: ContextMenuHitMap,
}

impl ContextMenuState {
    /// Open a fresh menu for `issue_id` (display `display_id`) anchored at `at`,
    /// over `actors` (the cached actor snapshot for the Assign submenu).
    /// `current_status` / `current_priority` mark the issue's current values in
    /// the submenus so a change is visible against what is set. The first
    /// enabled root row (`Open`) is selected.
    #[must_use]
    pub fn new(
        issue_id: impl Into<String>,
        display_id: impl Into<String>,
        current_status: IssueLifecycle,
        current_priority: i64,
        at: (u16, u16),
        actors: Vec<MenuActor>,
    ) -> Self {
        Self {
            issue_id: issue_id.into(),
            display_id: display_id.into(),
            current_status,
            current_priority,
            anchor: at,
            actors,
            root_selected: 0,
            submenu: SubMenu::None,
            copied: false,
            closed: false,
            hit_map: ContextMenuHitMap::default(),
        }
    }

    /// The issue id the menu was raised for.
    #[must_use]
    pub fn issue_id(&self) -> &str {
        &self.issue_id
    }

    /// Whether the menu has been closed (Esc at the root, or a leaf fired).
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether the last action copied the display id (drives the `copied` note).
    #[must_use]
    pub const fn copied(&self) -> bool {
        self.copied
    }

    /// The 0-based root selection index (for the render + tests).
    #[must_use]
    pub const fn root_selected(&self) -> usize {
        self.root_selected
    }

    /// The currently-selected root item.
    #[must_use]
    fn selected_root(&self) -> RootItem {
        RootItem::ALL[self.root_selected.min(RootItem::ALL.len() - 1)]
    }

    /// The visible row count of the open submenu, or `0` at the root.
    const fn submenu_len(&self) -> usize {
        match self.submenu {
            SubMenu::None => 0,
            SubMenu::MoveTo(_) => IssueLifecycle::ALL.len(),
            SubMenu::Priority(_) => PRIORITY_CHOICES.len(),
            SubMenu::Assign(_) => self.actors.len(),
        }
    }

    /// Fold one normalized menu key, returning the leaf intent it fired (if any).
    ///
    /// `↑`/`↓` move the selection (in the open submenu, else the root list, with
    /// the `Delete` row skipped); `→`/`Enter` open the selected submenu or fire a
    /// leaf; `←` collapses an open submenu (no-op at the root); `Esc` collapses an
    /// open submenu, else closes the whole menu.
    pub fn handle_key(&mut self, key: ContextMenuKey) -> Option<ContextMenuIntent> {
        // Any keypress clears a stale copied-confirmation note.
        self.copied = false;
        match key {
            ContextMenuKey::Up => {
                self.move_selection(Step::Up);
                None
            }
            ContextMenuKey::Down => {
                self.move_selection(Step::Down);
                None
            }
            ContextMenuKey::Right | ContextMenuKey::Enter => self.activate(),
            ContextMenuKey::Left => {
                self.collapse_submenu();
                None
            }
            ContextMenuKey::Esc => {
                if matches!(self.submenu, SubMenu::None) {
                    self.closed = true;
                } else {
                    self.collapse_submenu();
                }
                None
            }
        }
    }

    /// Fold a left-click at `(col, row)` against the last render's hit-map.
    ///
    /// A click on a painted submenu row selects + fires it (same as Enter on that
    /// row). A click on a painted root row selects it, then opens its submenu or
    /// fires its leaf (so a single click on `Open` opens, and a single click on
    /// `Move to` cascades the submenu). A click off every painted row closes the
    /// menu (the conventional click-away dismiss). Returns the fired leaf intent.
    pub fn handle_click(&mut self, col: u16, row: u16) -> Option<ContextMenuIntent> {
        self.copied = false;
        if let Some(i) = self.hit_map.sub_at(col, row) {
            self.set_submenu_selection(i);
            return self.activate();
        }
        if let Some(i) = self.hit_map.root_at(col, row) {
            self.root_selected = i;
            // A click on a disabled row (Delete) is inert but keeps the menu open.
            if !self.selected_root().is_enabled() {
                return None;
            }
            self.submenu = SubMenu::None;
            return self.activate();
        }
        // Click-away: dismiss.
        self.closed = true;
        None
    }

    /// Activate the current cursor: open the selected root submenu, or fire the
    /// selected leaf (a root leaf, or the selected submenu row).
    fn activate(&mut self) -> Option<ContextMenuIntent> {
        match self.submenu {
            SubMenu::None => self.activate_root(),
            SubMenu::MoveTo(i) => {
                let to_status = IssueLifecycle::ALL[i.min(IssueLifecycle::ALL.len() - 1)];
                self.closed = true;
                Some(ContextMenuIntent::MoveTo {
                    issue_id: self.issue_id.clone(),
                    to_status,
                })
            }
            SubMenu::Priority(i) => {
                let (_, priority) = PRIORITY_CHOICES[i.min(PRIORITY_CHOICES.len() - 1)];
                self.closed = true;
                Some(ContextMenuIntent::SetPriority {
                    issue_id: self.issue_id.clone(),
                    priority,
                })
            }
            SubMenu::Assign(i) => match self.actors.get(i).cloned() {
                Some(actor) => {
                    self.closed = true;
                    Some(ContextMenuIntent::Assign {
                        issue_id: self.issue_id.clone(),
                        actor_ref: actor.actor_ref,
                    })
                }
                // No actors cached — keep the (empty) submenu open rather than
                // firing a bogus assign.
                None => None,
            },
        }
    }

    /// Activate the selected ROOT row: open its submenu or fire its leaf.
    fn activate_root(&mut self) -> Option<ContextMenuIntent> {
        match self.selected_root() {
            RootItem::Open => {
                self.closed = true;
                Some(ContextMenuIntent::Open {
                    issue_id: self.issue_id.clone(),
                })
            }
            RootItem::MoveTo => {
                // Pre-select the issue's CURRENT status so a stray Enter is a no-op
                // move, not a silent jump to Backlog.
                self.submenu = SubMenu::MoveTo(self.current_status.order());
                None
            }
            RootItem::Priority => {
                let cur = PRIORITY_CHOICES
                    .iter()
                    .position(|(_, p)| *p == self.current_priority)
                    .unwrap_or(0);
                self.submenu = SubMenu::Priority(cur);
                None
            }
            RootItem::Assign => {
                self.submenu = SubMenu::Assign(0);
                None
            }
            RootItem::CopyId => {
                // No host clipboard cap — flash a copied-confirmation note. The
                // menu stays open so the note is visible; the glue still records
                // the CopyId intent (it logs the copy seam for later wiring).
                self.copied = true;
                Some(ContextMenuIntent::CopyId {
                    display_id: self.display_id.clone(),
                })
            }
            // Disabled — inert.
            RootItem::Delete => None,
        }
    }

    /// Move the selection one row in `step`'s direction in the open submenu, else
    /// the root list (skipping the disabled `Delete` root row).
    const fn move_selection(&mut self, step: Step) {
        if matches!(self.submenu, SubMenu::None) {
            self.move_root_selection(step);
        } else {
            self.move_submenu_selection(step);
        }
    }

    /// Step the root selection to the next ENABLED row in `step`'s direction, so
    /// the cursor never rests on the disabled `Delete` row. Stays put when there
    /// is no enabled row further in that direction (a clamp at the edge).
    const fn move_root_selection(&mut self, step: Step) {
        let last = RootItem::ALL.len() - 1;
        let mut idx = self.root_selected;
        loop {
            let Some(next) = step.apply(idx, last) else {
                // Hit the edge with no enabled row — keep the prior selection.
                return;
            };
            idx = next;
            if RootItem::ALL[idx].is_enabled() {
                self.root_selected = idx;
                return;
            }
        }
    }

    /// Step the open submenu's selection one row in `step`'s direction, clamped to
    /// its row count.
    const fn move_submenu_selection(&mut self, step: Step) {
        let len = self.submenu_len();
        if len == 0 {
            return;
        }
        let cur = self.current_submenu_index();
        if let Some(next) = step.apply(cur, len - 1) {
            self.set_submenu_selection(next);
        }
    }

    /// The selection index of the open submenu (0 at the root).
    const fn current_submenu_index(&self) -> usize {
        match self.submenu {
            SubMenu::None => 0,
            SubMenu::MoveTo(i) | SubMenu::Priority(i) | SubMenu::Assign(i) => i,
        }
    }

    /// Point the open submenu's selection at `i` (clamped by the caller).
    const fn set_submenu_selection(&mut self, i: usize) {
        self.submenu = match self.submenu {
            SubMenu::None => SubMenu::None,
            SubMenu::MoveTo(_) => SubMenu::MoveTo(i),
            SubMenu::Priority(_) => SubMenu::Priority(i),
            SubMenu::Assign(_) => SubMenu::Assign(i),
        };
    }

    /// Collapse an open submenu back to the root list (no-op at the root).
    const fn collapse_submenu(&mut self) {
        self.submenu = SubMenu::None;
    }
}

// ---------------------------------------------------------------------------
// Anchored render
// ---------------------------------------------------------------------------

/// Menu box width (the root column), wide enough for `Priority ▸` plus padding.
const MENU_W: u16 = 20;
/// Submenu box width, wide enough for `In Progress ✓` plus padding.
const SUB_W: u16 = 18;

/// Render the menu anchored near its click cell, recording a fresh hit-map onto
/// `state` so the next click hit-tests against exactly these rows.
///
/// The root box is clamped to stay on-screen (it flips left of the anchor when it
/// would overflow the right edge, and up when it would overflow the bottom). An
/// open submenu cascades to the right of the root (flipping left when it would
/// overflow). The selected row in each box paints with a filled background so the
/// cursor is visible; the disabled `Delete` row paints muted; a submenu marks the
/// issue's current value with a `✓`.
pub fn render_context_menu(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    state: &mut ContextMenuState,
) {
    let mut hit_map = ContextMenuHitMap::default();
    let (ax, ay) = state.anchor;

    // Root box geometry: +2 rows for the top/bottom border, +1 for the title row.
    let root_rows = u16::try_from(RootItem::ALL.len()).unwrap_or(u16::MAX);
    let root_h = root_rows + 3;
    let root_x = clamp_left(ax, MENU_W, area_w);
    let root_y = clamp_top(ay, root_h, area_h);
    draw_box(buf, root_x, root_y, MENU_W, root_h);
    put_str(
        buf,
        root_x + 2,
        root_y,
        &state.display_id,
        TITLE,
        root_x + MENU_W - 1,
    );

    // Root rows below the title row.
    let first_row = root_y + 2;
    for (i, item) in RootItem::ALL.iter().enumerate() {
        let row = first_row + u16::try_from(i).unwrap_or(0);
        let caret = if item.has_submenu() { "▸" } else { " " };
        paint_row(
            buf,
            &MenuRow {
                box_x: root_x,
                box_w: MENU_W,
                row,
                label: item.label(),
                trailing: caret,
                selected: i == state.root_selected,
                enabled: item.is_enabled(),
            },
        );
        hit_map.push_root(Rect::new(root_x + 1, row, MENU_W - 2, 1), i);
    }

    // The greyed-Delete footer note, on the bottom border row.
    put_str(
        buf,
        root_x + 1,
        root_y + root_h - 1,
        " delete: no RPC ",
        DISABLED,
        root_x + MENU_W - 1,
    );

    // The copied-confirmation note replaces the title-row tail when armed.
    if state.copied {
        put_str(
            buf,
            root_x + MENU_W - 9,
            root_y,
            " copied ",
            CURRENT,
            root_x + MENU_W,
        );
    }

    // The cascading submenu (to the right of the root, flipping left on overflow).
    render_submenu(buf, area_w, area_h, state, root_x, first_row, &mut hit_map);

    state.hit_map = hit_map;
}

/// Render the open submenu (if any), recording its rows into `hit_map`.
fn render_submenu(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    state: &ContextMenuState,
    root_x: u16,
    root_first_row: u16,
    hit_map: &mut ContextMenuHitMap,
) {
    // Each submenu row is `(label, is_current)`: the label to paint and whether it
    // is the issue's CURRENT state/priority (marked with a ✓).
    let rows: Vec<(String, bool)> = match state.submenu {
        SubMenu::None => return,
        SubMenu::MoveTo(_) => IssueLifecycle::ALL
            .iter()
            .map(|s| (s.label().to_string(), *s == state.current_status))
            .collect(),
        SubMenu::Priority(_) => PRIORITY_CHOICES
            .iter()
            .map(|(chip, p)| (chip.label().to_string(), *p == state.current_priority))
            .collect(),
        SubMenu::Assign(_) => state
            .actors
            .iter()
            .map(|a| (a.display_name.clone(), false))
            .collect(),
    };

    let sub_sel = state.current_submenu_index();
    let sub_h = u16::try_from(rows.len()).unwrap_or(0) + 2;
    // Cascade right of the root box; flip left when it would overflow.
    let sub_x = if root_x + MENU_W + SUB_W <= area_w {
        root_x + MENU_W
    } else {
        root_x.saturating_sub(SUB_W)
    };
    let sub_y = clamp_top(root_first_row, sub_h, area_h);
    draw_box(buf, sub_x, sub_y, SUB_W, sub_h);

    for (idx, (label, is_current)) in rows.iter().enumerate() {
        let row = sub_y + 1 + u16::try_from(idx).unwrap_or(0);
        let marker = if *is_current { "✓" } else { " " };
        paint_row(
            buf,
            &MenuRow {
                box_x: sub_x,
                box_w: SUB_W,
                row,
                label,
                trailing: marker,
                selected: idx == sub_sel,
                enabled: true,
            },
        );
        hit_map.push_sub(Rect::new(sub_x + 1, row, SUB_W - 2, 1), idx);
    }
}

/// One paintable menu row: its box origin/width, the screen row, the label, the
/// trailing caret/marker glyph, and the selected/enabled flags. Bundled so the
/// painter takes one descriptor rather than a long positional argument list.
struct MenuRow<'a> {
    /// Left x of the box the row sits in.
    box_x: u16,
    /// The box's width.
    box_w: u16,
    /// The screen row the entry paints on.
    row: u16,
    /// The row label.
    label: &'a str,
    /// The trailing caret (`▸`) / current-marker (`✓`) glyph, or a blank.
    trailing: &'a str,
    /// Whether this row is the cursor selection (gets a highlight background).
    selected: bool,
    /// Whether this row is enabled (a disabled row paints muted).
    enabled: bool,
}

/// Paint one menu row inside a box: a backdrop fill, the label, and a trailing
/// caret/marker glyph one cell in from the right border. The selected row gets a
/// highlight background; a disabled row paints muted.
fn paint_row(buf: &mut WireBuffer, r: &MenuRow) {
    let inner_x = r.box_x + 1;
    let inner_right = r.box_x + r.box_w - 1;
    let bg = if r.selected { SELECTED_BG } else { BACKDROP };
    let fg = if !r.enabled {
        DISABLED
    } else if r.selected {
        SELECTED
    } else {
        TEXT
    };
    // Backdrop fill across the inner row.
    for cx in inner_x..inner_right {
        let mut cell = Cell::new(" ");
        cell.bg = Some(bg);
        buf.push(Coord::new(cx, r.row), cell);
    }
    // Label, clipped one cell before the trailing glyph.
    let label_right = inner_right.saturating_sub(2);
    let mut cx = inner_x + 1;
    for ch in r.label.chars() {
        if cx >= label_right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(fg);
        cell.bg = Some(bg);
        buf.push(Coord::new(cx, r.row), cell);
        cx = cx.saturating_add(1);
    }
    // Trailing caret/marker one cell in from the right border.
    if !r.trailing.trim().is_empty() {
        let mut cell = Cell::new(r.trailing.to_string());
        cell.fg = Some(if r.selected { SELECTED } else { CURRENT });
        cell.bg = Some(bg);
        buf.push(Coord::new(inner_right - 1, r.row), cell);
    }
}

/// The leftmost x a `w`-wide box can sit at so it stays on-screen, preferring the
/// anchor but flipping left of it when it would overflow the right edge.
const fn clamp_left(anchor_x: u16, w: u16, area_w: u16) -> u16 {
    if anchor_x + w <= area_w {
        anchor_x
    } else {
        area_w.saturating_sub(w)
    }
}

/// The topmost y a `h`-tall box can sit at so it stays on-screen, preferring the
/// anchor but flipping up when it would overflow the bottom edge.
const fn clamp_top(anchor_y: u16, h: u16, area_h: u16) -> u16 {
    if anchor_y + h <= area_h {
        anchor_y
    } else {
        area_h.saturating_sub(h)
    }
}

/// Draw a rounded `w` × `h` border box at `(x0, y0)` with a dim backdrop fill so
/// the box reads as a floating surface over the board.
fn draw_box(buf: &mut WireBuffer, x0: u16, y0: u16, w: u16, h: u16) {
    let x1 = x0 + w - 1;
    let y1 = y0 + h - 1;
    // Backdrop fill (interior) first so the border + text overwrite it.
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut cell = Cell::new(" ");
            cell.bg = Some(BACKDROP);
            buf.push(Coord::new(x, y), cell);
        }
    }
    for x in x0..=x1 {
        put_char(buf, x, y0, '─', BORDER);
        put_char(buf, x, y1, '─', BORDER);
    }
    for y in y0..=y1 {
        put_char(buf, x0, y, '│', BORDER);
        put_char(buf, x1, y, '│', BORDER);
    }
    put_char(buf, x0, y0, '╭', BORDER);
    put_char(buf, x1, y0, '╮', BORDER);
    put_char(buf, x0, y1, '╰', BORDER);
    put_char(buf, x1, y1, '╯', BORDER);
}

/// Write `s` at `(x, row)` in `color`, clipping at column `right`.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        cell.bg = Some(BACKDROP);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
}

/// Write one `ch` at `(x, row)` in `color` over the box backdrop.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    cell.bg = Some(BACKDROP);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the full painted text of the buffer (symbols concatenated) so a
    /// test can assert a label appears somewhere without pinning a cell.
    fn painted_text(buf: &WireBuffer) -> String {
        buf.cells.iter().map(|(_, c)| c.symbol.as_str()).collect()
    }

    fn actors() -> Vec<MenuActor> {
        vec![
            MenuActor {
                actor_ref: "member:alice".into(),
                display_name: "alice".into(),
            },
            MenuActor {
                actor_ref: "agent:claude".into(),
                display_name: "claude".into(),
            },
        ]
    }

    fn menu() -> ContextMenuState {
        ContextMenuState::new(
            "issue-b",
            "HGR-2",
            IssueLifecycle::Todo,
            0,
            (10, 5),
            actors(),
        )
    }

    /// Keyboard: open the Move-to submenu, step to In Progress, Enter — the menu
    /// fires a `MoveTo{in_progress}` for the raised issue and closes.
    #[test]
    fn move_to_in_progress_fires_move_intent_and_closes() {
        let mut m = menu();
        // Root cursor starts on `Open`; step down to `Move to`.
        assert_eq!(m.handle_key(ContextMenuKey::Down), None);
        // Open the submenu (pre-selects the current status, Todo).
        assert_eq!(m.handle_key(ContextMenuKey::Right), None);
        // Todo (order 1) -> In Progress (order 2): one step down.
        assert_eq!(m.handle_key(ContextMenuKey::Down), None);
        let fired = m.handle_key(ContextMenuKey::Enter);
        assert_eq!(
            fired,
            Some(ContextMenuIntent::MoveTo {
                issue_id: "issue-b".into(),
                to_status: IssueLifecycle::InProgress,
            }),
            "Move to > In Progress must fire issue_update with state=in_progress"
        );
        assert!(m.is_closed(), "firing a leaf closes the menu");
    }

    /// Keyboard: open the Priority submenu, pick High — fires
    /// `SetPriority{priority:2}` (the wire scalar `High` round-trips to).
    #[test]
    fn priority_high_fires_set_priority_intent() {
        let mut m = menu();
        // Open(0) -> Move to(1) -> Priority(2): two steps down.
        m.handle_key(ContextMenuKey::Down);
        m.handle_key(ContextMenuKey::Down);
        // Open the Priority submenu (current priority 0 -> None preselected, idx 3).
        m.handle_key(ContextMenuKey::Right);
        // Step up to High: None(3) -> Medium(2) -> High(1): two steps up.
        m.handle_key(ContextMenuKey::Up);
        m.handle_key(ContextMenuKey::Up);
        let fired = m.handle_key(ContextMenuKey::Enter);
        assert_eq!(
            fired,
            Some(ContextMenuIntent::SetPriority {
                issue_id: "issue-b".into(),
                priority: 2,
            }),
            "Priority > High must fire issue_update with priority=2"
        );
    }

    /// Keyboard: open the Assign submenu, Enter on the first actor — fires an
    /// `Assign{actor_ref}` for that actor.
    #[test]
    fn assign_fires_assign_intent_with_actor_ref() {
        let mut m = menu();
        // Open(0) -> Move to(1) -> Priority(2) -> Assign(3): three steps down.
        m.handle_key(ContextMenuKey::Down);
        m.handle_key(ContextMenuKey::Down);
        m.handle_key(ContextMenuKey::Down);
        m.handle_key(ContextMenuKey::Right);
        let fired = m.handle_key(ContextMenuKey::Enter);
        assert_eq!(
            fired,
            Some(ContextMenuIntent::Assign {
                issue_id: "issue-b".into(),
                actor_ref: "member:alice".into(),
            }),
            "Assign > alice must fire issue_update with assignee=member:alice"
        );
    }

    /// Keyboard: Enter on the root `Open` row fires the Open intent.
    #[test]
    fn open_root_fires_open_intent() {
        let mut m = menu();
        let fired = m.handle_key(ContextMenuKey::Enter);
        assert_eq!(
            fired,
            Some(ContextMenuIntent::Open {
                issue_id: "issue-b".into(),
            }),
        );
        assert!(m.is_closed());
    }

    /// Keyboard: Copy id fires a [`ContextMenuIntent::CopyId`] with the display id
    /// and flashes the copied-confirmation note (the menu stays open so the note is
    /// visible).
    #[test]
    fn copy_id_fires_copy_intent_and_flashes_note() {
        let mut m = menu();
        // Open(0) -> ... -> Copy id(4): four steps down.
        for _ in 0..4 {
            m.handle_key(ContextMenuKey::Down);
        }
        let fired = m.handle_key(ContextMenuKey::Enter);
        assert_eq!(
            fired,
            Some(ContextMenuIntent::CopyId {
                display_id: "HGR-2".into(),
            })
        );
        assert!(m.copied(), "Copy id flashes the copied-confirmation note");
        assert!(
            !m.is_closed(),
            "Copy id keeps the menu open to show the note"
        );
    }

    /// Keyboard: the cursor skips the disabled `Delete` row — stepping down past
    /// `Copy id` (index 4) cannot land on `Delete` (index 5), and the disabled
    /// row fires nothing when activated directly.
    #[test]
    fn delete_row_is_disabled_and_unreachable() {
        let mut m = menu();
        for _ in 0..10 {
            m.handle_key(ContextMenuKey::Down);
        }
        // The selection clamps at `Copy id` (4), never `Delete` (5).
        assert_eq!(
            m.root_selected(),
            4,
            "cursor stops above the disabled Delete"
        );
        // The Delete row, even when force-selected, fires no intent (no RPC).
        m.root_selected = 5;
        assert_eq!(m.handle_key(ContextMenuKey::Enter), None);
        assert!(!m.is_closed(), "activating the disabled Delete is inert");
    }

    /// Keyboard: Esc inside an open submenu collapses back to the root; Esc at the
    /// root closes the whole menu.
    #[test]
    fn esc_collapses_submenu_then_closes_menu() {
        let mut m = menu();
        m.handle_key(ContextMenuKey::Down); // Move to
        m.handle_key(ContextMenuKey::Right); // open submenu
        m.handle_key(ContextMenuKey::Esc); // collapse
        assert!(!m.is_closed(), "Esc in a submenu collapses, not closes");
        assert!(matches!(m.submenu, SubMenu::None));
        m.handle_key(ContextMenuKey::Esc); // close at root
        assert!(m.is_closed(), "Esc at the root closes the menu");
    }

    /// Mouse: a left-click on the painted `Move to` root row opens the submenu,
    /// and a click on the In Progress submenu row fires the move intent.
    #[test]
    fn click_path_opens_submenu_and_fires_move() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = menu();
        render_context_menu(&mut buf, 80, 24, &mut m);

        // Click the `Move to` root row (index 1).
        let move_to_rect = m.hit_map.root[1].0;
        let fired = m.handle_click(move_to_rect.x, move_to_rect.y);
        assert_eq!(
            fired, None,
            "clicking a submenu-parent opens it, fires nothing"
        );

        // Re-render so the submenu's hit-map is recorded, then click In Progress.
        render_context_menu(&mut buf, 80, 24, &mut m);
        // In Progress is submenu index 2 (Backlog, Todo, In Progress, ...).
        let in_progress_rect = m
            .hit_map
            .sub
            .iter()
            .find(|(_, i)| *i == 2)
            .map(|(r, _)| *r)
            .expect("In Progress submenu row painted");
        let fired = m.handle_click(in_progress_rect.x, in_progress_rect.y);
        assert_eq!(
            fired,
            Some(ContextMenuIntent::MoveTo {
                issue_id: "issue-b".into(),
                to_status: IssueLifecycle::InProgress,
            }),
            "clicking the In Progress submenu row fires the move RPC intent"
        );
    }

    /// Mouse: a click off every painted row dismisses the menu.
    #[test]
    fn click_away_dismisses_menu() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = menu();
        render_context_menu(&mut buf, 80, 24, &mut m);
        let fired = m.handle_click(79, 23);
        assert_eq!(fired, None);
        assert!(m.is_closed(), "a click off the menu dismisses it");
    }

    /// Render: the root box paints the issue display id title and every enabled
    /// item label, with a `▸` caret on the submenu rows.
    #[test]
    fn render_paints_root_items_and_carets() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = menu();
        render_context_menu(&mut buf, 80, 24, &mut m);
        let text = painted_text(&buf);
        for needle in ["HGR-2", "Open", "Move to", "Priority", "Assign", "Copy id"] {
            assert!(
                text.contains(needle),
                "the menu must paint `{needle}`; painted: {text}"
            );
        }
        assert!(text.contains('▸'), "submenu rows paint a ▸ caret");
    }

    /// Render: an open submenu paints its rows beside the root, with the issue's
    /// current status marked.
    #[test]
    fn render_paints_open_submenu_with_current_marker() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = menu();
        m.handle_key(ContextMenuKey::Down); // Move to
        m.handle_key(ContextMenuKey::Right); // open submenu
        render_context_menu(&mut buf, 80, 24, &mut m);
        let text = painted_text(&buf);
        for needle in ["Backlog", "Todo", "In Progress", "In Review", "Done"] {
            assert!(
                text.contains(needle),
                "the Move-to submenu must paint `{needle}`; painted: {text}"
            );
        }
        // The current status (Todo) is flagged with a ✓ marker.
        assert!(text.contains('✓'), "the current status is marked with a ✓");
    }

    /// Render: the greyed-Delete footer note is painted so the disabled item is
    /// explained, not silently inert.
    #[test]
    fn render_paints_delete_disabled_note() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = menu();
        render_context_menu(&mut buf, 80, 24, &mut m);
        assert!(
            painted_text(&buf).contains("delete: no RPC"),
            "the disabled Delete item must carry an explanatory note"
        );
    }

    /// Render: a menu anchored at the bottom-right corner flips on-screen rather
    /// than painting off the buffer.
    #[test]
    fn render_clamps_box_on_screen_near_corner() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = ContextMenuState::new(
            "issue-z",
            "HGR-9",
            IssueLifecycle::Done,
            0,
            (79, 23),
            actors(),
        );
        render_context_menu(&mut buf, 80, 24, &mut m);
        // Every painted cell must fall inside the 80x24 viewport.
        for (coord, _) in &buf.cells {
            assert!(
                coord.x < 80 && coord.y < 24,
                "cell ({}, {}) painted off-screen",
                coord.x,
                coord.y
            );
        }
    }
}
