//! Phase 6c integration test: burndown subscribes to sessions.usage_data,
//! receives a publisher's msgpack `UsageDataEvent`, and re-renders to a
//! non-empty `WireBuffer`.
//!
//! Bus model: the loader auto-subscribes burndown to topics declared in
//! its `[subscribes]` block. A separate publisher (here: a hand-rolled
//! test plugin id, no wasm needed) calls `HostShared::publish` directly.
//! `pump_events` then routes the event through subscribers' wasmi
//! `_handle_event` exports.
//!
//! Pre-req: burndown's wasm artifact + manifest staged in
//! `<workspace>/dist/plugins/burndown/`. The harness skips with an
//! explanatory message when the artifact is missing instead of
//! failing CI on a forgotten `scripts/build-plugins.sh` run.

use std::path::PathBuf;

use ainb_plugin_api::{Manifest, RenderTarget};
use ainb_plugin_host::PluginHost;
use ainb_plugin_types_sessions::{
    ProjectUsage, TokenBucket, UsageData, UsageDataEvent, WIRE_VERSION,
};
use chrono::NaiveDate;

const BURNDOWN_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../dist/plugins/burndown"
);

fn burndown_artifacts() -> Option<(Manifest, Vec<u8>)> {
    let dir = PathBuf::from(BURNDOWN_DIR);
    let toml_path = dir.join("plugin.toml");
    let wasm_path = dir.join("plugin.wasm");
    if !toml_path.is_file() || !wasm_path.is_file() {
        return None;
    }
    let toml_src = std::fs::read_to_string(&toml_path).ok()?;
    let manifest = Manifest::from_toml(&toml_src).ok()?;
    let wasm = std::fs::read(&wasm_path).ok()?;
    Some((manifest, wasm))
}

fn fixture_event() -> UsageDataEvent {
    let bucket = TokenBucket {
        input_tokens: 100,
        cache_creation_tokens: 0,
        cache_read_tokens: 50,
        output_tokens: 200,
        reasoning_tokens: 0,
        session_count: 1,
        project_count: 1,
        call_count: 1,
        cost_usd: Some(0.005),
    };
    let day = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
    UsageDataEvent {
        version: WIRE_VERSION,
        published_ns: 0,
        partial: false,
        data: UsageData {
            daily: vec![(day, bucket.clone())],
            weekly: vec![(day, bucket.clone())],
            projects: vec![ProjectUsage {
                name: "ainb".into(),
                path: "/tmp/ainb".into(),
                bucket: bucket.clone(),
                repo: None,
            }],
            grand_total: bucket,
            calls: Vec::new(),
            sessions: Vec::new(),
            models: Vec::new(),
            activities: Vec::new(),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            shell_commands: Vec::new(),
            branches: Vec::new(),
            model_project_counts: Vec::new(),
        },
    }
}

#[test]
fn burndown_loader_auto_subscribes_to_sessions_usage_data() {
    let Some((manifest, wasm)) = burndown_artifacts() else {
        eprintln!(
            "skipping: dist/plugins/burndown missing — run \
             `bash scripts/build-plugins.sh` from the workspace root"
        );
        return;
    };

    // Sanity: the manifest itself declares the topic. Catches a future
    // accidental edit that drops the [subscribes] block.
    assert_eq!(
        manifest.subscribes.topics,
        vec!["sessions.usage_data".to_string()],
        "burndown manifest must declare the subscribes topic so the host \
         loader auto-subscribes it"
    );

    let mut host = PluginHost::new();
    let plugin_id = host
        .load_bytes(manifest, &wasm)
        .expect("burndown loads + _init succeeds")
        .0;

    let subs = host
        .shared()
        .subscriptions
        .lock()
        .expect("subs mutex");
    let topic_subs = subs
        .get("sessions.usage_data")
        .expect("auto-subscribe must register the topic in HostShared.subscriptions");
    assert!(
        topic_subs.iter().any(|p| p == &plugin_id),
        "burndown ({plugin_id}) must be among sessions.usage_data subscribers; \
         got: {topic_subs:?}"
    );
}

#[test]
fn burndown_renders_non_empty_after_sessions_usage_data() {
    let Some((manifest, wasm)) = burndown_artifacts() else {
        eprintln!(
            "skipping: dist/plugins/burndown missing — run \
             `bash scripts/build-plugins.sh` from the workspace root"
        );
        return;
    };

    let mut host = PluginHost::new();
    let plugin_id = host
        .load_bytes(manifest, &wasm)
        .expect("burndown loads")
        .0;

    // Pre-event render: state.data is None, so burndown paints the
    // "Waiting for session-reader plugin..." chrome. We don't gate on
    // *what* it paints here — only that the post-event render shows
    // strictly more content (proving the data path landed).
    host.render_plugin(&plugin_id).expect("pre-event render ok");
    let pre_buf = host
        .take_render(&plugin_id, RenderTarget::Screen)
        .expect("pre-event render must surface a buffer");
    let pre_visible_chars: usize = pre_buf
        .cells
        .iter()
        .filter(|c| !c.symbol.trim().is_empty())
        .count();

    // Publish a fixture UsageDataEvent on sessions.usage_data. Use a
    // distinct publisher id so the bus's "don't deliver to self" rule
    // doesn't drop it. Encoded with rmp-serde — same shape session-
    // reader uses.
    let event_bytes = rmp_serde::to_vec_named(&fixture_event())
        .expect("fixture UsageDataEvent encodes");
    host.shared()
        .publish("test-publisher", "sessions.usage_data".into(), event_bytes);

    host.pump_events()
        .expect("pump_events must dispatch sessions.usage_data");

    // Post-event render. burndown's _handle_event populated state.data
    // with the converted UsageData; ui::render now paints the daily +
    // project tables instead of the empty placeholder. We assert the
    // glyph count strictly grew, which is robust against minor copy
    // changes inside burndown's render code.
    host.render_plugin(&plugin_id).expect("post-event render ok");
    let post_buf = host
        .take_render(&plugin_id, RenderTarget::Screen)
        .expect("post-event render must surface a buffer");
    let post_visible_chars: usize = post_buf
        .cells
        .iter()
        .filter(|c| !c.symbol.trim().is_empty())
        .count();

    assert!(
        post_visible_chars > pre_visible_chars,
        "post-event render must paint more glyphs than the pre-event \
         empty state (pre={pre_visible_chars}, post={post_visible_chars})"
    );
    assert!(
        !host.get(&plugin_id).unwrap().is_degraded(),
        "burndown must stay healthy through the publish/pump cycle"
    );
}
