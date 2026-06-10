//! P8.5 — Daemon health pane: runtimes + claim cache + concurrency + dual-dim
//! throughput sparkline.
//!
//! The daemon-health screen (hotkey `D`) renders the daemon's in-memory health
//! snapshot (`hangar/daemon_health`): the registered provider runtimes (each with
//! a presence dot + pid), the bounded claim-slot cache occupancy as a fill bar,
//! the count of concurrently-executing tasks, and the rolling 60-second task
//! throughput as a [dual-dimension sparkline](crate::widgets::sparkline_dual) —
//! cell height = throughput, red proportion = failure rate.
//!
//! As with every Hangar screen the plugin owns **zero domain data**
//! (`project_ainb_plugin_owns_data_plane`): [`DaemonHealthState`] is built purely
//! from the wire [`DaemonHealthSnapshot`] the daemon hands back, and the renderer
//! is a pure width-aware paint over it (`project_ainb_tui_width_aware_panels`).

use ainb_hangar_proto::settings::{DaemonHealthSnapshot, RuntimeHealthRow, ThroughputSample};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::widgets::sparkline_dual::render_sparkline_dual;

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (labels, hints, empty bar track).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Connected runtime presence dot + claim-cache fill.
const ONLINE_GREEN: Color = Color::rgb(100, 200, 100);
/// Disconnected runtime presence dot.
const OFFLINE_RED: Color = Color::rgb(220, 120, 100);

/// The render-state cache for the daemon-health screen.
///
/// A flattened, render-ready view of the wire [`DaemonHealthSnapshot`]. Default
/// is the empty pane shown before the first `hangar/daemon_health` reply lands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DaemonHealthState {
    /// Registered provider runtimes.
    runtimes: Vec<RuntimeHealthRow>,
    /// Claim-slot cache slots in use.
    claim_used: u32,
    /// Claim-slot cache total capacity.
    claim_capacity: u32,
    /// Tasks currently dispatched / running.
    concurrent_tasks: u32,
    /// The rolling per-second throughput window (oldest-first).
    throughput: Vec<ThroughputSample>,
}

impl DaemonHealthState {
    /// Build the render state from a `hangar/daemon_health` snapshot.
    #[must_use]
    pub fn from_snapshot(snap: DaemonHealthSnapshot) -> Self {
        Self {
            runtimes: snap.runtimes,
            claim_used: snap.claim_cache.used,
            claim_capacity: snap.claim_cache.capacity,
            concurrent_tasks: snap.concurrent_tasks,
            throughput: snap.task_throughput_60s,
        }
    }

    /// The registered runtimes (read accessor for tests / glue).
    #[must_use]
    pub fn runtimes(&self) -> &[RuntimeHealthRow] {
        &self.runtimes
    }

    /// The rolling throughput window (read accessor for tests / glue).
    #[must_use]
    pub fn throughput(&self) -> &[ThroughputSample] {
        &self.throughput
    }
}

/// The sparkline's vertical resolution (rows) — enough cells that a 60-sample
/// monotone series produces visibly distinct heights.
const SPARK_ROWS: u16 = 8;

