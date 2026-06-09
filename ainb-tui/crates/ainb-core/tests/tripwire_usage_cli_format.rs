//! Tripwire SC1: `ainb usage report --format X` produces the requested
//! output shape in BOTH argv positions — pre-subcommand (global) and
//! post-subcommand (positional).
//!
//! Catches the bug class where clap-derive consumes the host's global
//! `--format` arg before the residual argv reaches the burndown plugin,
//! so `ainb --format json usage report` would silently render text.
//! Pairs with the argv injection in
//! `crates/ainb-core/src/cli/registry.rs::dispatch_inner`.
//!
//! Four formats × two positions = eight invocations. Each invocation
//! asserts on a single distinctive byte sequence that proves the
//! requested format reached the renderer:
//!
//! | Format    | Distinctive marker             |
//! |-----------|--------------------------------|
//! | text      | "Usage Report" + no JSON brace |
//! | json      | leading `{` + `"activities"`   |
//! | csv       | `section,metric,value` header  |
//! | markdown  | `# Usage Report` h1            |
//!
//! Skips gracefully when `dist/plugins/{burndown,session-reader}` aren't
//! staged so fresh checkouts don't fail the suite. Requires real
//! `~/.claude/projects` data to exist (or seeded synthetic data) — the
//! plugin emits format-correct output even with zero data, just without
//! populated table rows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Serialize the four `#[test]` cases — parallel ainb subprocess
/// spawns race against burndown/session-reader's eager startup and
/// have hit the registry's 120s deadline in CI under load. The mutex
/// caps inflight at 1 per test binary; cargo still runs separate
/// binaries in parallel.
static SERIAL: Mutex<()> = Mutex::new(());

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Walk up from the ainb binary looking for the `dist/plugins/` staging
/// dir. Mirrors `tripwire_real_data_in_tui::plugins_staged` so the test
/// skips cleanly on fresh checkouts.
fn plugins_staged() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("burndown").join("burndown").exists()
            && candidate.join("session-reader").join("session-reader").exists()
        {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).unwrap();
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{}"
skipped_dependencies = []
git_directories = []
"#,
        env!("CARGO_PKG_VERSION")
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).unwrap();

    // Seed a synthetic claude session so session-reader publishes
    // non-empty usage data. Empty roots are also valid (the plugin
    // still emits a format-correct skeleton) but populated data
    // exercises the same renderer path the user runs daily.
    let proj_dir = home.join(".claude").join("projects").join("-tripwire-fixture-project");
    fs::create_dir_all(&proj_dir).unwrap();
    let session_jsonl = r#"{"type":"assistant","timestamp":"2026-05-10T12:00:00.000Z","sessionId":"fixture-session-1","cwd":"/tmp/x","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
    fs::write(proj_dir.join("fixture-session-1.jsonl"), session_jsonl).unwrap();
}

/// Invoke `ainb usage report --period today` with the requested format
/// in either the pre- or post-subcommand position. Returns stdout as
/// a String. Panics on non-zero exit so the caller can write tight
/// positive assertions on the body.
fn run_usage_report(plugin_root: &Path, home: &Path, format: &str, pre_position: bool) -> String {
    let ainb = ainb_bin();
    let mut cmd = Command::new(&ainb);
    cmd.env("HOME", home).env("AINB_PLUGIN_ROOT", plugin_root);
    if pre_position {
        cmd.args(["--format", format, "usage", "report", "--period", "today"]);
    } else {
        cmd.args(["usage", "report", "--period", "today", "--format", format]);
    }
    let out = cmd.output().expect("ainb spawn");
    assert!(
        out.status.success(),
        "ainb usage report --format {format} ({}position) failed: exit={:?}\nstderr:\n{}",
        if pre_position { "pre-" } else { "post-" },
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn usage_report_format_text_both_positions() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    for pre in [true, false] {
        let out = run_usage_report(&plugin_root, home.path(), "text", pre);
        assert!(
            out.contains("Usage Report"),
            "text format ({}): missing 'Usage Report' header:\n{out}",
            if pre { "pre" } else { "post" }
        );
        assert!(
            !out.trim_start().starts_with('{'),
            "text format ({}): leaked JSON brace:\n{out}",
            if pre { "pre" } else { "post" }
        );
        assert!(
            !out.contains("section,metric,value"),
            "text format ({}): leaked CSV header:\n{out}",
            if pre { "pre" } else { "post" }
        );
        assert!(
            !out.contains("# Usage Report"),
            "text format ({}): leaked markdown h1:\n{out}",
            if pre { "pre" } else { "post" }
        );
    }
}

#[test]
fn usage_report_format_json_both_positions() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    for pre in [true, false] {
        let out = run_usage_report(&plugin_root, home.path(), "json", pre);
        let trimmed = out.trim_start();
        assert!(
            trimmed.starts_with('{'),
            "json format ({}): output doesn't open with '{{':\n{out}",
            if pre { "pre" } else { "post" }
        );
        // Sanity check that the body really is the JSON report (the
        // report_json helper emits an `"activities"` array unconditionally).
        assert!(
            out.contains("\"activities\""),
            "json format ({}): missing 'activities' key:\n{out}",
            if pre { "pre" } else { "post" }
        );
    }
}

#[test]
fn usage_report_format_csv_both_positions() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    for pre in [true, false] {
        let out = run_usage_report(&plugin_root, home.path(), "csv", pre);
        assert!(
            out.starts_with("section,metric,value"),
            "csv format ({}): missing 'section,metric,value' header:\n{out}",
            if pre { "pre" } else { "post" }
        );
    }
}

#[test]
fn usage_report_format_markdown_both_positions() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    for pre in [true, false] {
        let out = run_usage_report(&plugin_root, home.path(), "markdown", pre);
        assert!(
            out.starts_with("# Usage Report"),
            "markdown format ({}): missing '# Usage Report' h1:\n{out}",
            if pre { "pre" } else { "post" }
        );
        assert!(
            out.contains("## Overview"),
            "markdown format ({}): missing '## Overview' h2:\n{out}",
            if pre { "pre" } else { "post" }
        );
        assert!(
            !out.trim_start().starts_with('{'),
            "markdown format ({}): leaked JSON brace:\n{out}",
            if pre { "pre" } else { "post" }
        );
    }
}
