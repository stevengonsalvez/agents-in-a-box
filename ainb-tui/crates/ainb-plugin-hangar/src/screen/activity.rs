//! Per-issue ACTIVITY TIMELINE modal (multica parity #13).
//!
//! Opened with `y` on a selected issue-list row (or on the task-detail screen),
//! the activity timeline is a modal overlay — the same shape as the agent picker
//! — over whatever screen launched it. It shows the card's narrative: creation,
//! state moves, re-assignments, priority/title/due-date edits, task outcomes,
//! and the comments merged in by timestamp, oldest first.
//!
//! The rows come from the daemon's `hangar/issue_timeline` RPC
//! ([`TimelineEntryRow`]); the plugin owns zero domain data — it caches the
//! snapshot for render and nothing more.
//!
//! As with every Hangar screen the reducer ([`reduce_activity`]) is **pure**:
//! `j`/`k` scroll, `r` asks the glue to re-fetch, Esc is handled by the router.

use ainb_hangar_core::activity::ActivityAction;
use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::snapshots::TimelineEntryRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Cornflower-blue modal border (ainb-tui chrome accent).
const BORDER: Color = Color::rgb(100, 149, 237);
/// Gold modal title + hotkey letters in the bottom bar.
const TITLE: Color = Color::rgb(255, 215, 0);
/// Selected-row cursor + text.
const SELECTION: Color = Color::rgb(100, 200, 100);
/// Muted timestamps + help-bar descriptions.
const MUTED: Color = Color::rgb(120, 120, 140);
/// Body text.
const TEXT: Color = Color::rgb(220, 220, 230);
/// Opaque panel background painted across the whole modal rect (the host
/// composites the plugin buffer as a SPARSE overlay, so unwritten cells would
/// leak the screen underneath).
const PANEL_BG: Color = Color::rgb(30, 30, 40);

/// The render-state cache for the activity-timeline modal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActivityState {
    issue_id: String,
    issue_title: String,
    entries: Vec<TimelineEntryRow>,
    selected: usize,
    loading: bool,
}

impl ActivityState {
    /// A fresh, still-loading timeline for `issue_id`.
    #[must_use]
    pub fn loading(issue_id: &IssueId, issue_title: impl Into<String>) -> Self {
        Self {
            issue_id: issue_id.as_str().to_string(),
            issue_title: issue_title.into(),
            entries: Vec::new(),
            selected: 0,
            loading: true,
        }
    }

    /// Fold in a fetched timeline, clamping the selection into the new list.
    pub fn apply_entries(&mut self, entries: Vec<TimelineEntryRow>) {
        self.entries = entries;
        self.loading = false;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    /// The issue the modal was opened for.
    #[must_use]
    pub fn issue_id(&self) -> &str {
        &self.issue_id
    }

    /// The issue title rendered in the modal header.
    #[must_use]
    pub fn issue_title(&self) -> &str {
        &self.issue_title
    }

    /// The cached entries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[TimelineEntryRow] {
        &self.entries
    }

    /// The current selection index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the first fetch is still in flight.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        self.loading
    }
}

/// An input the activity reducer folds into [`ActivityState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityEvent {
    /// A printable key (`'j'`, `'k'`, `'r'`, …).
    Key(char),
}

/// A side-effect the plugin glue performs after an activity reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityIntent {
    /// Re-fetch the timeline for the issue (`r`).
    Refresh {
        /// The issue whose timeline to re-read.
        issue_id: String,
    },
}

/// The result of folding one [`ActivityEvent`] into an [`ActivityState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityReduction {
    /// The next state.
    pub state: ActivityState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<ActivityIntent>,
}

