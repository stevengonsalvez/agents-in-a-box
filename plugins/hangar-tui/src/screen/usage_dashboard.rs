//! e38.35 — Usage dashboard: token/cost totals + per-agent rollup.
//!
//! The usage-dashboard screen (hotkey `U`) renders the daemon's `task_usage`
//! rollup (`hangar/usage_rollup`): the workspace's grand total tokens in/out +
//! cost across every recorded run, then a per-agent breakdown table (each agent's
//! summed tokens + cost + run count, heaviest cost first). Mirrors Multica's
//! usage-rollup surface.
//!
//! As with every Hangar screen the plugin owns **zero domain data**
//! (`project_ainb_plugin_owns_data_plane`): [`UsageState`] is built purely from
//! the wire [`UsageRollupResult`] the daemon hands back, and the renderer is a
//! pure width-aware paint over it (`project_ainb_tui_width_aware_panels`).

use ainb_hangar_proto::snapshots::{AgentUsageRow, UsageRollupResult};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text (figures).
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (labels, hints, empty state).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Cost figures get a distinct green so spend stands out from token counts.
const COST_GREEN: Color = Color::rgb(120, 200, 130);

/// The render-state cache for the usage-dashboard screen.
///
/// A flattened, render-ready view of the wire [`UsageRollupResult`]. Default is
/// the empty pane shown before the first `hangar/usage_rollup` reply lands (and
/// the genuine zero-usage state — a workspace that has run nothing).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageState {
    /// Grand total input tokens across every recorded run.
    total_input_tokens: i64,
    /// Grand total output tokens across every recorded run.
    total_output_tokens: i64,
    /// Grand total cost (USD) across every recorded run.
    total_cost_usd: f64,
    /// Number of recorded runs the totals aggregate.
    total_runs: i64,
    /// The per-agent breakdown rows, heaviest cost first.
    agents: Vec<AgentUsageRow>,
}

impl UsageState {
    /// Build the render state from a `hangar/usage_rollup` snapshot.
    #[must_use]
    pub fn from_rollup(rollup: UsageRollupResult) -> Self {
        Self {
            total_input_tokens: rollup.total_input_tokens,
            total_output_tokens: rollup.total_output_tokens,
            total_cost_usd: rollup.total_cost_usd,
            total_runs: rollup.total_runs,
            agents: rollup.agents,
        }
    }

    /// The per-agent rows (read accessor for tests / glue).
    #[must_use]
    pub fn agents(&self) -> &[AgentUsageRow] {
        &self.agents
    }

    /// The grand total cost (read accessor for tests / glue).
    #[must_use]
    pub const fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }
}

/// Render the usage-dashboard pane into `buf` between rows `top` and `bottom`.
///
/// Layout (top-to-bottom):
///
/// ```text
/// Usage
/// total: 1.2M in · 340K out · $0.0231   (12 runs)
///
/// per agent
/// claude-agent   1.0M in   300K out   $0.0200   10 runs
/// codex-agent    200K in    40K out   $0.0031    2 runs
/// ```
///
/// Width-aware: every string clips at `area_w`. Strings truncate via `chars()`,
/// never byte-slice (the rust-utf8-truncate trap).
pub fn render_usage(buf: &mut WireBuffer, area_w: u16, top: u16, bottom: u16, state: &UsageState) {
    let mut row = top;
    put_str(buf, 0, row, "Usage", GOLD, area_w);
    row += 2;

    // Grand-total line: tokens in/out · cost · run count.
    if row <= bottom {
        let mut x = put_str(buf, 0, row, "total: ", MUTED_GRAY, area_w);
        x = put_str(
            buf,
            x,
            row,
            &fmt_tokens(state.total_input_tokens),
            SOFT_WHITE,
            area_w,
        );
        x = put_str(buf, x, row, " in  ", MUTED_GRAY, area_w);
        x = put_str(
            buf,
            x,
            row,
            &fmt_tokens(state.total_output_tokens),
            SOFT_WHITE,
            area_w,
        );
        x = put_str(buf, x, row, " out  ", MUTED_GRAY, area_w);
        x = put_str(
            buf,
            x,
            row,
            &fmt_cost(state.total_cost_usd),
            COST_GREEN,
            area_w,
        );
        let runs = format!("  ({} runs)", state.total_runs);
        put_str(buf, x, row, &runs, MUTED_GRAY, area_w);
        row += 2;
    }

    // Per-agent breakdown header + rows.
    if row <= bottom {
        put_str(buf, 0, row, "per agent", MUTED_GRAY, area_w);
        row += 1;
    }
    if state.agents.is_empty() {
        if row <= bottom {
            put_str(buf, 0, row, "no usage recorded yet", MUTED_GRAY, area_w);
        }
        return;
    }
    for agent in &state.agents {
        if row > bottom {
            return;
        }
        render_agent_row(buf, row, area_w, agent);
        row += 1;
    }
}

