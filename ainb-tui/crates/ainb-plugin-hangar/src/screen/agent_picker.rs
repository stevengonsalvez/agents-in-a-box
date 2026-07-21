//! P4.5 — Agent picker modal overlay: the pure reducer + centred modal render.
//!
//! Opened with `a` on a selected issue-list row, the agent picker is a modal
//! overlay (centred, dim background) over whatever screen launched it. It shows a
//! **polymorphic actor list** — humans and agents in one flat list, recent-use
//! pinned ahead of the alphabetical body (reference UX §12.1) — with a
//! type-narrowing `/` filter. Enter assigns the selected actor to the issue
//! ([`AgentPickerIntent::Assign`]); Esc closes without assigning.
//!
//! As with every Hangar screen the reducer ([`reduce_agent_picker`]) is **pure**:
//! it folds a key into a new [`AgentPickerState`] plus an optional intent for the
//! plugin glue to fire the assign RPC. The actor snapshot comes from the daemon's
//! `hangar/agents_list` RPC ([`ActorRow`]); the plugin owns zero domain data
//! (`project_ainb_plugin_owns_data_plane`) — it caches the snapshot for render
//! and nothing more.

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::ActorRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::widgets::actor_row::render_actor_row;

/// Cornflower-blue modal border (ainb-tui chrome accent).
const BORDER: Color = Color::rgb(100, 149, 237);
/// Gold modal title.
const TITLE: Color = Color::rgb(255, 215, 0);
/// Muted section-header text (`RECENT`).
const SECTION: Color = Color::rgb(120, 120, 140);
/// Opaque panel background painted across the whole modal rect. The host
/// composites the plugin buffer as a *sparse* overlay, so any interior coord the
/// picker doesn't explicitly write keeps the underlying board draw and bleeds
/// through. Filling every cell with this bg makes the modal opaque.
const PANEL_BG: Color = Color::rgb(30, 30, 40);

/// The render-state cache for the agent-picker modal.
///
/// Holds the issue the picker was opened for, the actor snapshot (already sorted
/// recent-then-alpha), the current selection within the *visible* list, the
/// filter mode + query, and a closed flag the router reads to dismiss the modal.
/// All fields are private; tests and the renderer read through accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPickerState {
    issue_id: IssueId,
    /// All actors, sorted recent-then-alpha at construction.
    actors: Vec<ActorRow>,
    /// Selection index into [`Self::visible_actors`].
    selected: usize,
    /// Whether `/` filter mode is active (typed chars narrow the list).
    filtering: bool,
    /// The current filter query (matched case-insensitively on display name).
    query: String,
    /// Set once Esc closed the modal; the router dismisses it.
    closed: bool,
}

impl AgentPickerState {
    /// A fresh picker for `issue_id` over `actors`, which are sorted
    /// recent-then-alpha and with the first visible actor selected.
    #[must_use]
    pub fn new(issue_id: IssueId, actors: Vec<ActorRow>) -> Self {
        let mut actors = actors;
        sort_recent_then_alpha(&mut actors);
        Self {
            issue_id,
            actors,
            selected: 0,
            filtering: false,
            query: String::new(),
            closed: false,
        }
    }

    /// The issue the picker was opened for.
    #[must_use]
    pub const fn issue_id(&self) -> &IssueId {
        &self.issue_id
    }

    /// The current selection index (into [`Self::visible_actors`]).
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the modal has been closed (Esc).
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether `/` filter mode is active.
    #[must_use]
    pub const fn is_filtering(&self) -> bool {
        self.filtering
    }

    /// The actors currently visible: the full list narrowed by the filter query
    /// (case-insensitive substring on the display name). Already in
    /// recent-then-alpha order.
    #[must_use]
    pub fn visible_actors(&self) -> Vec<ActorRow> {
        if self.query.is_empty() {
            return self.actors.clone();
        }
        let q = self.query.to_lowercase();
        self.actors
            .iter()
            .filter(|a| a.display_name.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    /// The currently-selected visible actor, if any.
    #[must_use]
    pub fn selected_actor(&self) -> Option<ActorRow> {
        self.visible_actors().into_iter().nth(self.selected)
    }
}

/// Sort actors recent-then-alpha: pinned (`recent_rank.is_some()`) ahead of the
/// body, ties broken by rank then case-insensitive display name.
fn sort_recent_then_alpha(actors: &mut [ActorRow]) {
    actors.sort_by(|a, b| match (a.recent_rank, b.recent_rank) {
        (Some(ra), Some(rb)) => ra
            .cmp(&rb)
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()),
    });
}

/// An input the picker reducer folds into [`AgentPickerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPickerEvent {
    /// A printable key (`'j'`, `'/'`, `'\n'`, a filter char, …).
    Key(char),
    /// Escape — closes the modal.
    Esc,
}

/// A side-effect the plugin glue performs after a picker reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPickerIntent {
    /// Assign the picked actor to the issue (Enter on a selection).
    Assign {
        /// The issue the picker was opened for.
        issue_id: IssueId,
        /// The picked actor in canonical `member:<id>` / `agent:<id>` form.
        actor_ref: String,
    },
}