/// Fold one [`ActivityEvent`] into `state`. Pure: no IO, no input mutation.
///
/// Esc is deliberately NOT handled here — the router owns modal dismissal so
/// every modal closes the same way.
#[must_use]
pub fn reduce_activity(state: &ActivityState, ev: ActivityEvent) -> ActivityReduction {
    let ActivityEvent::Key(c) = ev;
    match c {
        'j' => {
            let mut next = state.clone();
            let max = next.entries.len().saturating_sub(1);
            next.selected = (next.selected + 1).min(max);
            ActivityReduction {
                state: next,
                intent: None,
            }
        }
        'k' => {
            let mut next = state.clone();
            next.selected = next.selected.saturating_sub(1);
            ActivityReduction {
                state: next,
                intent: None,
            }
        }
        'r' => ActivityReduction {
            state: state.clone(),
            intent: Some(ActivityIntent::Refresh {
                issue_id: state.issue_id.clone(),
            }),
        },
        _ => ActivityReduction {
            state: state.clone(),
            intent: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Row formatting (shared with the tests, so the assertion is on real text)
// ---------------------------------------------------------------------------

/// `hh:mm` for an epoch-millis timestamp, UTC. Deliberately clock-free (pure
/// integer math) so a render snapshot is deterministic.
#[must_use]
pub fn hhmm(ms: i64) -> String {
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// The WHO column: `you` for a member, the agent id (or its resolved name) for
/// an agent, `system` for a daemon-driven row.
#[must_use]
pub fn actor_label(e: &TimelineEntryRow, resolve_agent: &dyn Fn(&str) -> Option<String>) -> String {
    match (e.actor_type.as_deref(), e.actor_id.as_deref()) {
        (Some("member"), _) => "you".to_string(),
        (Some("agent"), Some(id)) => resolve_agent(id).unwrap_or_else(|| id.to_string()),
        (Some("system") | None, _) => "system".to_string(),
        (Some(other), _) => other.to_string(),
    }
}

/// The WHAT column: the action's human label, or `💬` for a comment. An action
/// token this binary does not know renders RAW (the tolerant-read contract).
#[must_use]
pub fn action_label(e: &TimelineEntryRow) -> String {
    e.action.as_deref().map_or_else(
        || "💬".to_string(),
        |raw| ActivityAction::parse(raw).map_or_else(|| raw.to_string(), |a| a.label().to_string()),
    )
}

/// The DETAIL column: the comment body, or the change the details describe
/// (`open → in_progress`).
#[must_use]
pub fn detail_label(e: &TimelineEntryRow) -> String {
    if let Some(body) = e.body.as_deref() {
        return body.replace('\n', " ");
    }
    let Some(details) = e.details.as_ref() else {
        return String::new();
    };

    let render = |v: Option<&serde_json::Value>| -> String {
        match v {
            None | Some(serde_json::Value::Null) => "—".to_string(),
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }
    };
    if details.get("from_type").is_some() || details.get("to_type").is_some() {
        let side = |t: &str, i: &str| match (details.get(t), details.get(i)) {
            (Some(serde_json::Value::String(t)), Some(serde_json::Value::String(i))) => {
                format!("{t}:{i}")
            }
            _ => "—".to_string(),
        };
        return format!(
            "{} → {}",
            side("from_type", "from_id"),
            side("to_type", "to_id")
        );
    }
    if details.get("from").is_some() || details.get("to").is_some() {
        return format!(
            "{} → {}",
            render(details.get("from")),
            render(details.get("to"))
        );
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Width-aware modal render
// ---------------------------------------------------------------------------

/// Render the timeline as a centred modal over an `area_w` × `area_h` area.
pub fn render_activity(buf: &mut WireBuffer, area_w: u16, area_h: u16, state: &ActivityState) {
    let modal_w = (area_w * 8 / 10).clamp(40, area_w);
    let modal_h = (area_h * 7 / 10).clamp(8, area_h);
    let x0 = (area_w.saturating_sub(modal_w)) / 2;
    let y0 = (area_h.saturating_sub(modal_h)) / 2;

    fill_background(buf, x0, y0, modal_w, modal_h);
    draw_border(buf, x0, y0, modal_w, modal_h);

    let right = x0 + modal_w;
    let title = format!(" 🕘 Activity · {} ", state.issue_title());
    put_str(buf, x0 + 2, y0, &title, TITLE, right.saturating_sub(1));

    let inner_x = x0 + 2;
    let mut row = y0 + 1;
    // The last interior row carries the help bar.
    let bottom = y0 + modal_h - 2;

    if state.is_loading() {
        put_str(buf, inner_x, row, "loading…", MUTED, right - 1);
    } else if state.entries().is_empty() {
        put_str(buf, inner_x, row, "no activity yet", MUTED, right - 1);
    } else {
        for (i, e) in state.entries().iter().enumerate() {
            if row >= bottom {
                break;
            }
            let selected = i == state.selected_index();
            let cursor = if selected { "▶ " } else { "  " };
            put_str(
                buf,
                inner_x,
                row,
                cursor,
                if selected { SELECTION } else { MUTED },
                right - 1,
            );
            let mut cx = inner_x + 2;
            cx = put_str(
                buf,
                cx,
                row,
                &format!("{}  ", hhmm(e.created_at)),
                MUTED,
                right - 1,
            );
            let who = actor_label(e, &|_| None);
            cx = put_str(
                buf,
                cx,
                row,
                &format!("{who:<12} "),
                if selected { SELECTION } else { TEXT },
                right - 1,
            );
            cx = put_str(
                buf,
                cx,
                row,
                &format!("{:<10} ", action_label(e)),
                TEXT,
                right - 1,
            );
            put_str(buf, cx, row, &detail_label(e), TEXT, right - 1);
            row += 1;
        }
    }

    // Bottom help bar: gold keys + muted descriptions.
    let bar = y0 + modal_h - 1;
    let mut cx = inner_x;
    for (key, desc) in [
        ("j/k", " scroll   "),
        ("r", " refresh   "),
        ("esc", " back"),
    ] {
        cx = put_str(buf, cx, bar, key, TITLE, right - 1);
        cx = put_str(buf, cx, bar, desc, MUTED, right - 1);
    }
}

/// Fill every cell of the `w` × `h` rect at `(x0, y0)` with an opaque space.
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

/// Write `s` at `(x, row)` in `color`, clipping at column `right`. Returns the
/// column after the last written cell.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
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
    cx
}

/// Write one `ch` at `(x, row)` in `color`.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}
