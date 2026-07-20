//! P7.5 — Autopilot manager screen: the pure reducer + width-aware render.
//!
//! The autopilot manager (hotkey `4`) is a two-region screen: an upper table of
//! the workspace's cron-scheduled autopilots (NAME / CRON / NEXT TICK / LAST RUN
//! / STATUS) and a lower run-history pane for the selected autopilot. The action
//! keys map to live daemon RPCs: `r` fires the selected autopilot now
//! (`hangar/autopilot_fire_now`), `d` toggles enabled / disabled
//! (`hangar/autopilot_set_enabled`); `a` (add) and `e` (edit) are creation
//! intents the plugin glue routes to the create flow.
//!
//! As with every Hangar screen the reducer ([`reduce_autopilots`]) is **pure**:
//! it folds a key / host event into a new [`AutopilotsState`] plus an optional
//! [`AutopilotsIntent`] (which the plugin glue lifts into the matching daemon
//! RPC). The autopilot rows + run history come from the daemon
//! (`hangar/autopilots_list`, `hangar/autopilot_runs`); the plugin owns zero
//! domain data (`project_ainb_plugin_owns_data_plane`).

use ainb_hangar_proto::events::{AutopilotRow, AutopilotRunRow, HangarEvent};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Selected-row marker + enabled badge green.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Primary row text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (headers, hints, disabled badge).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Failed / skipped run accent.
const WARN_RED: Color = Color::rgb(220, 120, 100);

/// The render-state cache for the autopilot-manager screen.
///
/// Holds the autopilot snapshot, the list selection, and the loaded run history
/// for the selected autopilot. All fields private; tests and the renderer read
/// through accessors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutopilotsState {
    autopilots: Vec<AutopilotRow>,
    selected: usize,
    /// The run history for the selected autopilot, latest-first; keyed by the
    /// autopilot id it was loaded for so a stale reply for a since-changed
    /// selection is ignored.
    runs: Vec<AutopilotRunRow>,
    /// The autopilot id `runs` belongs to (`None` until a history loads).
    runs_for: Option<String>,
    /// First visible card index — the single board column's vertical scroll
    /// offset (63l.6). A wheel-scroll over the list nudges it.
    scroll_offset: usize,
    /// The autopilot id the pointer is hovering over (63l.6), or `None` off every
    /// card. The card-board render lifts the hovered card's border.
    hovered_id: Option<String>,
}

impl AutopilotsState {
    /// A fresh manager over `autopilots`, first row selected, no runs loaded.
    #[must_use]
    pub const fn new(autopilots: Vec<AutopilotRow>) -> Self {
        Self {
            autopilots,
            selected: 0,
            runs: Vec::new(),
            runs_for: None,
            scroll_offset: 0,
            hovered_id: None,
        }
    }

    /// The current list-selection index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// The autopilot rows.
    #[must_use]
    pub fn autopilots(&self) -> &[AutopilotRow] {
        &self.autopilots
    }

    /// The currently-selected autopilot, if any.
    #[must_use]
    pub fn selected_autopilot(&self) -> Option<&AutopilotRow> {
        self.autopilots.get(self.selected)
    }

    /// The loaded run history for the selected autopilot, or `&[]` when none is
    /// loaded (or the loaded history is for a since-changed selection).
    #[must_use]
    pub fn runs(&self) -> &[AutopilotRunRow] {
        match (self.selected_autopilot(), &self.runs_for) {
            (Some(ap), Some(for_id)) if &ap.id == for_id => &self.runs,
            _ => &[],
        }
    }

