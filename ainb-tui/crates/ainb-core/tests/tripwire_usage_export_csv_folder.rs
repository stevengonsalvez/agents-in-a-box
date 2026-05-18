//! Tripwire SC2: `ainb usage export --format csv -o <dir>` writes the
//! codeburn-style per-table CSV folder, refuses to clobber unrelated
//! directories, and `--top N` caps the long By-X tables.
//!
//! Pairs with the SC1 tripwire — the format flag now reaches the plugin
//! and the export subcommand respects `--output <dir>` for the per-table
//! folder fan-out.
//!
//! Skips gracefully when `dist/plugins/{burndown,session-reader}` aren't
//! staged so fresh checkouts don't fail the suite.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Serialize the four `#[test]` cases in this binary. Each subprocess
/// spawn races against burndown/session-reader's eager startup, and
/// four parallel ainb invocations can blow past the registry's 120s
/// deadline when the plugin pool gets queued. The mutex caps inflight
/// at 1 per test binary; cargo still runs separate binaries in
/// parallel.
static SERIAL: Mutex<()> = Mutex::new(());

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

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
    let proj_dir = home.join(".claude").join("projects").join("-tripwire-fixture-project");
    fs::create_dir_all(&proj_dir).unwrap();
    let session_jsonl = r#"{"type":"assistant","timestamp":"2026-05-10T12:00:00.000Z","sessionId":"fixture-session-1","cwd":"/tmp/x","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
    fs::write(proj_dir.join("fixture-session-1.jsonl"), session_jsonl).unwrap();
}

fn run_export_csv(plugin_root: &Path, home: &Path, output: &Path) -> std::process::Output {
    Command::new(ainb_bin())
        .env("HOME", home)
        .env("AINB_PLUGIN_ROOT", plugin_root)
        .args([
            "usage", "export", "--format", "csv", "--period", "all", "-o",
        ])
        .arg(output)
        .output()
        .expect("ainb spawn")
}

#[test]
fn usage_export_csv_writes_per_table_folder() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    // Use an explicit child path that *doesn't* exist yet so the plugin
    // creates it. We avoid mktemp -d because temp paths look like file
    // names (`tmp.XXXX`) to the extension heuristic.
    let export_dir = home.path().join("export-out");
    let out = run_export_csv(&plugin_root, home.path(), &export_dir);
    assert!(
        out.status.success(),
        "export failed: exit={:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // Required per-table files (always present).
    for required in [
        "summary.csv",
        "daily.csv",
        "activity.csv",
        "models.csv",
        "projects.csv",
        "sessions.csv",
        "README.txt",
        ".ainb-export",
    ] {
        let p = export_dir.join(required);
        assert!(
            p.exists(),
            "expected {required} in export folder; ls of dir:\n{:?}",
            fs::read_dir(&export_dir)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>()
        );
    }

    // summary.csv must have the inline-section CSV header.
    let summary = fs::read_to_string(export_dir.join("summary.csv")).unwrap();
    assert!(
        summary.contains("section,metric,value"),
        "summary.csv missing inline-section header:\n{summary}"
    );

    // README.txt must list every emitted file.
    let readme = fs::read_to_string(export_dir.join("README.txt")).unwrap();
    for fragment in [
        "summary.csv",
        "daily.csv",
        "activity.csv",
        "models.csv",
        "projects.csv",
        "sessions.csv",
    ] {
        assert!(
            readme.contains(fragment),
            "README.txt missing {fragment}:\n{readme}"
        );
    }
}

#[test]
fn usage_export_csv_refuses_to_clobber_unrelated_dir() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    // Pre-populate a non-empty dir without the .ainb-export marker.
    let danger = home.path().join("important-stuff");
    fs::create_dir_all(&danger).unwrap();
    fs::write(danger.join("notes.txt"), "user data — do not delete\n").unwrap();

    let out = run_export_csv(&plugin_root, home.path(), &danger);
    assert!(
        !out.status.success(),
        "export should have refused to clobber {} but succeeded with exit 0",
        danger.display()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to overwrite non-empty directory"),
        "expected refusal message in stderr; got:\n{stderr}"
    );

    // The pre-existing file must still be intact.
    let notes = fs::read_to_string(danger.join("notes.txt")).unwrap();
    assert_eq!(notes, "user data — do not delete\n");
}

#[test]
fn usage_export_csv_inline_stream_unchanged_without_output() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    // No -o means the legacy stdout-only inline-sectioned CSV stream.
    let out = Command::new(ainb_bin())
        .env("HOME", home.path())
        .env("AINB_PLUGIN_ROOT", &plugin_root)
        .args(["usage", "export", "--format", "csv", "--period", "all"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("section,metric,value"),
        "inline CSV missing header:\n{stdout}"
    );
}

#[test]
fn usage_report_top_caps_by_x_sections() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    // --top 3 with one synthetic session produces By-Project / By-Activity /
    // By-Model sections with up to 3 rows each. Real check: count of "- "
    // lines is at most 3 per section. We assert the upper bound holds.
    let out = Command::new(ainb_bin())
        .env("HOME", home.path())
        .env("AINB_PLUGIN_ROOT", &plugin_root)
        .args([
            "usage", "report", "--format", "text", "--period", "all", "--top", "3",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Walk each section and confirm no more than 3 dash-bullet rows after it.
    for section in ["By Project", "By Activity", "By Model"] {
        let after = stdout.split_once(section).map(|(_, rest)| rest).unwrap_or("");
        let rows = after.lines().skip(1).take_while(|l| l.starts_with("- ")).count();
        assert!(
            rows <= 3,
            "{section} emitted {rows} rows under --top 3 (max allowed: 3)\nFull output:\n{stdout}"
        );
    }
}
