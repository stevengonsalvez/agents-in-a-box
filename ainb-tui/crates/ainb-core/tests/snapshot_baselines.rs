// ABOUTME: Phase 5-prep snapshot baselines (Analytics screens + CLI usage
// JSON + statusline powerline). Captured BEFORE the plugin refactor so that
// later phases can tripwire against deterministic output.
//
// Phase 3 cutover: Analytics tabs now drive the burndown plugin via
// `PluginHost` instead of the in-tree component. The .snap files
// captured pre-cutover MUST still match byte-for-byte — that's the
// acceptance gate for closing 6tu.7.
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

use std::path::PathBuf;

use ainb::cli::statusline::{LiveCache, RateWindow, render_powerline};
use ainb::test_support::{cli_usage_report_json, sample_usage_data};
use ainb_plugin_api::{PluginEvent, RenderTarget};

/// Local mirror of the burndown plugin's `UsageTab` for snapshot driving.
/// Phase 3 cutover removed `ainb::components::usage` (the in-tree
/// Analytics UI) — these snapshots now drive the plugin via Custom
/// events, so the test only needs the tab name string.
#[derive(Clone, Copy)]
enum UsageTab {
    Daily,
    Weekly,
    Projects,
    Burndown,
}

fn workspace_dist() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("dist").join("plugins"))
        .expect("workspace root resolvable")
}

/// Drive the burndown plugin to render the requested tab against the
/// `sample_usage_data()` fixture and dump the result as a newline-separated,
/// right-trimmed text grid (matching the format the original
/// `TestBackend::buffer()` dumps used for the .snap baselines).
///
/// Soft-skips by returning `None` when `dist/plugins/burndown/plugin.wasm`
/// hasn't been built — keeps the suite green in CI without the WASI
/// toolchain.
fn render_analytics(tab: UsageTab, width: u16, height: u16) -> Option<String> {
    let _ = (width, height); // plugin currently paints fixed 80x24
    let dist = workspace_dist();
    if !dist.join("burndown").join("plugin.wasm").exists() {
        return None;
    }

    std::env::set_var("AINB_PLUGIN_ROOT", &dist);
    let (mut host, outcome) = ainb::plugins::init_plugin_host();
    assert!(outcome.failed.is_empty(), "plugin must load: {:?}", outcome.failed);

    // Push UsageData fixture.
    let data = sample_usage_data();
    let payload = serde_json::to_vec(&data).expect("UsageData -> json bytes");
    let ev = PluginEvent::Custom { topic: "burndown.usage_data".into(), payload };
    let bytes = rmp_serde::to_vec_named(&ev).unwrap();
    host.dispatch_event_bytes("burndown", &bytes).unwrap();

    // Push tab selection.
    let tab_name = match tab {
        UsageTab::Daily => "daily",
        UsageTab::Weekly => "weekly",
        UsageTab::Projects => "projects",
        UsageTab::Burndown => "burndown",
    };
    let tab_ev = PluginEvent::Custom {
        topic: "burndown.set_tab".into(),
        payload: serde_json::to_vec(&serde_json::json!({ "tab": tab_name })).expect("tab json bytes"),
    };
    host.dispatch_event_bytes("burndown", &rmp_serde::to_vec_named(&tab_ev).unwrap())
        .unwrap();

    host.render_plugin("burndown").unwrap();
    let buf = host.take_render("burndown", RenderTarget::Screen).unwrap();

    let mut out = String::with_capacity((buf.width as usize + 1) * buf.height as usize);
    for y in 0..buf.height {
        let mut line = String::with_capacity(buf.width as usize);
        for x in 0..buf.width {
            let i = usize::from(y) * usize::from(buf.width) + usize::from(x);
            line.push_str(&buf.cells[i].symbol);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    std::env::remove_var("AINB_PLUGIN_ROOT");
    Some(out)
}

fn baseline_settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_snapshot_path("baselines");
    s.set_prepend_module_to_snapshot(false);
    s.set_omit_expression(true);
    s
}

/// Run a snapshot test that drives the burndown plugin. Soft-skips
/// (passes) when `dist/plugins/burndown/plugin.wasm` hasn't been built.
fn assert_analytics_baseline(name: &str, tab: UsageTab) {
    let Some(dump) = render_analytics(tab, 80, 24) else {
        eprintln!(
            "skipping {name}: dist/plugins/burndown/plugin.wasm missing — \
             run scripts/build-plugins.sh"
        );
        return;
    };
    baseline_settings().bind(|| {
        insta::assert_snapshot!(name, dump);
    });
}

#[test]
fn analytics_daily() {
    assert_analytics_baseline("analytics_daily", UsageTab::Daily);
}

#[test]
fn analytics_weekly() {
    assert_analytics_baseline("analytics_weekly", UsageTab::Weekly);
}

#[test]
fn analytics_projects() {
    assert_analytics_baseline("analytics_projects", UsageTab::Projects);
}

#[test]
fn analytics_burndown() {
    assert_analytics_baseline("analytics_burndown", UsageTab::Burndown);
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
