//! e38.35 — Usage dashboard: token/cost totals + per-agent rollup.
//!
//! The usage-dashboard screen (hotkey `U`) renders the daemon's `task_usage`
//! rollup (`hangar/usage_rollup`): the workspace's grand total tokens in/out +
//! cost across every recorded run, then a per-agent breakdown table (each agent's
//! summed tokens + cost + run count, heaviest cost first). Mirrors the reference's
//! usage-rollup surface.
//!
//! As with every Hangar screen the plugin owns **zero domain data**
//! (`project_ainb_plugin_owns_data_plane`): [`UsageState`] is built purely from
//! the wire [`UsageRollupResult`] the daemon hands back, and the renderer is a
//! pure width-aware paint over it (`project_ainb_tui_width_aware_panels`).

use ainb_hangar_proto::snapshots::{
    AgentUsageRow, RunHistoryResult, RunHistoryRow, UsageRollupResult,
};
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
/// A flattened, render-ready view of the wire [`UsageRollupResult`] (the totals +
/// per-agent rollup, `hangar/usage_rollup`) AND the [`RunHistoryResult`] timeline
/// (the recent-runs section, `hangar/run_history`, P10 / D19). The two snapshots
/// arrive on independent replies, so within a single workspace they update in
/// place ([`Self::apply_rollup`] / [`Self::apply_run_history`]) — one landing
/// never wipes the other.
///
/// # Workspace generation scoping
///
/// Both halves are tagged with the workspace they were fetched for ([`Self::ws`]).
/// A `WorkspaceAction::SetActive` switch re-fetches BOTH snapshots, but they land
/// on independent replies (and either can lag, error, or arrive malformed). The
/// in-place merge only preserves the complementary half when the incoming reply
/// carries the SAME workspace; the FIRST reply of a new workspace resets the whole
/// state before applying, so a lagging/failed run-history can never leave the new
/// workspace's totals sitting next to the prior workspace's recent-runs timeline
/// (a cross-tenant stale-data leak). See `project_ainb_plugin_owns_data_plane`.
///
/// Default is the empty pane shown before either reply lands (and the genuine
/// zero-usage state — a workspace that has run nothing).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageState {
    /// The workspace both halves were last fetched for. `None` until the first
    /// reply lands. A reply carrying a different workspace resets the state before
    /// applying, so the two halves can never straddle a tenant boundary.
    ws: Option<String>,
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
    /// The recent-runs timeline rows, newest finished first (P10 / D19).
    runs: Vec<RunHistoryRow>,
}

impl UsageState {
    /// Build the render state from a `hangar/usage_rollup` snapshot for workspace
    /// `ws` (an empty run-history timeline until `hangar/run_history` lands).
    #[must_use]
    pub fn from_rollup(ws: &str, rollup: UsageRollupResult) -> Self {
        let mut s = Self::default();
        s.apply_rollup(ws, rollup);
        s
    }

    /// Reset the whole state when a reply arrives for a workspace other than the
    /// one both halves currently hold. Within the SAME workspace this is a no-op,
    /// so the two independent replies still merge in place; on a workspace switch
    /// it wipes the prior tenant's half so a lagging complementary reply can never
    /// leave stale cross-workspace data on screen.
    fn enter_ws(&mut self, ws: &str) {
        if self.ws.as_deref() != Some(ws) {
            *self = Self {
                ws: Some(ws.to_string()),
                ..Self::default()
            };
        }
    }

    /// Update the rollup-derived fields (totals + per-agent) for workspace `ws`.
    /// PRESERVES the run-history timeline when it belongs to the SAME workspace
    /// (the two snapshots arrive on separate replies); RESETS the whole state
    /// first when `ws` differs from the currently-held workspace, so a stale
    /// prior-tenant timeline is never shown beside fresh totals.
    pub fn apply_rollup(&mut self, ws: &str, rollup: UsageRollupResult) {
        self.enter_ws(ws);
        self.total_input_tokens = rollup.total_input_tokens;
        self.total_output_tokens = rollup.total_output_tokens;
        self.total_cost_usd = rollup.total_cost_usd;
        self.total_runs = rollup.total_runs;
        self.agents = rollup.agents;
    }

    /// Update the recent-runs timeline for workspace `ws` from a
    /// `hangar/run_history` snapshot (P10 / D19). PRESERVES the rollup totals +
    /// per-agent breakdown when they belong to the SAME workspace; RESETS the
    /// whole state first when `ws` differs, so stale prior-tenant totals are never
    /// shown beside a fresh timeline.
    pub fn apply_run_history(&mut self, ws: &str, history: RunHistoryResult) {
        self.enter_ws(ws);
        self.runs = history.runs;
    }

    /// The per-agent rows (read accessor for tests / glue).
    #[must_use]
    pub fn agents(&self) -> &[AgentUsageRow] {
        &self.agents
    }

    /// The recent-runs timeline rows (read accessor for tests / glue).
    #[must_use]
    pub fn runs(&self) -> &[RunHistoryRow] {
        &self.runs
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
    } else {
        for agent in &state.agents {
            if row > bottom {
                return;
            }
            render_agent_row(buf, row, area_w, agent);
            row += 1;
        }
    }

