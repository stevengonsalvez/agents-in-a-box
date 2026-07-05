//! P8.6 — Logs tail pane: a read-only, scrollable view over the daemon's
//! structured JSON log file.
//!
//! The logs screen (hotkey `L`) renders the daemon's P8.1 structured-log file
//! (`<hangar_home>/hangar/logs/daemon.<date>`) the same way the
//! `ainb hangar logs tail` CLI does: it reads the newest `daemon.*` file via the
//! shared [`ainb_hangar_core::logs`] reader and paints each event as
//! `{ts} {LVL} {target} {msg} {k=v}`, coloured by level. It is a **file read**,
//! not a daemon RPC — the same JSONL the CLI tails, so the plugin still owns no
//! domain data over the wire (`project_ainb_plugin_owns_data_plane`): the bytes
//! it renders are the daemon's own log artefact on disk.
//!
//! Level-filter chips sit at the top with their key hints rendered next to each
//! chip (`feedback_keybinding_hints_near_control`): `[a]all [i]info [w]warn
//! [e]error`. The active chip is highlighted; pressing its key sets the
//! `--level` floor the reader applies on the next refresh.

use ainb_hangar_core::logs::{LogLevel, LogLine};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (labels, hints, inactive chips, target column).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// `INFO` level colour.
const INFO_BLUE: Color = Color::rgb(100, 149, 237);
/// `WARN` level colour.
const WARN_AMBER: Color = Color::rgb(230, 190, 90);
/// `ERROR` level colour.
const ERROR_RED: Color = Color::rgb(220, 100, 90);
/// `DEBUG`/`TRACE` level colour (dim).
const DEBUG_DIM: Color = Color::rgb(140, 140, 160);
/// Active-chip highlight.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

/// The render state for the logs screen.
///
/// Holds the parsed log lines (oldest-first) most recently read from disk plus
/// the active level filter. Default is the empty pane shown before the first
/// read (or when the daemon has never run / written a log).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogsState {
    /// Parsed log lines, oldest-first (chronological — reads top-down).
    lines: Vec<LogLine>,
    /// The active `--level` floor; `None` shows every level (the `all` chip).
    filter: Option<LogLevel>,
}

impl LogsState {
    /// Build the state from a slice of already-parsed log lines.
    ///
    /// The plugin glue reads the newest `daemon.*` file with
    /// [`ainb_hangar_core::logs::read_tail`] and hands the result here; the
    /// screen itself stays pure (no IO) so it is snapshot-testable from a seeded
    /// vec.
    #[must_use]
    pub const fn from_lines(lines: Vec<LogLine>) -> Self {
        Self {
            lines,
            filter: None,
        }
    }

    /// The active level filter (read accessor for the glue / tests).
    #[must_use]
    pub const fn filter(&self) -> Option<LogLevel> {
        self.filter
    }

    /// Set the level filter floor. The glue re-reads the file applying this
    /// floor; the in-memory `lines` are also filtered defensively so the chip
    /// takes immediate effect even before the next read lands.
    pub const fn set_filter(&mut self, filter: Option<LogLevel>) {
        self.filter = filter;
    }

    /// Fold a key press into the screen, returning `true` when it changed the
    /// active filter (so the glue knows to re-read the file with the new floor).
    ///
    /// `a` = all, `i` = info floor, `w` = warn floor, `e` = error floor. Any
    /// other key is a no-op (returns `false`).
    pub fn handle_key(&mut self, c: char) -> bool {
        let next = match c {
            'a' => None,
            'i' => Some(LogLevel::Info),
            'w' => Some(LogLevel::Warn),
            'e' => Some(LogLevel::Error),
            _ => return false,
        };
        if next == self.filter {
            return false;
        }
        self.filter = next;
        true
    }

    /// The lines visible under the active filter, oldest-first.
    fn visible(&self) -> Vec<&LogLine> {
        self.lines.iter().filter(|l| l.passes_level(self.filter)).collect()
    }
}

/// The level-filter chips, in render order: `(key, label, floor)`.
const CHIPS: [(char, &str, Option<LogLevel>); 4] = [
    ('a', "all", None),
    ('i', "info", Some(LogLevel::Info)),
    ('w', "warn", Some(LogLevel::Warn)),
    ('e', "error", Some(LogLevel::Error)),
];

