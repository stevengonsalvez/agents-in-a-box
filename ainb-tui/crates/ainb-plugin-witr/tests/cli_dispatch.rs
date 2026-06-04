//! Integration tests for the `ainb witr` CLI surface
//! ([`WitrPlugin::cli_dispatch`]).
//!
//! Drives the full dispatch path against a stub `witr` binary staged
//! in a tempdir + injected as the plugin's resolved path. Covers the
//! cfx.7 acceptance bars:
//!   - `ainb witr 1` returns valid text output
//!   - `ainb witr --format json 1` round-trips JSON
//!   - missing-witr → exit 1
//!   - `--tree` / `--warnings` / `--short` variants
//!
//! Gated on cfg(unix): stubs are `#!/bin/sh` scripts marked
//! executable. Witr v1 support matrix is mac arm64 + linux x86_64.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ainb_plugin_witr::plugin::WitrPlugin;
use ainb_plugin_witr::state::Lifecycle;

const SAMPLE_JSON: &str = r#"{"Target":{"Type":"name","Value":"nginx"},"ResolvedTarget":"nginx","Process":{"PID":1234,"PPID":800,"Command":"nginx","User":"root"},"RestartCount":0,"Ancestry":[{"PID":800,"PPID":1,"Command":"systemd"}],"Source":{"Type":"systemd","Name":"nginx.service"},"Warnings":["running as root"]}"#;

fn stage_json_stub(dir: &Path) -> PathBuf {
    let json_file = dir.join("snapshot.json");
    fs::write(&json_file, SAMPLE_JSON).expect("write json fixture");
    let bin = dir.join("witr");
    // `--json -- <target>` prints the canned snapshot; bare
    // `-- <target>` (the --short passthrough) prints a one-line
    // human summary.
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--json\" ]; then\n  cat \"{json}\"\n  exit 0\nfi\nprintf 'witr: nginx (pid 1234) <- systemd\\n'\nexit 0\n",
        json = json_file.display(),
    );
    fs::write(&bin, script).expect("write stub");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).expect("stat").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod");
    bin
}

/// Build a Ready plugin whose witr_path points at a freshly-staged
/// stub. Returns (plugin, tempdir) — keep the tempdir alive for the
/// test's duration.
fn ready_plugin() -> (WitrPlugin, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = stage_json_stub(dir.path());
    let mut p = WitrPlugin::default();
    p.set_ready_for_test(stub);
    (p, dir)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[tokio::test]
async fn cli_witr_target_returns_text_output() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("witr", &argv(&["nginx"])).await;
    assert_eq!(out.exit_code, 0);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("nginx"), "text output: {s}");
    assert!(s.contains("1234"));
    assert!(s.contains("systemd"));
}

#[tokio::test]
async fn cli_witr_format_json_round_trips() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("witr", &argv(&["nginx", "--format", "json"])).await;
    assert_eq!(out.exit_code, 0);
    let s = String::from_utf8_lossy(&out.stdout);
    // Round-trips back into the model.
    let parsed: ainb_plugin_witr::model::WitrSnapshot =
        serde_json::from_str(&s).expect("json output must re-parse");
    assert_eq!(parsed.process.pid, 1234);
    assert_eq!(parsed.target.value, "nginx");
}

#[tokio::test]
async fn cli_witr_tree_shows_ancestry() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("witr", &argv(&["nginx", "--tree"])).await;
    assert_eq!(out.exit_code, 0);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("systemd"));
    assert!(s.contains("◀ target"));
}

#[tokio::test]
async fn cli_witr_warnings_only() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("witr", &argv(&["nginx", "--warnings"])).await;
    assert_eq!(out.exit_code, 0);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "running as root"
    );
}

#[tokio::test]
async fn cli_witr_short_passthrough() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("witr", &argv(&["nginx", "--short"])).await;
    assert_eq!(out.exit_code, 0);
    // Stub's non-json branch prints the one-line human summary.
    assert!(String::from_utf8_lossy(&out.stdout).contains("<- systemd"));
}

#[tokio::test]
async fn cli_witr_missing_binary_exits_one() {
    // Default plugin: lifecycle Unknown, witr_path None.
    let mut p = WitrPlugin::default();
    assert_eq!(p.lifecycle_for_test(), &Lifecycle::Unknown);

    let out = p.cli_dispatch_core("witr", &argv(&["nginx"])).await;
    assert_eq!(out.exit_code, 1);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("not found") || s.contains("Install"),
        "missing hint: {s}"
    );
}

#[tokio::test]
async fn cli_witr_unknown_namespace_exits_two() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("bogus", &argv(&["nginx"])).await;
    assert_eq!(out.exit_code, 2);
}

#[tokio::test]
async fn cli_witr_missing_target_usage_error() {
    let (mut p, _dir) = ready_plugin();

    let out = p.cli_dispatch_core("witr", &argv(&[])).await;
    assert_eq!(out.exit_code, 2);
    assert!(!out.stderr.is_empty(), "usage error on stderr");
}

// NOTE: these exercise the slash PARSER + state mutation only. The
// host has no slash-dispatch transport yet (stubbed; tracked in
// agents-in-a-box-6qc), so `handle_slash` is not reachable end-to-end
// from a user typing `/witr` in the input box. They prove the
// plugin-side foundation, not the full user-visible flow.

#[tokio::test]
async fn handle_slash_parses_target_and_opens_detail_state() {
    let (mut p, _dir) = ready_plugin();
    p.handle_slash("/witr nginx").await.expect("slash parses");
    assert_eq!(p.current_target_for_test(), "nginx");
    assert!(
        p.is_detail_open_for_test(),
        "handle_slash sets DetailOpen state"
    );
}

#[tokio::test]
async fn handle_slash_invalid_line_returns_error() {
    let (mut p, _dir) = ready_plugin();
    assert!(p.handle_slash("/witr").await.is_err());
}
