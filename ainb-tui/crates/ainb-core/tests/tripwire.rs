//! Phase 5 tripwire — proves ainb really runs as a plugin host.
//!
//! Four gates (Phase 5 + Phase 6f extension):
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
//!     --format=json` through the plugin's CLI dispatch with INJECTED
//!     UsageData; assert byte-equality to `usage_cli_json_report.snap`.
//!  4. `analytics_via_session_reader_matches_baseline` (Phase 6f) —
//!     spawn the release `ainb` subprocess against a deterministic
//!     fixture HOME, with BOTH `session-reader` and `burndown` loaded.
//!     The pipeline is real end-to-end: session-reader's
//!     `read_claude_logs` cap reads JSONL fixtures → publishes
//!     `sessions.usage_data` → burndown subscribes → CLI dispatch
//!     produces JSON. Byte-equal to `usage_cli_report_fixture.snap`.
//!     Closes the gap between gate #3 (which injects fake data) and
//!     the actual production data plane.
//!
//! Soft-skips when `dist/plugins/burndown/plugin.wasm` hasn't been built —
//! lets non-WASI dev loops keep running `cargo test`.
//!
//! Designed to finish in <30s on a warm cache; the plan's CI gate runs
//! `cargo test --test tripwire` as a required check on PRs touching
//! `ainb-tui/**`.

#![cfg(feature = "test-support")]
#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use ainb::test_support::{cli_usage_report_json, sample_usage_data};
use ainb_plugin_api::{PluginEvent, RenderTarget};
use tempfile::TempDir;

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
    let payload = serde_json::to_vec(&data).expect("UsageData -> json bytes");
    let ev = PluginEvent::Custom {
        topic: "burndown.usage_data".into(),
        payload,
    };
    host.dispatch_event_bytes("burndown", &rmp_serde::to_vec_named(&ev).unwrap())
        .expect("plugin handles usage_data");

    let tab_ev = PluginEvent::Custom {
        topic: "burndown.set_tab".into(),
        payload: serde_json::to_vec(&serde_json::json!({ "tab": tab.name() })).expect("tab json bytes"),
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

/// Recursive copy used by the Phase 6f gate to materialise the
/// committed fixture sessions tree into a tempdir HOME.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_child = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_child)?;
        } else {
            fs::copy(entry.path(), &dst_child)?;
        }
    }
    Ok(())
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
    let payload = serde_json::to_vec(&data).expect("UsageData -> json bytes");
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

/// Phase 6f gate: full real two-plugin pipeline byte-identical to baseline.
///
/// Spawns the cargo-built `ainb` subprocess against a deterministic
/// fixture HOME (`tests/fixtures/sessions/claude_projects/...`
/// materialised under `<tempdir>/.claude/projects/...`). With both
/// `session-reader` and `burndown` loaded, the pipeline is real:
///
///   session-reader's `read_claude_logs` cap reads JSONL fixtures
///   → publishes msgpack `sessions.usage_data`
///   → burndown subscribes (declarative `[subscribes]` table)
///   → host dispatches `usage report --format=json`
///   → plugin formats UsageData and emits via `fd_write`
///   → host captures stdout and forwards to subprocess stdout.
///
/// Stdout is byte-equal to `usage_cli_report_fixture.snap`.
///
/// Closes the gap between gate #3 (which injects fake data via
/// `Custom { topic: "burndown.usage_data" }`) and the actual
/// production data plane. A regression in the fs cap, the
/// session-reader publish loop, the topic auto-subscribe wiring,
/// the request_data correlation pump, or the CLI dispatch fd_write
/// capture — any of those — surfaces here as a byte-diff.
///
/// Soft-skips when either plugin .wasm is missing (matches the rest
/// of this file's "non-WASI dev loops keep running cargo test"
/// posture). CI must run `bash scripts/build-plugins.sh` first.
#[test]
fn analytics_via_session_reader_matches_baseline() {
    let _env = env_guard();
    let dist = workspace_dist();
    let burndown_wasm = dist.join("burndown").join("plugin.wasm");
    let session_reader_wasm = dist.join("session-reader").join("plugin.wasm");
    if !burndown_wasm.exists() || !session_reader_wasm.exists() {
        eprintln!(
            "skipping analytics_via_session_reader_matches_baseline: \
             dist/plugins/{{burndown,session-reader}}/plugin.wasm missing — \
             run scripts/build-plugins.sh"
        );
        return;
    }

    // Materialise fixture HOME.
    let tmp = TempDir::new().expect("tempdir for fixture HOME");
    let claude_root = tmp.path().join(".claude").join("projects");
    fs::create_dir_all(&claude_root).expect("mkdir -p .claude/projects");
    let src_claude = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sessions")
        .join("claude_projects");
    assert!(
        src_claude.exists(),
        "fixture sessions tree missing at {} — Phase 6e committed it; \
         tripwire 6f cannot run without it",
        src_claude.display()
    );
    copy_dir_recursive(&src_claude, &claude_root)
        .expect("copy fixture claude_projects/ into <HOME>/.claude/projects");

    // Spawn the cargo-built ainb binary. CARGO_BIN_EXE_ainb is
    // injected by cargo for integration tests of bin crates.
    let ainb_path = PathBuf::from(env!("CARGO_BIN_EXE_ainb"));
    assert!(
        ainb_path.exists(),
        "ainb binary not built at {}",
        ainb_path.display()
    );

    let mut cmd = Command::new(&ainb_path);
    cmd.args(["usage", "report", "--format=json"]);
    cmd.env("HOME", tmp.path());
    cmd.env("AINB_HOME", tmp.path());
    cmd.env("AINB_PLUGIN_ROOT", &dist);
    cmd.env_remove("AINB_DISABLE_PLUGINS");
    let out = cmd.output().expect("spawn ainb");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "ainb usage report exit {:?}; stderr={}; stdout-head={:?}",
        out.status.code(),
        stderr,
        stdout.chars().take(200).collect::<String>()
    );

    // Compare to the Phase 6e fixture baseline. Trim one trailing
    // newline both sides per memory `reference_insta_trailing_newline_trap`.
    let baseline_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("baselines")
        .join("usage_cli_report_fixture.snap");
    let baseline = fs::read_to_string(&baseline_path)
        .unwrap_or_else(|e| panic!("read baseline {}: {e}", baseline_path.display()));
    let actual = stdout.trim_end_matches('\n');
    let expected = baseline.trim_end_matches('\n');

    if actual != expected {
        // First-divergent-line diff so a regression is debuggable.
        let mut line_no = 1usize;
        for (a, e) in actual.lines().zip(expected.lines()) {
            if a != e {
                panic!(
                    "full-pipeline stdout != baseline at line {line_no}\n\
                     actual  : {a:?}\n\
                     expected: {e:?}\n\
                     (re-baseline by replacing tests/baselines/usage_cli_report_fixture.snap \
                      if intended)"
                );
            }
            line_no += 1;
        }
        panic!(
            "full-pipeline stdout != baseline: line counts differ \
             (actual={}, expected={})",
            actual.lines().count(),
            expected.lines().count()
        );
    }

    drop(tmp);
}