/// Render one per-agent row: `<agent>  <in> in  <out> out  <cost>  <runs> runs`.
fn render_agent_row(buf: &mut WireBuffer, row: u16, area_w: u16, agent: &AgentUsageRow) {
    let mut x = put_str(buf, 0, row, &agent.agent_id, SOFT_WHITE, area_w);
    x += 2;
    x = put_str(
        buf,
        x,
        row,
        &fmt_tokens(agent.input_tokens),
        SOFT_WHITE,
        area_w,
    );
    x = put_str(buf, x, row, " in  ", MUTED_GRAY, area_w);
    x = put_str(
        buf,
        x,
        row,
        &fmt_tokens(agent.output_tokens),
        SOFT_WHITE,
        area_w,
    );
    x = put_str(buf, x, row, " out  ", MUTED_GRAY, area_w);
    x = put_str(buf, x, row, &fmt_cost(agent.cost_usd), COST_GREEN, area_w);
    let runs = format!("  {} runs", agent.runs);
    put_str(buf, x, row, &runs, MUTED_GRAY, area_w);
}

/// Compact a token count: `1500` -> `1.5K`, `2_000_000` -> `2.0M`, small as-is.
///
/// The `i64 -> f64` cast is for display rounding only; token counts are well
/// within f64's exact-integer range (2^52), so the precision-loss lint does not
/// apply to the magnitudes this renders.
#[allow(clippy::cast_precision_loss)]
fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a USD cost with four decimals (a single run can be sub-cent): `$0.0231`.
fn fmt_cost(usd: f64) -> String {
    format!("${usd:.4}")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, tin: i64, tout: i64, cost: f64, runs: i64) -> AgentUsageRow {
        AgentUsageRow {
            agent_id: id.into(),
            input_tokens: tin,
            output_tokens: tout,
            cost_usd: cost,
            runs,
        }
    }

    /// Collect the rendered glyphs at `row` into a string (column-ordered).
    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            let ch = buf
                .cells
                .iter()
                .find(|(coord, _)| coord.x == x && coord.y == row)
                .map_or(' ', |(_, c)| c.symbol.chars().next().unwrap_or(' '));
            s.push(ch);
        }
        s.trim_end().to_string()
    }

    #[test]
    fn renders_totals_and_per_agent_rows() {
        let rollup = UsageRollupResult {
            total_input_tokens: 1_500_000,
            total_output_tokens: 340_000,
            total_cost_usd: 0.0231,
            total_runs: 12,
            agents: vec![
                agent("claude-agent", 1_300_000, 300_000, 0.0200, 10),
                agent("codex-agent", 200_000, 40_000, 0.0031, 2),
            ],
        };
        let state = UsageState::from_rollup(rollup);
        let mut buf = WireBuffer::new(80, 24);
        render_usage(&mut buf, 80, 0, 20, &state);

        // Title.
        assert_eq!(row_text(&buf, 0, 80), "Usage");
        // Total line carries the compacted tokens, the cost, and the run count.
        let total = row_text(&buf, 2, 80);
        assert!(total.contains("total:"), "total line: {total}");
        assert!(total.contains("1.5M"), "input tokens compacted: {total}");
        assert!(total.contains("340.0K"), "output tokens compacted: {total}");
        assert!(total.contains("$0.0231"), "cost rendered: {total}");
        assert!(total.contains("12 runs"), "run count rendered: {total}");

        // The per-agent header + both agent rows render (header row 4, agents 5/6).
        assert_eq!(row_text(&buf, 4, 80), "per agent");
        let a0 = row_text(&buf, 5, 80);
        assert!(a0.contains("claude-agent"), "agent row 0: {a0}");
        assert!(a0.contains("$0.0200"), "agent row 0 cost: {a0}");
        assert!(a0.contains("10 runs"), "agent row 0 runs: {a0}");
        let a1 = row_text(&buf, 6, 80);
        assert!(a1.contains("codex-agent"), "agent row 1: {a1}");
    }

    #[test]
    fn empty_state_renders_placeholder() {
        let state = UsageState::default();
        let mut buf = WireBuffer::new(80, 24);
        render_usage(&mut buf, 80, 0, 20, &state);
        assert_eq!(row_text(&buf, 0, 80), "Usage");
        // Zero totals still render a $0.0000 cost line, then the no-usage hint.
        let total = row_text(&buf, 2, 80);
        assert!(total.contains("$0.0000"), "zero cost line: {total}");
        assert_eq!(row_text(&buf, 5, 80), "no usage recorded yet");
    }
}
