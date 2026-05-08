//! Tripwire: drive the burndown plugin via PluginHost and assert the
//! rendered text dump matches the in-tree `components::usage::render`
//! output byte-identically for the same fixture.
//!
//! This is the gate the leader called out in the override: "snapshot_baselines
//! tripwire MUST be byte-identical to agent-3 baselines". We don't recompare
//! against the .snap files directly here — that lands once
//! `tests/snapshot_baselines.rs` switches to driving the plugin. This test
//! is the structural prerequisite: it proves the plugin produces the same
//! cell grid the in-tree render did, for the same input.
//!
//! Soft-skips when `dist/plugins/burndown/plugin.wasm` hasn't been built —
//! agent dev loops without the WASI toolchain still pass the test suite.

#![cfg(feature = "test-support")]
#![allow(missing_docs)]

use std::path::PathBuf;

use ainb::components::usage::{self as usage, UsageTab, UsageViewState};
use ainb::test_support::sample_usage_data;
use ainb_plugin_api::{PluginEvent, RenderTarget};
use ratatui::{Terminal, backend::TestBackend};

fn workspace_dist() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("dist").join("plugins"))
        .expect("workspace root resolvable")
}

/// Render Analytics in-tree (the pre-cutover code path) → text dump.
fn in_tree_dump(tab: UsageTab, w: u16, h: u16) -> String {
    let mut state = UsageViewState::default();
    state.active_tab = tab;
    state.data = Some(sample_usage_data());

    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| usage::render(frame, frame.size(), &state))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let area = *buffer.area();
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
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

/// Drive the plugin: push UsageData via _handle_event, render, drain.
/// Convert WireBuffer → text dump in the same right-trimmed format as
/// `in_tree_dump`.
fn plugin_dump(tab: UsageTab) -> Option<String> {
    let dist = workspace_dist();
    if !dist.join("burndown").join("plugin.wasm").exists() {
        return None;
    }

    std::env::set_var("AINB_PLUGIN_ROOT", &dist);
    let (mut host, outcome) = ainb::plugins::init_plugin_host();
    assert!(outcome.failed.is_empty(), "plugin must load: {:?}", outcome.failed);

    // Wrap the sample UsageData inside a CustomEvent that the plugin
    // recognises (`burndown.usage_data`) and ship it via msgpack.
    let mut state = UsageViewState::default();
    state.active_tab = tab;
    state.data = Some(sample_usage_data());

    let payload = serde_json::to_value(state.data.as_ref().unwrap())
        .expect("UsageData -> json");
    let ev = PluginEvent::Custom {
        topic: "burndown.usage_data".to_string(),
        payload,
    };
    let bytes = rmp_serde::to_vec_named(&ev).expect("encode event");
    host.dispatch_event_bytes("burndown", &bytes)
        .expect("plugin handles event");

    // Drive tab selection through the same Custom-event channel.
    let tab_name = match tab {
        UsageTab::Daily => "daily",
        UsageTab::Weekly => "weekly",
        UsageTab::Projects => "projects",
        UsageTab::Burndown => "burndown",
        UsageTab::Optimize => "optimize",
    };
    let tab_ev = PluginEvent::Custom {
        topic: "burndown.set_tab".to_string(),
        payload: serde_json::json!({ "tab": tab_name }),
    };
    let tab_bytes = rmp_serde::to_vec_named(&tab_ev).expect("encode tab event");
    host.dispatch_event_bytes("burndown", &tab_bytes)
        .expect("plugin handles tab event");

    host.render_plugin("burndown").expect("_render runs");
    let buf = host
        .take_render("burndown", RenderTarget::Screen)
        .expect("plugin painted");
    let mut out =
        String::with_capacity((buf.width as usize + 1) * buf.height as usize);
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

fn assert_parity(tab: UsageTab) {
    let in_tree = in_tree_dump(tab, 80, 24);
    let Some(plugin) = plugin_dump(tab) else {
        eprintln!(
            "skipping {:?}: dist/plugins/burndown/plugin.wasm missing — \
             run scripts/build-plugins.sh",
            tab
        );
        return;
    };

    if in_tree != plugin {
        let in_lines: Vec<&str> = in_tree.lines().collect();
        let pl_lines: Vec<&str> = plugin.lines().collect();
        let max = in_lines.len().max(pl_lines.len());
        for i in 0..max {
            let a = in_lines.get(i).copied().unwrap_or("<missing>");
            let b = pl_lines.get(i).copied().unwrap_or("<missing>");
            if a != b {
                eprintln!("row {i}:");
                eprintln!("  in-tree: {a:?}");
                eprintln!("  plugin : {b:?}");
            }
        }
        panic!("plugin dump differs from in-tree dump for {:?}", tab);
    }
}

#[test]
fn plugin_render_matches_in_tree_for_daily_tab() {
    assert_parity(UsageTab::Daily);
}

#[test]
fn plugin_render_matches_in_tree_for_weekly_tab() {
    assert_parity(UsageTab::Weekly);
}

#[test]
fn plugin_render_matches_in_tree_for_projects_tab() {
    assert_parity(UsageTab::Projects);
}

#[test]
fn plugin_render_matches_in_tree_for_burndown_tab() {
    assert_parity(UsageTab::Burndown);
}
