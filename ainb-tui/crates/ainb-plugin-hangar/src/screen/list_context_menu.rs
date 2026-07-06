//! 63l.6 — a generic right-click context menu for the non-issue list screens
//! (Kanban / Autopilots / Skills).
//!
//! ## Why a second menu
//!
//! The issue board's [`context_menu`](super::context_menu) is irreducibly
//! issue-specific: every leaf dispatches `hangar/issue_update` (move-to a
//! lifecycle status, set a priority, assign an actor). The Kanban / Autopilots /
//! Skills screens have different real actions (run a task now, fire an autopilot,
//! enable/disable it) over their OWN existing daemon RPC seams.
//!
//! So rather than overload the issue menu, this is a flat, **action-list-driven**
//! menu that mirrors the same discipline — a small floating box anchored at the
//! click, keyboard + mouse navigable, whose leaf folds to a typed
//! [`ListMenuAction`] the plugin glue binds to the screen's EXISTING action. It
//! invents no new wire method; the action enum is just a tag the glue switches on
//! to fire the screen's already-wired RPC.
//!
//! ```text
//!  Down{Right}            ┌──────────────────┐
//!  on a card  ──────────▶ │ #abc123          │  ← card id title
//!  (board mouse intent)   │ Open             │
//!                         │ Run now          │── task_transition{running}
//!                         │ Cancel           │── task_transition{cancelled}
//!                         │ Copy id          │
//!                         └──────────────────┘
//! ```
//!
//! Pure + IO-free like every Hangar reducer: it folds keys/clicks into a new
//! state plus an optional [`ListMenuAction`] the plugin glue acts on.

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::mouse::Rect;

/// Cornflower-blue menu border (shared with the issue context menu + the modals).
const BORDER: Color = Color::rgb(100, 149, 237);
/// Gold menu title (the card id heading).
const TITLE: Color = Color::rgb(255, 215, 0);
/// Soft-white item text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Highlighted (selected) item text.
const SELECTED: Color = Color::rgb(255, 255, 255);
/// Highlighted-item background fill.
const SELECTED_BG: Color = Color::rgb(60, 70, 110);
/// Dim backdrop fill behind the menu box.
const BACKDROP: Color = Color::rgb(24, 26, 34);

/// The real action a leaf dispatches, bound by the plugin glue to the screen's
/// EXISTING daemon RPC seam (no new wire method).
///
/// The id the action carries is the card's id (task id / autopilot id / skill
/// slug); the glue maps each variant onto the matching pending-action the
/// `render` pass already drains and fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListMenuAction {
    /// Open the card's detail (the click-open path).
    Open(String),
    /// Run / fire the card now. For Kanban this is a `task_transition` to
    /// `running`; for Autopilots a `hangar/autopilot_fire_now`.
    RunNow(String),
    /// Cancel the card. For Kanban a `task_transition` to `cancelled`.
    Cancel(String),
    /// Toggle the card enabled/disabled (Autopilots `hangar/autopilot_set_enabled`,
    /// or a skill attach/detach). Carries the id + the target flag.
    SetEnabled {
        /// The card id.
        id: String,
        /// The target enabled state.
        enabled: bool,
    },
    /// Edit the card (Autopilots edit flow).
    Edit(String),
    /// Copy the card's id (a confirmation note; no host clipboard cap yet).
    CopyId(String),
}

/// One menu row: its visible label and the action it fires (or `None` for a
/// disabled / inert row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMenuItem {
    /// The row label painted in the menu.
    pub label: String,
    /// The action the row fires when activated, or `None` when the row is inert.
    pub action: Option<ListMenuAction>,
}

impl ListMenuItem {
    /// A row with a `label` that fires `action`.
    #[must_use]
    pub fn new(label: impl Into<String>, action: ListMenuAction) -> Self {
        Self {
            label: label.into(),
            action: Some(action),
        }
    }
}

/// A normalized menu navigation key the glue forwards (mirrors the issue menu's
/// alphabet so both menus navigate identically).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMenuKey {
    /// `↑` / `k` — move selection up.
    Up,
    /// `↓` / `j` — move selection down.
    Down,
    /// `Enter` / `→` / `l` — fire the selected row.
    Enter,
    /// `Esc` / `←` / `h` — close the menu.
    Esc,
}

/// The render-time hit-map: each painted row tagged with its index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ListHitMap {
    rows: Vec<(Rect, usize)>,
}

impl ListHitMap {
    fn push(&mut self, rect: Rect, i: usize) {
        self.rows.push((rect, i));
    }

    fn at(&self, col: u16, row: u16) -> Option<usize> {
        self.rows.iter().find_map(|(rect, i)| rect.contains(col, row).then_some(*i))
    }
}

