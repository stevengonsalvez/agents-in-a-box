//! Tripwire SC3: `ainb usage models --by-task` emits the per-model ×
//! per-activity-category matrix, and the `--provider` arg accepts every
//! new variant (cursor / copilot / gemini).
//!
//! Verifies the SC3 surface contract — real Cursor / Copilot / Gemini
//! call parsing isn't required for the tripwire to pass; the parsers
//! are scaffolds and return empty Vecs until on-disk formats stabilise.
//! What the tripwire DOES verify:
//!
//! - `usage models` (no flag) renders a flat per-model rollup in every
//!   format.
//! - `usage models --by-task` renders the matrix in every format
//!   with the expected schema header / table structure.
//! - `--provider cursor`, `--provider copilot`, `--provider gemini`
//!   are all accepted by clap (no "invalid value" parse error).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Serialize the three `#[test]` cases — parallel ainb subprocess
/// spawns race against burndown/session-reader's eager startup and
/// have hit the registry's 120s deadline in CI under load. The mutex
/// caps inflight at 1 per test binary; cargo still runs separate
/// binaries in parallel.
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
    // Two assistant turns so the matrix has at least one row of real
    // data to bucket into a category (Conversation since user_message
    // is empty + no tools used).
    let session_jsonl = r#"{"type":"assistant","timestamp":"2026-05-10T12:00:00.000Z","sessionId":"fixture-session-1","cwd":"/tmp/x","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
{"type":"assistant","timestamp":"2026-05-10T12:01:00.000Z","sessionId":"fixture-session-1","cwd":"/tmp/x","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":2000,"output_tokens":1000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
    fs::write(proj_dir.join("fixture-session-1.jsonl"), session_jsonl).unwrap();
}

fn run_models(plugin_root: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ainb_bin())
        .env("HOME", home)
        .env("AINB_PLUGIN_ROOT", plugin_root)
        .args(["usage", "models", "--period", "all"])
        .args(args)
        .output()
        .expect("ainb spawn")
}

#[test]
fn usage_models_flat_rollup_renders_in_each_format() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    let json = run_models(&plugin_root, home.path(), &["--format", "json"]);
    assert!(
        json.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let json_out = String::from_utf8_lossy(&json.stdout);
    assert!(
        json_out.contains("\"models\""),
        "json missing models key:\n{json_out}"
    );

    let csv = run_models(&plugin_root, home.path(), &["--format", "csv"]);
    assert!(csv.status.success());
    let csv_out = String::from_utf8_lossy(&csv.stdout);
    assert!(
        csv_out.starts_with("model,calls,tokens,cost_usd"),
        "csv flat header wrong:\n{csv_out}"
    );

    let md = run_models(&plugin_root, home.path(), &["--format", "markdown"]);
    assert!(md.status.success());
    let md_out = String::from_utf8_lossy(&md.stdout);
    assert!(
        md_out.starts_with("# Models"),
        "markdown flat heading wrong:\n{md_out}"
    );

    let text = run_models(&plugin_root, home.path(), &["--format", "text"]);
    assert!(text.status.success());
    let text_out = String::from_utf8_lossy(&text.stdout);
    assert!(
        text_out.contains("By Model"),
        "text flat heading wrong:\n{text_out}"
    );
}

#[test]
fn usage_models_by_task_matrix_renders_in_each_format() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    let json = run_models(
        &plugin_root,
        home.path(),
        &["--by-task", "--format", "json"],
    );
    assert!(
        json.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let out = String::from_utf8_lossy(&json.stdout);
    assert!(
        out.contains("\"schema\": \"ainb.usage.models_by_task.v1\""),
        "missing schema tag:\n{out}"
    );
    assert!(out.contains("\"columns\""), "missing columns array:\n{out}");
    assert!(out.contains("Coding"), "missing Coding column:\n{out}");
    assert!(
        out.contains("Conversation"),
        "missing Conversation column:\n{out}"
    );
    assert!(
        out.contains("claude-sonnet-4-5"),
        "missing seeded model row:\n{out}"
    );

    let csv = run_models(&plugin_root, home.path(), &["--by-task", "--format", "csv"]);
    assert!(csv.status.success());
    let csv_out = String::from_utf8_lossy(&csv.stdout);
    assert!(
        csv_out.starts_with("model,Coding_calls,Coding_tokens,Coding_cost_usd"),
        "csv matrix header wrong:\n{csv_out}"
    );
    assert!(
        csv_out.contains("claude-sonnet-4-5"),
        "missing model row in csv:\n{csv_out}"
    );

    let md = run_models(
        &plugin_root,
        home.path(),
        &["--by-task", "--format", "markdown"],
    );
    assert!(md.status.success());
    let md_out = String::from_utf8_lossy(&md.stdout);
    assert!(
        md_out.starts_with("# Models by Task"),
        "markdown matrix heading wrong:\n{md_out}"
    );

    let text = run_models(
        &plugin_root,
        home.path(),
        &["--by-task", "--format", "text"],
    );
    assert!(text.status.success());
    let text_out = String::from_utf8_lossy(&text.stdout);
    assert!(
        text_out.starts_with("Models by Task"),
        "text matrix heading wrong:\n{text_out}"
    );
}

/// All three new provider variants accept on the CLI surface — the
/// scaffolds return empty rollups, but clap must not reject the flag.
#[test]
fn usage_report_accepts_cursor_copilot_gemini_providers() {
    let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/{{burndown,session-reader}} not staged");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    for provider in ["cursor", "copilot", "gemini"] {
        let out = Command::new(ainb_bin())
            .env("HOME", home.path())
            .env("AINB_PLUGIN_ROOT", &plugin_root)
            .args([
                "usage",
                "report",
                "--provider",
                provider,
                "--format",
                "text",
                "--period",
                "today",
            ])
            .output()
            .expect("ainb spawn");
        assert!(
            out.status.success(),
            "--provider {provider} rejected (exit {:?}):\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Scaffolds emit zero rows, so the text body should still
        // contain the section headers — confirms the report path
        // ran instead of early-failing.
        assert!(
            stdout.contains("Usage Report") || stdout.contains("Today"),
            "--provider {provider} report missing chrome:\n{stdout}"
        );
    }
}