    // Recent-runs timeline (P10 / D19), below the per-agent breakdown.
    row += 1;
    if row <= bottom {
        put_str(buf, 0, row, "recent runs", MUTED_GRAY, area_w);
        row += 1;
    }
    if state.runs.is_empty() {
        if row <= bottom {
            put_str(buf, 0, row, "no runs recorded yet", MUTED_GRAY, area_w);
        }
        return;
    }
    for run in &state.runs {
        if row > bottom {
            return;
        }
        render_run_row(buf, row, area_w, run);
        row += 1;
    }
}

/// Success/failure glyph colour for a run outcome.
const RUN_OK_GREEN: Color = Color::rgb(120, 200, 130);
/// Failure glyph colour.
const RUN_FAIL_RED: Color = Color::rgb(220, 110, 110);

/// Render one recent-run row:
/// `<glyph> <provider>  <outcome>  <in> in  <out> out  <cost>  <dur>`.
fn render_run_row(buf: &mut WireBuffer, row: u16, area_w: u16, run: &RunHistoryRow) {
    let ok = run.outcome == "success";
    let (glyph, glyph_color) = if ok {
        ("✓", RUN_OK_GREEN)
    } else {
        ("✗", RUN_FAIL_RED)
    };
    let mut x = put_str(buf, 0, row, glyph, glyph_color, area_w);
    x += 1;
    x = put_str(buf, x, row, &run.provider, SOFT_WHITE, area_w);
    x += 2;
    x = put_str(
        buf,
        x,
        row,
        &run.outcome,
        if ok { RUN_OK_GREEN } else { RUN_FAIL_RED },
        area_w,
    );
    x += 2;
    x = put_str(
        buf,
        x,
        row,
        &fmt_tokens(run.input_tokens),
        SOFT_WHITE,
        area_w,
    );
    x = put_str(buf, x, row, " in  ", MUTED_GRAY, area_w);
    x = put_str(
        buf,
        x,
        row,
        &fmt_tokens(run.output_tokens),
        SOFT_WHITE,
        area_w,
    );
    x = put_str(buf, x, row, " out  ", MUTED_GRAY, area_w);
    x = put_str(buf, x, row, &fmt_cost(run.cost_usd), COST_GREEN, area_w);
    let dur = format!("  {}", fmt_duration(run.started_at, run.finished_at));
    put_str(buf, x, row, &dur, MUTED_GRAY, area_w);
}