    /// Flatten the autopilots into a SINGLE card-board column (63l.6) the render
    /// paints and the mouse layer hit-tests against. Autopilots are a flat list
    /// (no status columns), so the board is one `Autopilots (N)` column of cards;
    /// each card carries the autopilot name + cron + the enabled/disabled state and
    /// last-run status in its title. The same geometry feeds the render and the
    /// hit-map.
    #[must_use]
    pub fn board_columns(&self) -> Vec<crate::widgets::card_board::BoardColumn> {
        use crate::widgets::card_board::{BoardCard, BoardColumn, PriorityChip};
        let cards = self
            .autopilots
            .iter()
            .map(|ap| {
                let last = ap.last_run_status.clone().unwrap_or_else(|| "never".into());
                let state = if ap.enabled { "enabled" } else { "disabled" };
                BoardCard {
                    issue_id: ap.id.clone(),
                    display_id: ap.name.clone(),
                    title: format!("{} · {state} · last {last}", ap.cron_expr),
                    priority: PriorityChip::from_priority(0),
                    assignee_initial: ap.name.chars().next(),
                    linked: false,
                }
            })
            .collect::<Vec<_>>();
        vec![BoardColumn {
            glyph: '◷',
            name: "Autopilots".to_string(),
            cards,
            scroll_offset: self.scroll_offset.min(self.autopilots.len().saturating_sub(1)),
        }]
    }

    /// The `(0, card)` slot the render draws with the heavy highlight border
    /// (63l.6): the hovered card when the pointer is over one, else the keyboard
    /// selection. `None` when the list is empty.
    #[must_use]
    pub fn highlight_board_card(&self) -> Option<(usize, usize)> {
        if let Some(hover) = self.hovered_id.as_deref() {
            if let Some(idx) = self.autopilots.iter().position(|a| a.id == hover) {
                return Some((0, idx));
            }
        }
        (!self.autopilots.is_empty()).then_some((0, self.selected))
    }

    /// Set (or clear) the hovered autopilot id (63l.6).
    pub fn set_hover(&mut self, id: Option<String>) {
        self.hovered_id = id;
    }

    /// The hovered autopilot id, if any (63l.6).
    #[must_use]
    pub fn hovered_id(&self) -> Option<&str> {
        self.hovered_id.as_deref()
    }

    /// Scroll the single board column vertically by `delta` rows (63l.6),
    /// saturating at `0` and capped at the last card. A wheel-scroll drives this.
    pub fn scroll(&mut self, delta: i32) {
        let next = if delta >= 0 {
            self.scroll_offset.saturating_add(delta.unsigned_abs() as usize)
        } else {
            self.scroll_offset.saturating_sub(delta.unsigned_abs() as usize)
        };
        self.scroll_offset = next.min(self.autopilots.len().saturating_sub(1));
    }

    /// Move the list selection onto the autopilot carrying `id` (63l.6 mouse
    /// click), so a pointer click lands the selection on the clicked card. A no-op
    /// when no autopilot carries that id.
    pub fn select_by_id(&mut self, id: &str) {
        if let Some(idx) = self.autopilots.iter().position(|a| a.id == id) {
            self.selected = idx;
        }
    }
}

/// An input the autopilots reducer folds into [`AutopilotsState`].
// Reduction enum: `Event(HangarEvent)` dominates the size, the rest are scalar
// inputs. Short-lived, reducer-folded, not a hot allocation path — left unboxed
// for consistency with the other screen reducers (boxing would only add churn).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutopilotsEvent {
    /// A printable key (`'j'`, `'r'`, `'d'`, …).
    Key(char),
    /// The daemon replied to a [`AutopilotsIntent::LoadRuns`] with the run list.
    RunsLoaded {
        /// The autopilot the runs belong to.
        autopilot_id: String,
        /// The run rows, latest-first.
        runs: Vec<AutopilotRunRow>,
    },
    /// A host stream event (e.g. [`HangarEvent::AutopilotUpdated`]).
    Event(HangarEvent),
}

/// A side-effect the plugin glue performs after an autopilots reduction.
///
/// Each variant maps to one daemon JSON-RPC the plugin glue fires (P7.5):
/// `LoadRuns` → `hangar/autopilot_runs`, `FireNow` → `hangar/autopilot_fire_now`,
/// `SetEnabled` → `hangar/autopilot_set_enabled`. `Add` / `Edit` open the
/// create-autopilot flow (no RPC at the screen level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutopilotsIntent {
    /// Load the run history for an autopilot (`hangar/autopilot_runs`). Raised on
    /// selection change so the history pane tracks the selected row.
    LoadRuns(String),
    /// Fire the selected autopilot now (`r`) — `hangar/autopilot_fire_now`.
    FireNow(String),
    /// Toggle the selected autopilot's enabled flag (`d`) —
    /// `hangar/autopilot_set_enabled`. Carries the id + the *target* flag.
    SetEnabled {
        /// The autopilot to toggle.
        autopilot_id: String,
        /// The target enabled state (the inverse of the current one).
        enabled: bool,
    },
    /// Open the create-autopilot flow (`a`).
    Add,
    /// Open the edit flow for the selected autopilot (`e`).
    Edit(String),
}