/// Render the logs pane into `buf` between rows `top` and `bottom`.
///
/// Layout (top-to-bottom):
///
/// ```text
/// Logs
/// [a]all  [i]info  [w]warn  [e]error
/// 12:00:00 INFO  ainb_hangar_daemon  daemon ready  task_id=t-aaa
/// 12:00:01 WARN  …::run_loop         claim slot retry  attempts=2
/// …
/// ```
///
/// The chip row carries each key hint next to its chip
/// (`feedback_keybinding_hints_near_control`); the active chip is highlighted
/// `SELECTION_GREEN`. Each log line is coloured by level. Strings truncate via
/// `chars()`, never byte-slice (the rust-utf8-truncate trap).
pub fn render_logs(buf: &mut WireBuffer, area_w: u16, top: u16, bottom: u16, state: &LogsState) {
    let mut row = top;
    put_str(buf, 0, row, "Logs", GOLD, area_w);
    row += 1;

    // Level-filter chips with their key hints rendered next to each chip.
    render_chips(buf, row, area_w, state.filter);
    row += 2;

    let visible = state.visible();
    if visible.is_empty() {
        put_str(buf, 0, row, "no log events", MUTED_GRAY, area_w);
        return;
    }

    for line in visible {
        if row > bottom {
            break;
        }
        render_line(buf, row, area_w, line);
        row += 1;
    }
}

/// Render the chip row: `[a]all  [i]info  [w]warn  [e]error`, active highlighted.
fn render_chips(buf: &mut WireBuffer, row: u16, area_w: u16, active: Option<LogLevel>) {
    let mut x = 0u16;
    for (key, label, floor) in CHIPS {
        let is_active = floor == active;
        x = put_str(buf, x, row, "[", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, &key.to_string(), GOLD, area_w);
        x = put_str(buf, x, row, "]", MUTED_GRAY, area_w);
        let color = if is_active {
            SELECTION_GREEN
        } else {
            MUTED_GRAY
        };
        x = put_str(buf, x, row, label, color, area_w);
        x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
        if x >= area_w {
            break;
        }
    }
}

/// Render one log line: `{ts} {LVL} {target} {msg} {k=v…}`, level-coloured.
fn render_line(buf: &mut WireBuffer, row: u16, area_w: u16, line: &LogLine) {
    let level_color = level_color(line);
    let mut x = 0u16;

    // Timestamp (trimmed to the clock portion when it's an RFC3339 stamp).
    let ts = short_ts(&line.timestamp);
    if !ts.is_empty() {
        x = put_str(buf, x, row, ts, MUTED_GRAY, area_w);
        x += 1;
    }

    // Level token, coloured.
    if !line.level.is_empty() {
        x = put_str(buf, x, row, &line.level, level_color, area_w);
        x += 1;
    }

    // Target (module path), muted.
    if !line.target.is_empty() {
        x = put_str(buf, x, row, &line.target, MUTED_GRAY, area_w);
        x += 1;
    }

    // Message, primary.
    if !line.message.is_empty() {
        x = put_str(buf, x, row, &line.message, SOFT_WHITE, area_w);
        x += 1;
    }

    // The k=v field tail.
    for (k, v) in &line.fields {
        let kv = format!("{k}={v}");
        x = put_str(buf, x, row, &kv, MUTED_GRAY, area_w);
        x += 1;
        if x >= area_w {
            break;
        }
    }
}

/// The colour for a line's level token.
fn level_color(line: &LogLine) -> Color {
    match line.level_enum() {
        Some(LogLevel::Info) => INFO_BLUE,
        Some(LogLevel::Warn) => WARN_AMBER,
        Some(LogLevel::Error) => ERROR_RED,
        Some(LogLevel::Debug | LogLevel::Trace) => DEBUG_DIM,
        None => MUTED_GRAY,
    }
}

/// Trim an RFC3339 timestamp to its `HH:MM:SS` clock portion for a compact
/// column; pass anything non-RFC3339 through unchanged (tolerant of a blank /
/// odd `timestamp`). Char-safe.
fn short_ts(ts: &str) -> &str {
    // RFC3339: `<date>T<HH:MM:SS>.<frac>Z`. Slice the 8 clock chars after `T`.
    if let Some(t_idx) = ts.find('T') {
        let after_t = &ts[t_idx + 1..];
        if after_t.len() >= 8 && after_t.is_char_boundary(8) {
            return &after_t[..8];
        }
    }
    ts
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next
/// free column. Char-safe (iterates `char`s, not bytes — the utf8-truncate trap).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        put_cell(buf, cx, row, ch, color);
        cx = cx.saturating_add(1);
    }
    cx
}

/// Write a single coloured glyph at `(x, row)`.
fn put_cell(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

/// The level colours, exported so the snapshot test can assert per-level
/// colouring non-vacuously without re-declaring the RGB triples.
pub mod colors {
    use ainb_plugin_sdk::Color;

    /// `INFO` blue.
    pub const INFO: Color = super::INFO_BLUE;
    /// `WARN` amber.
    pub const WARN: Color = super::WARN_AMBER;
    /// `ERROR` red.
    pub const ERROR: Color = super::ERROR_RED;
    /// Active-chip highlight.
    pub const ACTIVE_CHIP: Color = super::SELECTION_GREEN;
}