/// Render the daemon-health pane into `buf` between rows `top` and `bottom`.
///
/// Layout (top-to-bottom), per the P8.5 mockup:
///
/// ```text
/// Daemon health
/// runtime: claude   ● connected   pid 14829
/// claim cache: 12/64  ▓▓▓░░░░░ 19%
/// concurrent: 3 active
/// throughput (60s):
/// ▂▃▅█▆▄▃▂▁▂▃▅…   (dual-dim: height = throughput, red = failure rate)
/// ```
///
/// Width-aware: the claim-cache bar width is derived from `area_w`
/// (`project_ainb_tui_width_aware_panels`); the sparkline paints one column per
/// throughput sample (clipped to `area_w`). Strings truncate via `chars()`, never
/// byte-slice (the rust-utf8-truncate trap).
pub fn render_daemon_health(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &DaemonHealthState,
) {
    let mut row = top;
    put_str(buf, 0, row, "Daemon health", GOLD, area_w);
    row += 2;

    // Runtimes list (one row each): `runtime: <provider>  ● <status>  pid <pid>`.
    if state.runtimes.is_empty() {
        put_str(buf, 0, row, "no runtimes registered", MUTED_GRAY, area_w);
        row += 1;
    } else {
        for rt in &state.runtimes {
            if row > bottom {
                return;
            }
            render_runtime(buf, row, area_w, rt);
            row += 1;
        }
    }
    row += 1;

    // Claim-cache fill bar.
    if row <= bottom {
        render_claim_cache(buf, row, area_w, state.claim_used, state.claim_capacity);
        row += 1;
    }
    row += 1;

    // Concurrent-task count.
    if row <= bottom {
        let mut x = put_str(buf, 0, row, "concurrent: ", MUTED_GRAY, area_w);
        x = put_str(
            buf,
            x,
            row,
            &state.concurrent_tasks.to_string(),
            SOFT_WHITE,
            area_w,
        );
        put_str(buf, x + 1, row, "active", MUTED_GRAY, area_w);
        row += 2;
    }

    // Throughput sparkline header + the dual-dim sparkline beneath it.
    if row <= bottom {
        put_str(buf, 0, row, "throughput (60s):", MUTED_GRAY, area_w);
        row += 1;
    }
    let spark_rows = SPARK_ROWS.min(bottom.saturating_sub(row).saturating_add(1));
    if spark_rows > 0 && row <= bottom {
        // Clip the window to the available width so a narrow pane never overflows.
        let max_cols = area_w as usize;
        let samples: &[ThroughputSample] = if state.throughput.len() > max_cols {
            &state.throughput[state.throughput.len() - max_cols..]
        } else {
            &state.throughput
        };
        render_sparkline_dual(buf, 0, row, spark_rows, samples);
    }
}

/// Render one runtime row: provider, a presence dot coloured by `connected`, pid.
fn render_runtime(buf: &mut WireBuffer, row: u16, area_w: u16, rt: &RuntimeHealthRow) {
    let mut x = put_str(buf, 0, row, "runtime: ", MUTED_GRAY, area_w);
    x = put_str(buf, x, row, &rt.provider, SOFT_WHITE, area_w);
    x += 2;
    let (dot_color, status) = if rt.connected {
        (ONLINE_GREEN, "connected")
    } else {
        (OFFLINE_RED, "offline")
    };
    x = put_str(buf, x, row, "●", dot_color, area_w);
    x += 1;
    x = put_str(buf, x, row, status, dot_color, area_w);
    x += 2;
    let pid = format!("pid {}", rt.pid);
    put_str(buf, x, row, &pid, MUTED_GRAY, area_w);
}

/// Render the claim-cache fill bar: `claim cache: <used>/<cap>  ▓▓▓░░░ <pct>%`.
///
/// The bar width derives from `area_w` (a fixed share, capped) per the
/// width-aware panel pattern; the filled portion is `used/capacity` of it.
fn render_claim_cache(buf: &mut WireBuffer, row: u16, area_w: u16, used: u32, capacity: u32) {
    let label = format!("claim cache: {used}/{capacity}  ");
    let x = put_str(buf, 0, row, &label, MUTED_GRAY, area_w);

    // Bar width: a quarter of the pane, clamped to a sane [8, 24] band, and never
    // past the right edge.
    let bar_w = (area_w / 4).clamp(8, 24).min(area_w.saturating_sub(x + 6));
    let filled = if capacity == 0 {
        0
    } else {
        u16::try_from(u64::from(used) * u64::from(bar_w) / u64::from(capacity)).unwrap_or(bar_w)
    };
    let filled = filled.min(bar_w);
    let mut bx = x;
    for i in 0..bar_w {
        let (glyph, color) = if i < filled {
            ('▓', ONLINE_GREEN)
        } else {
            ('░', MUTED_GRAY)
        };
        put_cell(buf, bx, row, glyph, color);
        bx += 1;
    }
    let pct = if capacity == 0 {
        0
    } else {
        u64::from(used) * 100 / u64::from(capacity)
    };
    put_str(buf, bx + 1, row, &format!("{pct}%"), MUTED_GRAY, area_w);
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next free
/// column. Char-safe (iterates `char`s, not bytes — the utf8-truncate trap).
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