/// The result of folding one [`AgentPickerEvent`] into an [`AgentPickerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPickerReduction {
    /// The next picker state.
    pub state: AgentPickerState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<AgentPickerIntent>,
}

/// Fold one [`AgentPickerEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_agent_picker(state: &AgentPickerState, ev: AgentPickerEvent) -> AgentPickerReduction {
    match ev {
        AgentPickerEvent::Esc => close(state),
        AgentPickerEvent::Key(c) => reduce_key(state, c),
    }
}

/// Handle a printable key, branching on whether the filter is active.
fn reduce_key(state: &AgentPickerState, c: char) -> AgentPickerReduction {
    if state.filtering {
        return reduce_filter_key(state, c);
    }
    match c {
        '/' => start_filter(state),
        'j' => move_selection_down(state),
        'k' => move_selection_up(state),
        '\n' | '\r' => assign(state),
        _ => unchanged(state),
    }
}

/// Handle a key while the filter input is active: Enter assigns, Esc-like keys
/// don't apply here (Esc is a dedicated event), Backspace trims, any other
/// printable char extends the query.
fn reduce_filter_key(state: &AgentPickerState, c: char) -> AgentPickerReduction {
    match c {
        '\n' | '\r' => assign(state),
        '\u{8}' | '\u{7f}' => {
            // Backspace: trim the query a char, clamp the selection.
            let mut next = state.clone();
            next.query.pop();
            clamp_selection(&mut next);
            no_intent(next)
        }
        _ => {
            let mut next = state.clone();
            next.query.push(c);
            // Narrowing can shrink the visible list; clamp the selection.
            clamp_selection(&mut next);
            no_intent(next)
        }
    }
}

/// Enter filter mode (typed chars now narrow the list).
fn start_filter(state: &AgentPickerState) -> AgentPickerReduction {
    let mut next = state.clone();
    next.filtering = true;
    no_intent(next)
}

/// Close the modal (Esc), no intent.
fn close(state: &AgentPickerState) -> AgentPickerReduction {
    let mut next = state.clone();
    next.closed = true;
    no_intent(next)
}

/// Move the selection down one row, clamped to the last visible actor.
fn move_selection_down(state: &AgentPickerState) -> AgentPickerReduction {
    let mut next = state.clone();
    let len = next.visible_actors().len();
    let max = len.saturating_sub(1);
    next.selected = (next.selected + 1).min(max);
    no_intent(next)
}

/// Move the selection up one row, clamped to the first visible actor.
fn move_selection_up(state: &AgentPickerState) -> AgentPickerReduction {
    let mut next = state.clone();
    next.selected = next.selected.saturating_sub(1);
    no_intent(next)
}

/// Assign the selected actor (Enter), emitting the assign intent. A no-op when
/// the visible list is empty (nothing to assign).
fn assign(state: &AgentPickerState) -> AgentPickerReduction {
    match state.selected_actor() {
        Some(actor) => AgentPickerReduction {
            state: state.clone(),
            intent: Some(AgentPickerIntent::Assign {
                issue_id: state.issue_id.clone(),
                actor_ref: actor.actor_ref,
            }),
        },
        None => unchanged(state),
    }
}

/// Clamp the selection into the (possibly newly narrowed) visible list.
fn clamp_selection(state: &mut AgentPickerState) {
    let len = state.visible_actors().len();
    if len == 0 {
        state.selected = 0;
    } else if state.selected >= len {
        state.selected = len - 1;
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: AgentPickerState) -> AgentPickerReduction {
    AgentPickerReduction {
        state,
        intent: None,
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &AgentPickerState) -> AgentPickerReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware modal render
// ---------------------------------------------------------------------------

/// Render the picker as a centred modal over an `area_w` × `area_h` area.
///
/// The modal is ~60% wide / ~70% tall (clamped to fit the 80×24 floor), drawn
/// with a rounded cornflower-blue border, a gold title, an optional `RECENT`
/// section header, and one [`render_actor_row`] per visible actor.
pub fn render_agent_picker(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    state: &AgentPickerState,
) {
    let (x0, y0, modal_w, modal_h) = modal_rect(area_w, area_h);

    // Paint an opaque background across the whole modal first so the sparse
    // overlay can't leak the underlying board through the border/title/row gaps.
    fill_background(buf, x0, y0, modal_w, modal_h);
    draw_border(buf, x0, y0, modal_w, modal_h);
    // Title.
    put_str(buf, x0 + 2, y0, " Pick assignee ", TITLE, x0 + modal_w);

    let inner_x = x0 + 2;
    let inner_w = modal_w.saturating_sub(4);
    let mut row = y0 + 1;
    let bottom = y0 + modal_h - 1;

    let visible = state.visible_actors();
    let mut printed_recent_header = false;
    for (i, actor) in visible.iter().enumerate() {
        if row >= bottom {
            break;
        }
        // RECENT section header before the first pinned actor.
        if actor.recent_rank.is_some() && !printed_recent_header {
            put_str(buf, inner_x, row, "RECENT", SECTION, inner_x + inner_w);
            printed_recent_header = true;
            row += 1;
            if row >= bottom {
                break;
            }
        }
        render_actor_row(buf, inner_x, row, inner_w, actor, i == state.selected);
        row += 1;
    }
}

/// The centred modal geometry `(x0, y0, w, h)` for an `area_w` × `area_h` area:
/// ~60% wide / ~70% tall, clamped to the 40×8 floor and the area ceiling.
fn modal_rect(area_w: u16, area_h: u16) -> (u16, u16, u16, u16) {
    let modal_w = (area_w * 6 / 10).clamp(40, area_w);
    let modal_h = (area_h * 7 / 10).clamp(8, area_h);
    let x0 = (area_w.saturating_sub(modal_w)) / 2;
    let y0 = (area_h.saturating_sub(modal_h)) / 2;
    (x0, y0, modal_w, modal_h)
}

/// Fill every cell of the `w` × `h` rect at `(x0, y0)` with an opaque space
/// carrying [`PANEL_BG`], so the host's sparse overlay composite can't leak the
/// board through any coord the border/title/rows don't cover.
fn fill_background(buf: &mut WireBuffer, x0: u16, y0: u16, w: u16, h: u16) {
    let x1 = x0 + w - 1;
    let y1 = y0 + h - 1;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut cell = Cell::new(" ");
            cell.bg = Some(PANEL_BG);
            buf.push(Coord::new(x, y), cell);
        }
    }
}

