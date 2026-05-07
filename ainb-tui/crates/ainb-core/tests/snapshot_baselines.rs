// ABOUTME: Phase 5-prep snapshot baselines (Analytics screens + CLI usage
// JSON + statusline powerline). Captured BEFORE the plugin refactor so that
// later phases can tripwire against deterministic output.
//
// Baselines live in `tests/baselines/` (configured per-test via
// `insta::Settings::set_snapshot_path`). Determinism rules:
//   - Fixture comes from `ainb::test_support::sample_usage_data()` —
//     timestamps frozen to 2026-04-29T10:00:00 UTC; numeric counters and
//     project metadata are constants.
//   - The statusline cache is built from a hand-rolled struct literal
//     with fixed `updated_at`, fixed pct, and fixed model — no wall clock.
//   - No RNG. No environment lookups (`CLAUDE_CONFIG_DIR` etc. are not
//     consulted by these helpers).
//
// Re-baseline cycle:
//   INSTA_UPDATE=always cargo test -p ainb --features test-support \
//     --test snapshot_baselines
//   cargo test -p ainb --features test-support --test snapshot_baselines

#![cfg(feature = "test-support")]
#![allow(missing_docs)]

use ainb::cli::statusline::{LiveCache, RateWindow, render_powerline};
use ainb::components::usage::{self, UsageTab, UsageViewState};
use ainb::test_support::{cli_usage_report_json, sample_usage_data};
use ratatui::{Terminal, backend::TestBackend};

/// Render the Analytics screen at the given size with the given tab and
/// dump the resulting buffer cell grid as a newline-separated string.
/// Trailing whitespace is trimmed per row so right-padded blanks don't
/// pollute the diff when terminals re-flow.
fn render_analytics(tab: UsageTab, width: u16, height: u16) -> String {
    let mut state = UsageViewState::default();
    state.active_tab = tab;
    state.data = Some(sample_usage_data());

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| usage::render(frame, frame.size(), &state))
        .expect("draw");
    dump_buffer(&terminal)
}

fn dump_buffer(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let area = *buffer.area();
    let mut out =
        String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        let mut line = String::with_capacity(area.width as usize);
        for x in 0..area.width {
            line.push_str(buffer.get(x, y).symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn baseline_settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_snapshot_path("baselines");
    s.set_prepend_module_to_snapshot(false);
    s.set_omit_expression(true);
    s
}

#[test]
fn analytics_daily() {
    let dump = render_analytics(UsageTab::Daily, 80, 24);
    baseline_settings().bind(|| {
        insta::assert_snapshot!("analytics_daily", dump);
    });
}

#[test]
fn analytics_weekly() {
    let dump = render_analytics(UsageTab::Weekly, 80, 24);
    baseline_settings().bind(|| {
        insta::assert_snapshot!("analytics_weekly", dump);
    });
}

#[test]
fn analytics_projects() {
    let dump = render_analytics(UsageTab::Projects, 80, 24);
    baseline_settings().bind(|| {
        insta::assert_snapshot!("analytics_projects", dump);
    });
}

#[test]
fn analytics_burndown() {
    let dump = render_analytics(UsageTab::Burndown, 80, 24);
    baseline_settings().bind(|| {
        insta::assert_snapshot!("analytics_burndown", dump);
    });
}

#[test]
fn usage_cli_json_report() {
    let data = sample_usage_data();
    let report = cli_usage_report_json(&data);
    baseline_settings().bind(|| {
        insta::assert_json_snapshot!("usage_cli_json_report", report);
    });
}

#[test]
fn statusline_budget_bar() {
    // Hand-rolled cache with frozen timestamp / pct / cost so the rendered
    // ANSI line is byte-identical run to run.
    let cache = LiveCache {
        version: 1,
        updated_at: "2026-04-29T10:00:00Z".to_string(),
        five_hour: Some(RateWindow {
            pct: 42,
            resets_at: Some("2026-04-29T15:00:00Z".to_string()),
        }),
        seven_day: Some(RateWindow {
            pct: 73,
            resets_at: Some("2026-05-06T10:00:00Z".to_string()),
        }),
        today_cost_usd: Some(2.34),
        context_pct: Some(58),
        model: Some("claude-sonnet-4-5".to_string()),
    };
    let line = render_powerline(&cache);
    baseline_settings().bind(|| {
        insta::assert_snapshot!("statusline_budget_bar", line);
    });
}