/// Format a run's duration from its start/finish epoch-ms stamps as a compact
/// `1.4s` / `2m03s`. A missing/negative span renders `-` (no start recorded).
///
/// The `i64 -> f64` cast is for display rounding only; millisecond spans are well
/// within f64's exact-integer range.
#[allow(clippy::cast_precision_loss)]
fn fmt_duration(started_at: Option<i64>, finished_at: i64) -> String {
    let Some(start) = started_at else {
        return "-".to_string();
    };
    let ms = finished_at - start;
    if ms < 0 {
        return "-".to_string();
    }
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let secs = ms / 1000;
        format!("{}m{:02}s", secs / 60, secs % 60)
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

    fn run_row(
        run_id: &str,
        provider: &str,
        outcome: &str,
        tin: i64,
        tout: i64,
        cost: f64,
        started: Option<i64>,
        finished: i64,
    ) -> RunHistoryRow {
        RunHistoryRow {
            run_id: run_id.into(),
            task_id: None,
            session_id: None,
            provider: provider.into(),
            profile: None,
            started_at: started,
            finished_at: finished,
            outcome: outcome.into(),
            input_tokens: tin,
            output_tokens: tout,
            cost_usd: cost,
            diff_add: 0,
            diff_del: 0,
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
        let state = UsageState::from_rollup("ws-a", rollup);
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
        // The recent-runs section shows its own empty hint below.
        assert_eq!(row_text(&buf, 6, 80), "recent runs");
        assert_eq!(row_text(&buf, 7, 80), "no runs recorded yet");
    }

    #[test]
    fn recent_runs_render_with_outcome_and_duration() {
        let mut state = UsageState::from_rollup(
            "ws-a",
            UsageRollupResult {
                total_input_tokens: 1000,
                total_output_tokens: 200,
                total_cost_usd: 0.02,
                total_runs: 2,
                agents: vec![agent("claude-agent", 1000, 200, 0.02, 2)],
            },
        );
        state.apply_run_history(
            "ws-a",
            RunHistoryResult {
                runs: vec![
                    run_row("r2", "codex", "success", 1500, 340, 0.0231, Some(0), 1400),
                    run_row("r1", "claude", "failed", 500, 0, 0.0, Some(0), 800),
                ],
            },
        );
        let mut buf = WireBuffer::new(80, 24);
        render_usage(&mut buf, 80, 0, 22, &state);

        // Find the "recent runs" header row, then assert the two run rows below it.
        let header = (0..22)
            .find(|&r| row_text(&buf, r, 80) == "recent runs")
            .expect("recent runs header rendered");
        let r0 = row_text(&buf, header + 1, 80);
        assert!(r0.contains('✓'), "success glyph: {r0}");
        assert!(r0.contains("codex"), "provider: {r0}");
        assert!(r0.contains("success"), "outcome: {r0}");
        assert!(r0.contains("$0.0231"), "cost: {r0}");
        assert!(r0.contains("1.4s"), "duration 1400ms -> 1.4s: {r0}");
        let r1 = row_text(&buf, header + 2, 80);
        assert!(r1.contains('✗'), "failure glyph: {r1}");
        assert!(r1.contains("failed"), "outcome: {r1}");
    }

    #[test]
    fn rollup_and_run_history_updates_do_not_wipe_each_other() {
        // Run history lands first (workspace A).
        let mut state = UsageState::default();
        state.apply_run_history(
            "ws-a",
            RunHistoryResult {
                runs: vec![run_row(
                    "r1",
                    "claude",
                    "success",
                    10,
                    5,
                    0.001,
                    Some(0),
                    500,
                )],
            },
        );
        assert_eq!(state.runs().len(), 1);

        // Then the rollup lands for the SAME workspace — the runs must survive.
        state.apply_rollup(
            "ws-a",
            UsageRollupResult {
                total_input_tokens: 10,
                total_output_tokens: 5,
                total_cost_usd: 0.001,
                total_runs: 1,
                agents: vec![agent("claude-agent", 10, 5, 0.001, 1)],
            },
        );
        assert_eq!(state.runs().len(), 1, "rollup update must not wipe runs");
        assert_eq!(state.agents().len(), 1);

        // A fresh run-history reply for the SAME workspace must not wipe totals.
        state.apply_run_history(
            "ws-a",
            RunHistoryResult {
                runs: vec![
                    run_row("r2", "codex", "success", 20, 8, 0.002, Some(0), 700),
                    run_row("r1", "claude", "success", 10, 5, 0.001, Some(0), 500),
                ],
            },
        );
        assert_eq!(state.runs().len(), 2);
        assert!(
            (state.total_cost_usd() - 0.001).abs() < 1e-9,
            "totals preserved"
        );
        assert_eq!(state.agents().len(), 1, "per-agent rollup preserved");
    }

    #[test]
    fn rollup_for_new_workspace_clears_prior_run_history() {
        // Workspace A is fully populated (both halves).
        let mut state = UsageState::from_rollup(
            "ws-a",
            UsageRollupResult {
                total_input_tokens: 1000,
                total_output_tokens: 200,
                total_cost_usd: 0.02,
                total_runs: 2,
                agents: vec![agent("claude-agent", 1000, 200, 0.02, 2)],
            },
        );
        state.apply_run_history(
            "ws-a",
            RunHistoryResult {
                runs: vec![run_row(
                    "r1",
                    "claude",
                    "success",
                    10,
                    5,
                    0.001,
                    Some(0),
                    500,
                )],
            },
        );
        assert_eq!(state.runs().len(), 1);

        // Switch to workspace B: the rollup reply lands FIRST. Workspace B's totals
        // must NOT be shown beside workspace A's recent-runs timeline. The stale
        // timeline is cleared until B's run-history reply lands (and if it lags,
        // errors, or arrives malformed, the timeline simply stays empty).
        state.apply_rollup(
            "ws-b",
            UsageRollupResult {
                total_input_tokens: 5,
                total_output_tokens: 2,
                total_cost_usd: 0.5,
                total_runs: 1,
                agents: vec![agent("codex-agent", 5, 2, 0.5, 1)],
            },
        );
        assert!(
            state.runs().is_empty(),
            "workspace switch must clear the prior tenant's run-history timeline"
        );
        assert!(
            (state.total_cost_usd() - 0.5).abs() < 1e-9,
            "new workspace totals applied"
        );
    }

    #[test]
    fn run_history_for_new_workspace_clears_prior_rollup() {
        // Workspace A fully populated.
        let mut state = UsageState::from_rollup(
            "ws-a",
            UsageRollupResult {
                total_input_tokens: 1000,
                total_output_tokens: 200,
                total_cost_usd: 0.02,
                total_runs: 2,
                agents: vec![agent("claude-agent", 1000, 200, 0.02, 2)],
            },
        );
        state.apply_run_history(
            "ws-a",
            RunHistoryResult {
                runs: vec![run_row(
                    "r1",
                    "claude",
                    "success",
                    10,
                    5,
                    0.001,
                    Some(0),
                    500,
                )],
            },
        );

        // Switch to workspace B: the run-history reply lands FIRST. Workspace A's
        // totals + per-agent breakdown must be wiped so a lagging B rollup can't
        // leave stale cross-workspace totals beside B's fresh timeline.
        state.apply_run_history(
            "ws-b",
            RunHistoryResult {
                runs: vec![run_row("r9", "codex", "success", 3, 1, 0.003, Some(0), 300)],
            },
        );
        assert_eq!(state.runs().len(), 1);
        assert!(
            state.agents().is_empty(),
            "prior workspace per-agent rollup cleared"
        );
        assert!(
            state.total_cost_usd().abs() < 1e-9,
            "prior workspace totals cleared until new rollup lands"
        );
    }
}