/// Draw a rounded border rectangle of `w` × `h` at `(x0, y0)`.
fn draw_border(buf: &mut WireBuffer, x0: u16, y0: u16, w: u16, h: u16) {
    let x1 = x0 + w - 1;
    let y1 = y0 + h - 1;
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
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
}

/// Write one `ch` at `(x, row)` in `color`.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ainb_hangar_proto::events::PresenceState;
    use std::collections::HashMap;

    fn actor(name: &str, is_agent: bool, recent_rank: Option<u32>) -> ActorRow {
        ActorRow {
            actor_ref: if is_agent {
                format!("agent:{name}")
            } else {
                format!("member:{name}")
            },
            display_name: name.to_string(),
            subtitle: "sub".into(),
            presence: PresenceState::Online,
            is_agent,
            recent_rank,
        }
    }

    fn picker() -> AgentPickerState {
        AgentPickerState::new(
            IssueId::from_str("i1").expect("valid id"),
            vec![
                actor("dev", true, Some(0)),
                actor("reviewer", true, None),
                actor("Stevie", false, None),
            ],
        )
    }

    /// The modal must paint an **opaque** rectangle: every interior coord of the
    /// modal rect has to be written by the picker so the host's sparse overlay
    /// can never leak the underlying board draw through the gaps.
    ///
    /// Regression: before the background fill, `render_agent_picker` wrote only
    /// the border, title, and one glyph-cell per actor row, leaving the gaps
    /// between/around rows unwritten — the kanban board bled through them.
    #[test]
    fn modal_paints_every_interior_coord_opaque() {
        let area_w = 80;
        let area_h = 24;
        let state = picker();

        let mut buf = WireBuffer::new(area_w, area_h);
        render_agent_picker(&mut buf, area_w, area_h, &state);

        // Effective (last-write-wins) cell per coord, mirroring host composite.
        let mut painted: HashMap<(u16, u16), &Cell> = HashMap::new();
        for (coord, cell) in &buf.cells {
            painted.insert((coord.x, coord.y), cell);
        }

        let (x0, y0, w, h) = modal_rect(area_w, area_h);
        let (x1, y1) = (x0 + w - 1, y0 + h - 1);

        for y in y0..=y1 {
            for x in x0..=x1 {
                assert!(
                    painted.contains_key(&(x, y)),
                    "modal coord ({x},{y}) is unpainted — the board would bleed \
                     through the sparse overlay here"
                );
            }
        }
    }

    /// Every cell the fill lays down under the modal carries the opaque panel
    /// background, so the underlying board colour is fully replaced (not just
    /// its glyph). Border/title/rows drawn on top may override individual cells,
    /// but no interior gap is left transparent.
    #[test]
    fn interior_gaps_carry_opaque_background() {
        let area_w = 80;
        let area_h = 24;
        let state = picker();

        let mut buf = WireBuffer::new(area_w, area_h);
        render_agent_picker(&mut buf, area_w, area_h, &state);

        let mut painted: HashMap<(u16, u16), Cell> = HashMap::new();
        for (coord, cell) in &buf.cells {
            painted.insert((coord.x, coord.y), cell.clone());
        }

        let (x0, y0, _w, h) = modal_rect(area_w, area_h);
        // An empty interior coord well inside the modal, below the rows.
        let gap = painted.get(&(x0 + 5, y0 + h - 2)).expect("interior gap coord must be painted");
        assert_eq!(gap.symbol, " ", "gap should be a space, not a board glyph");
        assert_eq!(
            gap.bg,
            Some(PANEL_BG),
            "gap cell must carry the opaque panel background"
        );
    }
}