/// The result of folding one [`AutopilotsEvent`] into an [`AutopilotsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotsReduction {
    /// The next state.
    pub state: AutopilotsState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<AutopilotsIntent>,
}

/// Fold one [`AutopilotsEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_autopilots(state: &AutopilotsState, ev: AutopilotsEvent) -> AutopilotsReduction {
    match ev {
        AutopilotsEvent::Key(c) => reduce_key(state, c),
        AutopilotsEvent::RunsLoaded { autopilot_id, runs } => {
            runs_loaded(state, autopilot_id, runs)
        }
        AutopilotsEvent::Event(event) => fold_event(state, event),
    }
}

/// Handle a printable key (P7.5 bindings):
/// - `j`/`k` move the list selection (and re-load the run history);
/// - `r` fires the selected autopilot now;
/// - `d` toggles the selected autopilot enabled/disabled;
/// - `a` opens the create flow; `e` edits the selected autopilot.
fn reduce_key(state: &AutopilotsState, c: char) -> AutopilotsReduction {
    match c {
        'j' => move_selection(state, 1),
        'k' => move_selection(state, -1),
        'r' => with_selected(state, |ap| AutopilotsIntent::FireNow(ap.id.clone())),
        'd' => with_selected(state, |ap| AutopilotsIntent::SetEnabled {
            autopilot_id: ap.id.clone(),
            enabled: !ap.enabled,
        }),
        'a' => with_intent(state.clone(), AutopilotsIntent::Add),
        'e' => with_selected(state, |ap| AutopilotsIntent::Edit(ap.id.clone())),
        _ => unchanged(state),
    }
}

/// Build an intent that needs the selected autopilot, or a no-op when the list
/// is empty.
fn with_selected(
    state: &AutopilotsState,
    make: impl FnOnce(&AutopilotRow) -> AutopilotsIntent,
) -> AutopilotsReduction {
    state.selected_autopilot().map_or_else(
        || unchanged(state),
        |ap| with_intent(state.clone(), make(ap)),
    )
}

/// Move the selection by `delta` (clamped), raising a [`AutopilotsIntent::LoadRuns`]
/// for the newly-selected autopilot so the history pane follows the selection.
fn move_selection(state: &AutopilotsState, delta: i32) -> AutopilotsReduction {
    if state.autopilots.is_empty() {
        return unchanged(state);
    }
    let mut next = state.clone();
    let max = next.autopilots.len() - 1;
    let cur = i32::try_from(next.selected).unwrap_or(0);
    next.selected =
        usize::try_from((cur + delta).clamp(0, i32::try_from(max).unwrap_or(0))).unwrap_or(0);
    // Selection changed → pull the new row's run history.
    let intent = next.selected_autopilot().map(|ap| AutopilotsIntent::LoadRuns(ap.id.clone()));
    AutopilotsReduction {
        state: next,
        intent,
    }
}

/// Populate the run history for the selected autopilot from a daemon reply,
/// ignoring a reply for a since-changed selection.
fn runs_loaded(
    state: &AutopilotsState,
    autopilot_id: String,
    runs: Vec<AutopilotRunRow>,
) -> AutopilotsReduction {
    let mut next = state.clone();
    next.runs = runs;
    next.runs_for = Some(autopilot_id);
    no_intent(next)
}

