//! P7.5 — Autopilot manager screen: the pure reducer + width-aware render.
//!
//! The autopilot manager (hotkey `5`) is a two-region screen: an upper table of
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
}

/// An input the autopilots reducer folds into [`AutopilotsState`].
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
    let intent = next
        .selected_autopilot()
        .map(|ap| AutopilotsIntent::LoadRuns(ap.id.clone()));
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
            if state
                .selected_autopilot()
                .is_some_and(|ap| ap.id == autopilot_id)
            {
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
// Width-aware two-region render
// ---------------------------------------------------------------------------

/// The fixed table column widths (NAME / CRON / NEXT TICK / LAST RUN / STATUS).
/// The action-key hints sit to the right of the header row, beside the controls
/// they affect (`feedback_keybinding_hints_near_control`).
const NAME_W: u16 = 16;
const CRON_W: u16 = 16;
const NEXT_W: u16 = 14;
const LAST_W: u16 = 14;

/// Render the autopilot manager into `buf` between rows `top` and `bottom`.
///
/// The upper region is the autopilot table (header + one row per autopilot with a
/// `▶` selection marker + an enabled/disabled badge); the lower region (below a
/// divider) is the run-history pane for the selected autopilot. The empty list
/// shows the "No autopilots" help line. The action-key hints paint on the header
/// row, right-aligned next to the controls they drive.
pub fn render_autopilots(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &AutopilotsState,
) {
    // Header row + action hints.
    let header = format!(
        "{:<nw$} {:<cw$} {:<xw$} {:<lw$} STATUS",
        "NAME",
        "CRON",
        "NEXT TICK",
        "LAST RUN",
        nw = NAME_W as usize,
        cw = CRON_W as usize,
        xw = NEXT_W as usize,
        lw = LAST_W as usize,
    );
    let header_end = u16::try_from(header.chars().count()).unwrap_or(0);
    put_str(buf, 0, top, &header, MUTED_GRAY, area_w);
    render_action_hints(buf, top, area_w, header_end);

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

    // The table occupies the upper half; the run-history pane the lower half,
    // separated by a divider. Reserve at least 3 rows for the history pane.
    let avail = bottom.saturating_sub(body_top);
    let table_rows = (avail.saturating_sub(4))
        .max(1)
        .min(u16::try_from(state.autopilots.len()).unwrap_or(u16::MAX));
    let mut row = body_top;
    for (i, ap) in state.autopilots.iter().enumerate() {
        if i >= table_rows as usize {
            break;
        }
        render_row(buf, row, area_w, ap, i == state.selected);
        row += 1;
    }

    // Divider + run-history pane.
    let divider_row = row;
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

/// Paint the action-key hints on the header row, right-aligned so each key sits
/// beside the controls it drives (`feedback_keybinding_hints_near_control`).
///
/// Suppressed when the right-aligned hints would overlap the header text
/// (`header_end`) — the table header always wins the column contest, so a narrow
/// terminal drops the hint rather than clobbering `STATUS` (the footer still
/// carries the same hints).
fn render_action_hints(buf: &mut WireBuffer, row: u16, area_w: u16, header_end: u16) {
    const HINTS: &str = "[a]dd [r]un [d]isable [e]dit";
    let hint_w = u16::try_from(HINTS.chars().count()).unwrap_or(0);
    if hint_w >= area_w {
        return;
    }
    let start = area_w - hint_w;
    // Keep a one-column gap after the header text; drop the hints if they'd
    // collide with it.
    if start <= header_end {
        return;
    }
    put_str(buf, start, row, HINTS, GOLD, area_w);
}

/// Render one autopilot table row with a `▶` marker on the selection and an
/// enabled/disabled badge.
fn render_row(buf: &mut WireBuffer, row: u16, area_w: u16, ap: &AutopilotRow, selected: bool) {
    let marker = if selected { '▶' } else { ' ' };
    let name_color = if selected {
        SELECTION_GREEN
    } else {
        SOFT_WHITE
    };

    let next = ap
        .next_tick_at
        .map_or_else(|| "—".to_string(), |_| "scheduled".to_string());
    let last = ap
        .last_run_status
        .clone()
        .unwrap_or_else(|| "never".to_string());
    let status = if ap.enabled { "enabled" } else { "disabled" };

    // Marker + name in the selection colour; the rest in soft white.
    let head = format!(
        "{marker} {:<nw$}",
        truncate(&ap.name, NAME_W as usize - 2),
        nw = NAME_W as usize - 2
    );
    let mut x = put_str(buf, 0, row, &head, name_color, area_w);
    x += 1;
    x = put_str(
        buf,
        x,
        row,
        &format!(
            "{:<cw$}",
            truncate(&ap.cron_expr, CRON_W as usize),
            cw = CRON_W as usize
        ),
        SOFT_WHITE,
        area_w,
    );
    x += 1;
    x = put_str(
        buf,
        x,
        row,
        &format!("{:<xw$}", next, xw = NEXT_W as usize),
        SOFT_WHITE,
        area_w,
    );
    x += 1;
    let last_color = if last == "failed" || last == "skipped" {
        WARN_RED
    } else {
        SOFT_WHITE
    };
    x = put_str(
        buf,
        x,
        row,
        &format!(
            "{:<lw$}",
            truncate(&last, LAST_W as usize),
            lw = LAST_W as usize
        ),
        last_color,
        area_w,
    );
    x += 1;
    let status_color = if ap.enabled {
        SELECTION_GREEN
    } else {
        MUTED_GRAY
    };
    put_str(buf, x, row, status, status_color, area_w);
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

/// Truncate `s` to `max` chars with an ellipsis, char-safe (multi-byte aware).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
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
