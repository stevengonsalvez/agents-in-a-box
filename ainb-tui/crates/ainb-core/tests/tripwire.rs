//! Phase 5 tripwire — proves ainb really runs as a plugin host.
//!
//! Three gates per the plan (`plans/plugin-mvp-phases-0-5.md` §Phase 5):
//!
//!  1. `analytics_disappears_without_plugins` — `AINB_DISABLE_PLUGINS=1` →
//!     the host comes up without the burndown plugin loaded; the
//!     PluginScreen renders its placeholder fallback so the TUI doesn't
//!     panic.
//!  2. `analytics_byte_identical_via_plugin` — load burndown.wasm, push
//!     `sample_usage_data()` via Custom event, render every Analytics tab,
//!     compare cell-for-cell against the agent-3 baselines under
//!     `tests/baselines/`.
//!  3. `cli_usage_via_plugin_matches_baseline` — drive `ainb usage report
//!     --format=json` through the plugin's CLI dispatch; assert the
//!     output matches the `usage_cli_json_report.snap` baseline byte-for-byte.
//!
//! Soft-skips when `dist/plugins/burndown/plugin.wasm` hasn't been built —
//! lets non-WASI dev loops keep running `cargo test`.
//!
//! Designed to finish in <30s on a warm cache; the plan's CI gate runs
//! `cargo test --test tripwire` as a required check on PRs touching
//! `ainb-tui/**`.

#![cfg(feature = "test-support")]
#![allow(missing_docs)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use ainb::test_support::{cli_usage_report_json, sample_usage_data};
use ainb_plugin_api::{PluginEvent, RenderTarget};

/// Serialise env mutation across the three tripwire tests. Cargo runs
/// tests inside a binary on parallel threads; `std::env` is process-global,
/// so back-to-back `set_var` from different tests would race and the
/// `AINB_DISABLE_PLUGINS=1` test could trample its peers.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> MutexGuard<'static, ()> {
    // Poisoned guards still serialise — recover and continue.
    ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn workspace_dist() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("dist").join("plugins"))
        .expect("workspace root resolvable")
}

fn plugin_present() -> Option<PathBuf> {
    let dist = workspace_dist();
    dist.join("burndown")
        .join("plugin.wasm")
        .exists()
        .then_some(dist)
}

/// Tab discriminant local to this file — the plugin owns the real type
/// internally; tripwire only needs the string the Custom event payload
/// expects.
#[derive(Clone, Copy)]
enum Tab {
    Daily,
    Weekly,
    Projects,
    Burndown,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Projects => "projects",
            Self::Burndown => "burndown",
        }
    }

    fn baseline_filename(self) -> &'static str {
        match self {
            Self::Daily => "analytics_daily.snap",
            Self::Weekly => "analytics_weekly.snap",
            Self::Projects => "analytics_projects.snap",
            Self::Burndown => "analytics_burndown.snap",
        }
    }
}

fn read_baseline(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("baselines")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read baseline {}: {e}", path.display()))
}

/// `insta` baselines carry a YAML frontmatter (`---\n…\n---\n`) followed
/// by the dump. Strip it so byte comparisons hit the painted cell grid.
fn strip_insta_frontmatter(s: &str) -> &str {
    let s = s.strip_prefix("---\n").unwrap_or(s);
    if let Some(idx) = s.find("\n---\n") {
        &s[idx + "\n---\n".len()..]
    } else {
        s
    }
}

fn render_tab(host: &mut ainb_plugin_host::PluginHost, tab: Tab) -> String {
    // Push UsageData (idempotent — host re-sends it for each tab so a
    // failure on one tab can't poison the next).
    let data = sample_usage_data();
    let payload = serde_json::to_value(&data).expect("UsageData -> json");
    let ev = PluginEvent::Custom {
        topic: "burndown.usage_data".into(),
        payload,
    };
    host.dispatch_event_bytes("burndown", &rmp_serde::to_vec_named(&ev).unwrap())
        .expect("plugin handles usage_data");

    let tab_ev = PluginEvent::Custom {
        topic: "burndown.set_tab".into(),
        payload: serde_json::json!({ "tab": tab.name() }),
    };
    host.dispatch_event_bytes("burndown", &rmp_serde::to_vec_named(&tab_ev).unwrap())
        .expect("plugin handles set_tab");

    host.render_plugin("burndown").expect("_render runs");
    let buf = host
        .take_render("burndown", RenderTarget::Screen)
        .expect("plugin painted");

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
    out
}