/// The generic list context-menu overlay state.
///
/// Holds the card id title, the anchor cell, the menu items, the selection
/// cursor, a `closed` flag the glue reads to dismiss, a transient `copied` flag,
/// and the last render's hit-map. All fields private; the renderer + tests read
/// through accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListContextMenuState {
    title: String,
    anchor: (u16, u16),
    items: Vec<ListMenuItem>,
    selected: usize,
    closed: bool,
    copied: bool,
    hit_map: ListHitMap,
}

impl ListContextMenuState {
    /// Open a menu titled `title` (the card id) anchored at `at`, over `items`.
    /// The first row is selected.
    #[must_use]
    pub fn new(title: impl Into<String>, at: (u16, u16), items: Vec<ListMenuItem>) -> Self {
        Self {
            title: title.into(),
            anchor: at,
            items,
            selected: 0,
            closed: false,
            copied: false,
            hit_map: ListHitMap::default(),
        }
    }

    /// Whether the menu has been closed (a leaf fired or Esc/click-away).
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether the last action copied the card id (drives the `copied` note).
    #[must_use]
    pub const fn copied(&self) -> bool {
        self.copied
    }

    /// The 0-based selection index (for the render + tests).
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Fold one normalized menu key, returning the leaf action it fired (if any).
    pub fn handle_key(&mut self, key: ListMenuKey) -> Option<ListMenuAction> {
        self.copied = false;
        match key {
            ListMenuKey::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            ListMenuKey::Down => {
                let last = self.items.len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                None
            }
            ListMenuKey::Enter => self.activate(),
            ListMenuKey::Esc => {
                self.closed = true;
                None
            }
        }
    }

    /// Fold a left-click at `(col, row)` against the last render's hit-map: a click
    /// on a painted row selects + fires it; a click off every row closes the menu
    /// (the conventional click-away dismiss).
    pub fn handle_click(&mut self, col: u16, row: u16) -> Option<ListMenuAction> {
        self.copied = false;
        if let Some(i) = self.hit_map.at(col, row) {
            self.selected = i;
            return self.activate();
        }
        self.closed = true;
        None
    }

    /// Activate the selected row: fire its action, closing the menu (a `CopyId`
    /// keeps the menu open to show its confirmation note).
    fn activate(&mut self) -> Option<ListMenuAction> {
        let action = self.items.get(self.selected).and_then(|i| i.action.clone())?;
        if matches!(action, ListMenuAction::CopyId(_)) {
            self.copied = true;
        } else {
            self.closed = true;
        }
        Some(action)
    }
}

// ---------------------------------------------------------------------------
// Anchored render
// ---------------------------------------------------------------------------

/// Menu box width.
const MENU_W: u16 = 20;

/// Render the menu anchored near its click cell, recording a fresh hit-map onto
/// `state` so the next click hit-tests against exactly these rows. The box is
/// clamped to stay on-screen (flips left/up near an edge).
pub fn render_list_context_menu(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    state: &mut ListContextMenuState,
) {
    let mut hit_map = ListHitMap::default();
    let (ax, ay) = state.anchor;
    let rows = u16::try_from(state.items.len()).unwrap_or(u16::MAX);
    // +1 title row, +2 borders.
    let box_h = rows + 3;
    let box_x = clamp_left(ax, MENU_W, area_w);
    let box_y = clamp_top(ay, box_h, area_h);
    draw_box(buf, box_x, box_y, MENU_W, box_h);
    put_str(
        buf,
        box_x + 2,
        box_y,
        &state.title,
        TITLE,
        box_x + MENU_W - 1,
    );

    let first_row = box_y + 2;
    for (i, item) in state.items.iter().enumerate() {
        let row = first_row + u16::try_from(i).unwrap_or(0);
        paint_row(buf, box_x, MENU_W, row, &item.label, i == state.selected);
        hit_map.push(Rect::new(box_x + 1, row, MENU_W - 2, 1), i);
    }

    if state.copied {
        put_str(
            buf,
            box_x + MENU_W - 9,
            box_y,
            " copied ",
            Color::rgb(150, 200, 150),
            box_x + MENU_W,
        );
    }
    state.hit_map = hit_map;
}