/// Fold a host event: an `AutopilotUpdated` refreshes the matching row in place;
/// an `AutopilotRunChanged` for the selected autopilot re-pulls its run history.
fn fold_event(state: &AutopilotsState, event: HangarEvent) -> AutopilotsReduction {
    match event {
        HangarEvent::AutopilotUpdated(row) => {
            let mut next = state.clone();
            if let Some(slot) = next.autopilots.iter_mut().find(|a| a.id == row.id) {
                *slot = row;
            }
            no_intent(next)
        }
        HangarEvent::AutopilotRunChanged { autopilot_id, .. } => {
            // Re-pull the run history when the change is for the selected row.
            if state.selected_autopilot().is_some_and(|ap| ap.id == autopilot_id) {
                with_intent(state.clone(), AutopilotsIntent::LoadRuns(autopilot_id))
            } else {
                unchanged(state)
            }
        }
        _ => unchanged(state),
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: AutopilotsState) -> AutopilotsReduction {
    AutopilotsReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
const fn with_intent(state: AutopilotsState, intent: AutopilotsIntent) -> AutopilotsReduction {
    AutopilotsReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &AutopilotsState) -> AutopilotsReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Card-board upper region + run-history lower region (63l.6)
// ---------------------------------------------------------------------------

/// Render the autopilot manager into `buf` between rows `top` and `bottom`.
///
/// The UPPER region renders the autopilots THROUGH the shared Linear-style
/// card-board (63l.6) — a single `Autopilots (N)` column of bordered cards, each
/// carrying the autopilot name (id line), its cron + enabled/disabled state +
/// last-run status (title), with the hovered/selected card raised by the heavy
/// clay border. The action-key hints paint on the header row. The LOWER region
/// (below a divider) keeps the run-history pane for the selected autopilot. The
/// empty list shows the "No autopilots" help line.
pub fn render_autopilots(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &AutopilotsState,
) {
    // Action-key hints on the top row, right-aligned beside the controls they
    // drive (`feedback_keybinding_hints_near_control`).
    render_action_hints(buf, top, area_w);

    let body_top = top + 1;

    if state.autopilots.is_empty() {
        put_str(
            buf,
            0,
            body_top,
            "No autopilots. Press 'a' to add",
            MUTED_GRAY,
            area_w,
        );
        return;
    }

    // The card-board occupies the upper region; the run-history pane the lower
    // region, separated by a divider. Reserve at least 4 rows for the history.
    let avail = bottom.saturating_sub(body_top);
    let board_bottom = body_top.saturating_add(avail.saturating_sub(4).max(1));
    let columns = state.board_columns();
    let highlight = state.highlight_board_card();
    let _ = crate::widgets::card_board::render_card_board(
        buf,
        area_w,
        body_top,
        board_bottom,
        &columns,
        highlight,
    );

    // Divider + run-history pane.
    let divider_row = board_bottom;
    let label = state
        .selected_autopilot()
        .map_or_else(String::new, |ap| format!("─ Recent runs ({}) ", ap.name));
    let divider = format!(
        "{label}{}",
        "─".repeat((area_w as usize).saturating_sub(label.chars().count()))
    );
    put_str(buf, 0, divider_row, &divider, MUTED_GRAY, area_w);

    let mut hr = divider_row + 1;
    let runs = state.runs();
    if runs.is_empty() {
        put_str(buf, 0, hr, "(no runs yet)", MUTED_GRAY, area_w);
    } else {
        for run in runs {
            if hr >= bottom {
                break;
            }
            render_run(buf, hr, area_w, run);
            hr += 1;
        }
    }
}

/// Paint the action-key hints on the top row, right-aligned so each key sits
/// beside the controls it drives (`feedback_keybinding_hints_near_control`).
/// Dropped on a terminal too narrow to hold them (the footer carries them too).
fn render_action_hints(buf: &mut WireBuffer, row: u16, area_w: u16) {
    const HINTS: &str = "[a]dd [r]un [d]isable [e]dit";
    let hint_w = u16::try_from(HINTS.chars().count()).unwrap_or(0);
    if hint_w >= area_w {
        return;
    }
    let start = area_w - hint_w;
    put_str(buf, start, row, HINTS, GOLD, area_w);
}

/// Render one run-history line.
fn render_run(buf: &mut WireBuffer, row: u16, area_w: u16, run: &AutopilotRunRow) {
    let color = match run.status.as_str() {
        "failed" | "skipped" => WARN_RED,
        "completed" => SELECTION_GREEN,
        _ => SOFT_WHITE,
    };
    let line = format!("{}  {}", run.started_at, run.status);
    put_str(buf, 0, row, &line, color, area_w);
}

/// Write `s` at `(x, row)` in `color`, clipping at `area_w`. Returns the next
/// free column. Char-safe (iterates `char`s, not bytes).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, area_w: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= area_w {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn autopilot(id: &str, name: &str, enabled: bool) -> AutopilotRow {
        AutopilotRow {
            id: id.into(),
            workspace_id: "default".into(),
            agent_id: "agent-1".into(),
            name: name.into(),
            cron_expr: "0 9 * * *".into(),
            next_tick_at: Some(1),
            enabled,
            last_run_status: Some("completed".into()),
            last_run_at: Some(1),
        }
    }

    /// `board_columns` flattens the autopilots into a single card-board column
    /// whose cards carry the name + cron + enabled/disabled state, so the screen
    /// renders THROUGH the shared card-board (63l.6).
    #[test]
    fn board_columns_map_autopilots_to_a_single_column_of_cards() {
        let state = AutopilotsState::new(vec![
            autopilot("ap-1", "daily", true),
            autopilot("ap-2", "nightly", false),
        ]);
        let cols = state.board_columns();
        assert_eq!(cols.len(), 1, "autopilots are a single flat column");
        assert_eq!(cols[0].name, "Autopilots");
        assert_eq!(cols[0].cards.len(), 2);
        assert_eq!(cols[0].cards[0].issue_id, "ap-1");
        assert_eq!(cols[0].cards[0].display_id, "daily");
        assert!(
            cols[0].cards[0].title.contains("enabled"),
            "the enabled autopilot's card title reads enabled: {:?}",
            cols[0].cards[0].title
        );
        assert!(
            cols[0].cards[1].title.contains("disabled"),
            "the disabled autopilot's card title reads disabled: {:?}",
            cols[0].cards[1].title
        );
    }

    /// A hover overrides the selection for the card-board highlight; clearing it
    /// falls back to the selection.
    #[test]
    fn highlight_resolves_hover_over_selection() {
        let mut state = AutopilotsState::new(vec![
            autopilot("ap-1", "a", true),
            autopilot("ap-2", "b", true),
        ]);
        assert_eq!(state.highlight_board_card(), Some((0, 0)));
        state.set_hover(Some("ap-2".to_string()));
        assert_eq!(state.highlight_board_card(), Some((0, 1)));
        state.set_hover(None);
        assert_eq!(state.highlight_board_card(), Some((0, 0)));
    }

    /// A wheel-scroll nudges the single column's offset, saturating at `0` and
    /// capped at the last card.
    #[test]
    fn scroll_offsets_and_saturates() {
        let aps: Vec<AutopilotRow> =
            (0..4).map(|i| autopilot(&format!("ap-{i}"), &format!("n{i}"), true)).collect();
        let mut state = AutopilotsState::new(aps);
        state.scroll(1);
        state.scroll(1);
        assert_eq!(state.board_columns()[0].scroll_offset, 2);
        state.scroll(-5);
        assert_eq!(state.board_columns()[0].scroll_offset, 0);
        for _ in 0..10 {
            state.scroll(1);
        }
        assert_eq!(state.board_columns()[0].scroll_offset, 3);
    }

    /// A mouse click selects the clicked autopilot by id (a no-op for an unknown
    /// id), so the run-history pane follows the click.
    #[test]
    fn select_by_id_moves_the_selection() {
        let mut state = AutopilotsState::new(vec![
            autopilot("ap-1", "a", true),
            autopilot("ap-2", "b", true),
        ]);
        state.select_by_id("ap-2");
        assert_eq!(state.selected_index(), 1);
        state.select_by_id("ghost");
        assert_eq!(state.selected_index(), 1, "an unknown id is a no-op");
    }
}