#[test]
fn analytics_disappears_without_plugins() {
    let _env = env_guard();
    // Pin AINB_PLUGIN_ROOT to a real existing dir so the disable-flag is
    // the ONLY reason the host comes up empty (otherwise the test could
    // pass for the wrong reason — e.g. the plugin simply not being on
    // disk on this machine).
    let dist = workspace_dist();
    if !dist.exists() {
        eprintln!(
            "skipping: dist/plugins missing — run scripts/build-plugins.sh \
             so the test can prove the disable flag (not the absence) is \
             what kept plugins out of the host"
        );
        return;
    }
    std::env::set_var("AINB_PLUGIN_ROOT", &dist);
    std::env::set_var("AINB_DISABLE_PLUGINS", "1");

    let (host, outcome) = ainb::plugins::init_plugin_host();
    assert!(
        outcome.loaded.is_empty(),
        "AINB_DISABLE_PLUGINS=1 must skip plugin discovery; got loaded={:?}",
        outcome.loaded
    );
    assert!(
        outcome.failed.is_empty(),
        "no plugin should attempt to load; got failed={:?}",
        outcome.failed
    );
    assert!(
        host.plugins().next().is_none(),
        "host must come up with zero plugins"
    );

    std::env::remove_var("AINB_DISABLE_PLUGINS");
    std::env::remove_var("AINB_PLUGIN_ROOT");
}

#[test]
fn analytics_byte_identical_via_plugin() {
    let _env = env_guard();
    let Some(dist) = plugin_present() else {
        eprintln!(
            "skipping: dist/plugins/burndown/plugin.wasm missing — \
             run scripts/build-plugins.sh"
        );
        return;
    };
    std::env::set_var("AINB_PLUGIN_ROOT", &dist);

    let (mut host, outcome) = ainb::plugins::init_plugin_host();
    assert!(outcome.failed.is_empty(), "plugin must load: {:?}", outcome.failed);

    for tab in [Tab::Daily, Tab::Weekly, Tab::Projects, Tab::Burndown] {
        let dump = render_tab(&mut host, tab);
        let baseline_raw = read_baseline(tab.baseline_filename());
        let baseline = strip_insta_frontmatter(&baseline_raw);
        // Insta strips one trailing newline when persisting; our dump ends
        // with a `\n` after the last row.
        let normalized_dump = dump.trim_end_matches('\n');
        let normalized_baseline = baseline.trim_end_matches('\n');
        assert_eq!(
            normalized_dump,
            normalized_baseline,
            "tab {} drifted from baseline {}",
            tab.name(),
            tab.baseline_filename()
        );
    }

    std::env::remove_var("AINB_PLUGIN_ROOT");
}

#[test]
fn cli_usage_via_plugin_matches_baseline() {
    let _env = env_guard();
    let Some(dist) = plugin_present() else {
        eprintln!(
            "skipping: dist/plugins/burndown/plugin.wasm missing — \
             run scripts/build-plugins.sh"
        );
        return;
    };
    std::env::set_var("AINB_PLUGIN_ROOT", &dist);

    let (mut host, outcome) = ainb::plugins::init_plugin_host();
    assert!(outcome.failed.is_empty(), "plugin must load: {:?}", outcome.failed);

    let data = sample_usage_data();
    let payload = serde_json::to_value(&data).expect("UsageData -> json");
    let ev = PluginEvent::Custom {
        topic: "burndown.usage_data".into(),
        payload,
    };
    host.dispatch_event_bytes("burndown", &rmp_serde::to_vec_named(&ev).unwrap())
        .unwrap();

    let argv = vec!["report".to_string(), "--format=json".to_string()];
    let (stdout, stderr) = host.dispatch_cli("burndown", "usage", &argv).unwrap();
    assert!(
        stderr.is_empty(),
        "stderr should be empty: {}",
        String::from_utf8_lossy(&stderr)
    );

    let plugin_out =
        String::from_utf8(stdout).expect("plugin stdout is utf8");

    // First identity: plugin output equals the in-tree report_json.
    // Same gate as plugin_burndown_cli_dispatch.rs but inlined here so the
    // tripwire stands alone in CI.
    let expected_value = cli_usage_report_json(&data);
    let in_tree = serde_json::to_string_pretty(&expected_value).unwrap() + "\n";
    assert_eq!(
        plugin_out, in_tree,
        "plugin CLI report JSON must match in-tree report_json"
    );

    // Second identity: the same output also matches the snapshot baseline
    // captured at Phase 5-prep, byte-for-byte (modulo the insta YAML frontmatter).
    // Defends against any silent in-tree path drift that would slip past
    // the first identity.
    let baseline_raw = read_baseline("usage_cli_json_report.snap");
    let baseline = strip_insta_frontmatter(&baseline_raw);
    // The baseline was captured by serializing the JSON Value directly —
    // strip the trailing newline that the plugin path adds before
    // comparing payload-to-payload.
    let plugin_payload = plugin_out.trim_end_matches('\n');
    let baseline_payload = baseline.trim_end_matches('\n');
    assert_eq!(
        plugin_payload, baseline_payload,
        "plugin CLI report JSON drifted from baseline"
    );

    std::env::remove_var("AINB_PLUGIN_ROOT");
}