/// Paint one menu row inside the box: a backdrop fill + the label, highlighted
/// when selected.
fn paint_row(buf: &mut WireBuffer, box_x: u16, box_w: u16, row: u16, label: &str, selected: bool) {
    let inner_x = box_x + 1;
    let inner_right = box_x + box_w - 1;
    let bg = if selected { SELECTED_BG } else { BACKDROP };
    let fg = if selected { SELECTED } else { TEXT };
    for cx in inner_x..inner_right {
        let mut cell = Cell::new(" ");
        cell.bg = Some(bg);
        buf.push(Coord::new(cx, row), cell);
    }
    let mut cx = inner_x + 1;
    for ch in label.chars() {
        if cx >= inner_right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(fg);
        cell.bg = Some(bg);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
}

/// The leftmost x a `w`-wide box can sit at so it stays on-screen.
const fn clamp_left(anchor_x: u16, w: u16, area_w: u16) -> u16 {
    if anchor_x + w <= area_w {
        anchor_x
    } else {
        area_w.saturating_sub(w)
    }
}

/// The topmost y a `h`-tall box can sit at so it stays on-screen.
const fn clamp_top(anchor_y: u16, h: u16, area_h: u16) -> u16 {
    if anchor_y + h <= area_h {
        anchor_y
    } else {
        area_h.saturating_sub(h)
    }
}

/// Draw a `w` × `h` border box at `(x0, y0)` with a dim backdrop fill.
fn draw_box(buf: &mut WireBuffer, x0: u16, y0: u16, w: u16, h: u16) {
    let x1 = x0 + w - 1;
    let y1 = y0 + h - 1;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut cell = Cell::new(" ");
            cell.bg = Some(BACKDROP);
            buf.push(Coord::new(x, y), cell);
        }
    }
    for x in x0..=x1 {
        put_char(buf, x, y0, '─');
        put_char(buf, x, y1, '─');
    }
    for y in y0..=y1 {
        put_char(buf, x0, y, '│');
        put_char(buf, x1, y, '│');
    }
    put_char(buf, x0, y0, '╭');
    put_char(buf, x1, y0, '╮');
    put_char(buf, x0, y1, '╰');
    put_char(buf, x1, y1, '╯');
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

/// Write one border `ch` at `(x, row)` over the box backdrop.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(BORDER);
    cell.bg = Some(BACKDROP);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kanban_menu() -> ListContextMenuState {
        ListContextMenuState::new(
            "#abc123",
            (10, 5),
            vec![
                ListMenuItem::new("Open", ListMenuAction::Open("task-1".into())),
                ListMenuItem::new("Run now", ListMenuAction::RunNow("task-1".into())),
                ListMenuItem::new("Cancel", ListMenuAction::Cancel("task-1".into())),
                ListMenuItem::new("Copy id", ListMenuAction::CopyId("#abc123".into())),
            ],
        )
    }

    /// Keyboard: step down to `Run now` and Enter fires the RunNow action + closes.
    #[test]
    fn run_now_fires_and_closes() {
        let mut m = kanban_menu();
        assert_eq!(m.handle_key(ListMenuKey::Down), None); // Run now
        let fired = m.handle_key(ListMenuKey::Enter);
        assert_eq!(fired, Some(ListMenuAction::RunNow("task-1".into())));
        assert!(m.is_closed(), "firing a leaf closes the menu");
    }

    /// Keyboard: Copy id fires the CopyId action, flashes the note, keeps the menu
    /// open.
    #[test]
    fn copy_id_flashes_note_and_stays_open() {
        let mut m = kanban_menu();
        for _ in 0..3 {
            m.handle_key(ListMenuKey::Down); // Copy id (index 3)
        }
        let fired = m.handle_key(ListMenuKey::Enter);
        assert_eq!(fired, Some(ListMenuAction::CopyId("#abc123".into())));
        assert!(m.copied());
        assert!(!m.is_closed());
    }

    /// Mouse: a click on the painted `Run now` row fires the action.
    #[test]
    fn click_run_now_row_fires() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = kanban_menu();
        render_list_context_menu(&mut buf, 80, 24, &mut m);
        let run_rect = m.hit_map.rows[1].0; // Run now is item index 1
        let fired = m.handle_click(run_rect.x, run_rect.y);
        assert_eq!(fired, Some(ListMenuAction::RunNow("task-1".into())));
    }

    /// Mouse: a click off every row dismisses the menu.
    #[test]
    fn click_away_dismisses() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = kanban_menu();
        render_list_context_menu(&mut buf, 80, 24, &mut m);
        assert_eq!(m.handle_click(79, 23), None);
        assert!(m.is_closed());
    }

    /// Render: the menu paints its title + every item label, clamped on-screen.
    #[test]
    fn render_paints_items_and_clamps_on_screen() {
        let mut buf = WireBuffer::new(80, 24);
        let mut m = ListContextMenuState::new(
            "#zzz",
            (79, 23),
            vec![ListMenuItem::new(
                "Run now",
                ListMenuAction::RunNow("t".into()),
            )],
        );
        render_list_context_menu(&mut buf, 80, 24, &mut m);
        let text: String = buf.cells.iter().map(|(_, c)| c.symbol.as_str()).collect();
        assert!(text.contains("#zzz"), "title painted: {text}");
        assert!(text.contains("Run now"), "item painted: {text}");
        for (coord, _) in &buf.cells {
            assert!(coord.x < 80 && coord.y < 24, "menu painted off-screen");
        }
    }
}
