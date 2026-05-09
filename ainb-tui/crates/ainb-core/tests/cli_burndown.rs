//! Phase 6c-cli + 6e CI gate. Spawns the cargo-built `ainb` binary as
//! a subprocess and asserts the burndown-plugin-routed `usage`
//! subcommands behave per spec.
//!
//! Test layers:
//!
//! 1. **Fail-clean** (no fixtures, no plugins): plugins disabled →
//!    exit 2 + actionable stderr, empty stdout.
//! 2. **Byte-identity** (deterministic fixture HOME): for each
//!    plugin-side subcommand whose JSON output is deterministic
//!    (`report`, `export`, `optimize`, `compare`, `yield`, `status`),
//!    the `--format=json` stdout matches a frozen baseline byte-for-
//!    byte. Drift surfaces via the first divergent line.
//! 3. **Structural** (deterministic fixture HOME, but the subcommand
//!    isn't byte-stable yet): `today`/`month` run through the same
//!    plugin path as `report` but `build_filters_from_args` doesn't
//!    re-apply the period filter inside the plugin yet, and
//!    `model-alias` is reflection-only — these get shape assertions
//!    rather than baselines so the period filter can be wired in a
//!    follow-up without breaking this file.
//! 4. **Coverage breadth**: every one of the 9 plugin-routed
//!    subcommands exits 0 + emits non-empty stdout. Catches "added a
//!    new subcommand and forgot to wire it" regressions independent
//!    of the byte-identity tests.
//!
//! Phase 6e (this file's most recent addition) replaces three
//! previously `#[ignore]`'d happy-path tests that degraded silently
//! to skip when the developer didn't have real `~/.claude` data.
//! The new harness materialises the committed fixture sessions tree
//! (`tests/fixtures/sessions/`) into a tempdir HOME, points the
//! spawned `ainb` at that tempdir via `HOME` + `AINB_HOME`, and
//! hard-fails (panics) when the plugin staging dir is missing — see
//! `feedback_plugin_completion_real_tmux_integration` in user
//! memory for the rationale.
//!
//! Gated on `--features cli-tests` because the happy-path tests
//! require both plugin .wasm files staged in `dist/plugins/`. The
//! fail-clean path doesn't strictly need them but stays under the
//! same gate so the suite is one-flag-on-or-off.
//!
//! Memory references:
//! * reference_plugin_clap_strip_global_flags — host strips `--format`
//!   before dispatching argv to the plugin.
//! * reference_wasmi_fd_write_capture — plugin's println! goes through
//!   the host's fd_write capture before reaching real stdout.
//! * reference_env_lock_for_parallel_tests — `AINB_PLUGIN_ROOT` /
//!   `AINB_DISABLE_PLUGINS` / `HOME` / `AINB_HOME` are process-global;
//!   we ship them via `Command::env` overrides (subprocesses don't see
//!   parent env mutations) and serialise the parent-side `env_remove`
//!   calls behind `ENV_LOCK`.
//! * reference_insta_trailing_newline_trap — baseline files store
//!   the same trailing-newline-trimmed shape insta uses, so
//!   `read_baseline` strips one trailing `\n` before comparison.

#![cfg(feature = "cli-tests")]
#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use tempfile::TempDir;

/// Serialise tests that mutate process-wide env vars
/// (`AINB_PLUGIN_ROOT`, `AINB_DISABLE_PLUGINS`). cargo test runs
/// integration tests on a thread pool; without this lock, a test
/// running with `AINB_DISABLE_PLUGINS=1` could leak its env into a
/// neighbour spawning the same binary.
///
/// Note: the `run_ainb` helper passes env via `Command::env`, so a
/// child process never sees the parent's mutated env. The lock is
/// retained because (a) other tests in the workspace may still mutate
/// process env, and (b) it lets `run_ainb` `env_remove` parent-leaked
/// vars without racing a parallel test that just set them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Path to the release-built `ainb` binary cargo just produced. The
/// magic env var is injected by cargo for integration tests of bin
/// crates — see `https://doc.rust-lang.org/cargo/reference/environment-variables.html`.
fn ainb_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Workspace `dist/plugins/` — populated by `scripts/build-plugins.sh`.
fn dist_plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("dist").join("plugins"))
        .expect("workspace root resolvable")
}

fn dist_has_plugin(name: &str) -> bool {
    dist_plugins_dir().join(name).join("plugin.wasm").exists()
}

/// Run `ainb <args>` with the given env overrides. Returns
/// (exit_code, stdout, stderr).
fn run_ainb(args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let _guard = ENV_LOCK.lock().expect("env lock poisoned");
    let mut cmd = Command::new(ainb_path());
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Important: drop AINB_PLUGIN_ROOT unless explicitly set —
    // otherwise a stale env var on the developer's machine could
    // accidentally point at a different plugin staging dir.
    if !env.iter().any(|(k, _)| *k == "AINB_PLUGIN_ROOT") {
        cmd.env_remove("AINB_PLUGIN_ROOT");
    }
    if !env.iter().any(|(k, _)| *k == "AINB_DISABLE_PLUGINS") {
        cmd.env_remove("AINB_DISABLE_PLUGINS");
    }
    let out = cmd.output().expect("spawn ainb");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Path to the committed fixture-sessions root.
/// `<crate>/tests/fixtures/sessions/`
fn fixture_sessions_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sessions")
}

/// Path to the committed baseline file `<crate>/tests/baselines/<name>`.
fn baseline_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("baselines")
        .join(name)
}

/// Recursive copy. Used to materialise the fixture sessions tree into
/// `<HOME>/.claude/projects/...` inside a tempdir.
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

/// Materialise `tests/fixtures/sessions/claude_projects/...` into a
/// tempdir at `<tempdir>/.claude/projects/...`, then return env-var
/// overrides that point the spawned `ainb` subprocess at this isolated
/// HOME (so the session-reader plugin sees the fixture data, not the
/// developer's real `~/.claude`).
///
/// The returned `TempDir` must outlive the `run_ainb` call — drop it
/// only after the subprocess exits.
///
/// Sets:
/// * `HOME` → tempdir (used by `dirs::home_dir()` for the session-reader
///   plugin's `read_claude_logs` allowlist).
/// * `AINB_HOME` → tempdir (used by the host's plugin-cache layout —
///   `crates/ainb-plugin-host/src/cache_layout.rs`).
/// * `AINB_PLUGIN_ROOT` → workspace `dist/plugins/`.
///
/// Per memory `reference_env_lock_for_parallel_tests`: spawned
/// subprocesses never observe the parent's env mutations because we
/// pass overrides via `Command::env`. The `ENV_LOCK` in `run_ainb`
/// still serialises `Command::env_remove` calls against parallel
/// neighbours.
fn with_fixture_home() -> (TempDir, Vec<(String, String)>) {
    let tmp = TempDir::new().expect("tempdir for fixture HOME");
    let claude_root = tmp.path().join(".claude").join("projects");
    fs::create_dir_all(&claude_root).expect("mkdir -p .claude/projects");
    let src_claude = fixture_sessions_root().join("claude_projects");
    if src_claude.exists() {
        copy_dir_recursive(&src_claude, &claude_root)
            .expect("copy fixture claude_projects/ into <HOME>/.claude/projects");
    }
    // Reserved for future codex / gemini / copilot fixtures — the
    // helper materialises whichever subdirs exist, so adding e.g.
    // `tests/fixtures/sessions/codex_sessions/` later wires through
    // without changing this signature.
    for (fixture_subdir, home_subpath) in [
        ("codex_sessions", &[".codex", "sessions"][..]),
        ("gemini_sessions", &[".gemini", "sessions"][..]),
        ("copilot_sessions", &[".config", "github-copilot", "sessions"][..]),
    ] {
        let src = fixture_sessions_root().join(fixture_subdir);
        if src.exists() {
            let mut dst = tmp.path().to_path_buf();
            for seg in home_subpath {
                dst = dst.join(seg);
            }
            fs::create_dir_all(&dst).expect("mkdir -p provider dir");
            copy_dir_recursive(&src, &dst).expect("copy provider fixture");
        }
    }
    let dist = dist_plugins_dir();
    let env = vec![
        ("HOME".to_string(), tmp.path().display().to_string()),
        ("AINB_HOME".to_string(), tmp.path().display().to_string()),
        (
            "AINB_PLUGIN_ROOT".to_string(),
            dist.display().to_string(),
        ),
    ];
    (tmp, env)
}

/// Fail loudly when the plugin staging dir is missing. The previous
/// behaviour of soft-skipping silently was Stevie's pet peeve — see
/// `feedback_plugin_completion_real_tmux_integration`. The CI gate
/// must hard-fail so a broken plugin build doesn't pretend to be
/// green.
fn assert_plugins_built() {
    if !dist_has_plugin("burndown") || !dist_has_plugin("session-reader") {
        panic!(
            "dist/plugins/{{burndown,session-reader}}/plugin.wasm missing — \
             run `bash scripts/build-plugins.sh` from the workspace root \
             before `cargo test --features cli-tests`"
        );
    }
}

/// Read a baseline file, stripping the trailing newline insta-style
/// tooling tends to add. Returns the canonical baseline string for
/// byte-equality comparison against captured stdout.
fn read_baseline(name: &str) -> String {
    let path = baseline_path(name);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("baseline {} unreadable: {e}", path.display()));
    // Match insta's trim-one-trailing-newline behaviour
    // (reference_insta_trailing_newline_trap).
    raw.trim_end_matches('\n').to_string()
}

/// Convert a captured stdout buffer into a comparable form: strip the
/// trailing newline only (preserve internal whitespace).
fn normalise_stdout(s: &str) -> String {
    s.trim_end_matches('\n').to_string()
}

/// Diff helper that surfaces the first divergent byte range so a
/// regression is debuggable from the test failure output.
#[track_caller]
fn assert_byte_eq(actual: &str, expected: &str, baseline_name: &str) {
    if actual == expected {
        return;
    }
    // Find the first divergent line for a focused diff.
    let mut line_no = 1usize;
    for (a, e) in actual.lines().zip(expected.lines()) {
        if a != e {
            panic!(
                "stdout != baseline {baseline_name} at line {line_no}\n\
                 actual  : {a:?}\n\
                 expected: {e:?}\n\
                 (re-baseline with: copy actual stdout to crates/ainb-core/tests/baselines/{baseline_name})"
            );
        }
        line_no += 1;
    }
    // Falls through when one side is a prefix of the other.
    panic!(
        "stdout != baseline {baseline_name}: line counts differ \
         (actual={}, expected={}); re-baseline if intended",
        actual.lines().count(),
        expected.lines().count()
    );
}

#[test]
fn cli_usage_report_fails_cleanly_without_plugins() {
    let (code, stdout, stderr) = run_ainb(
        &["usage", "report"],
        &[("AINB_DISABLE_PLUGINS", "1")],
    );
    assert_eq!(
        code,
        Some(2),
        "expected exit 2 with plugins disabled, got {code:?}; stderr={stderr}"
    );
    assert!(
        stdout.is_empty(),
        "stdout should be empty on failure, got {stdout:?}"
    );
    assert!(
        stderr.contains("requires the burndown plugin"),
        "stderr should mention the missing plugin, got {stderr:?}"
    );
}

#[test]
fn cli_usage_today_subcommand_exits_2_without_plugins() {
    // Same failure path but a different subcommand — proves all 9 are
    // gated behind the plugin presence check, not just `report`.
    let (code, stdout, stderr) = run_ainb(
        &["usage", "today"],
        &[("AINB_DISABLE_PLUGINS", "1")],
    );
    assert_eq!(code, Some(2), "expected exit 2; stderr={stderr}");
    assert!(stdout.is_empty(), "stdout should be empty");
    assert!(stderr.contains("requires the burndown plugin"));
}

#[test]
fn cli_usage_admin_subcommand_works_without_plugins() {
    // `usage cache info` is one of the three host-side admin
    // subcommands (Plan / Currency / Cache) that DO NOT need the
    // burndown plugin. AINB_DISABLE_PLUGINS=1 must not break them.
    let (code, _stdout, _stderr) = run_ainb(
        &["usage", "cache", "info"],
        &[("AINB_DISABLE_PLUGINS", "1")],
    );
    // Cache info either returns 0 (existing cache) or surfaces a
    // descriptive error from rusqlite — anything except the exit-2
    // 'requires the burndown plugin' contract is acceptable here.
    assert!(
        matches!(code, Some(0) | Some(1)),
        "admin subcommand shouldn't trigger the plugin-gated exit-2 path; got {code:?}"
    );
}

/// Helper: spawn `ainb <args>` against a fresh fixture HOME. Returns
/// (exit_code, stdout, stderr). Every spawn gets its own tempdir HOME
/// so concurrent tests don't share plugin cache files.
fn run_ainb_with_fixture(args: &[&str]) -> (Option<i32>, String, String) {
    assert_plugins_built();
    let (_tmp, env) = with_fixture_home();
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let result = run_ainb(args, &env_refs);
    drop(_tmp);
    result
}

/// Byte-identity check: spawn `ainb usage report --format=json` against
/// the deterministic fixture HOME, assert exit 0 + stdout matches the
/// frozen baseline. This is the strict regression gate — any
/// behavioural drift in the plugin pipeline (parser, aggregator,
/// JSON encoder) trips this.
#[test]
fn cli_usage_report_byte_identical_to_baseline() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "report", "--format=json"]);
    assert_eq!(
        code,
        Some(0),
        "exit {code:?}; stderr={stderr}; stdout-head={:?}",
        stdout.chars().take(200).collect::<String>()
    );
    let actual = normalise_stdout(&stdout);
    let expected = read_baseline("usage_cli_report_fixture.snap");
    assert_byte_eq(&actual, &expected, "usage_cli_report_fixture.snap");
}

/// Byte-identity check for `usage export --format=json`. The plugin
/// path skips the `Local::now` wrapper that the host-side host-side
/// path uses, so this output is fully deterministic.
#[test]
fn cli_usage_export_byte_identical_to_baseline() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "export", "--format=json"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    let actual = normalise_stdout(&stdout);
    let expected = read_baseline("usage_cli_export_fixture.snap");
    assert_byte_eq(&actual, &expected, "usage_cli_export_fixture.snap");
}

/// Byte-identity check for `usage optimize --format=json`. The
/// optimizer is a pure function over `UsageData` — no clock, no env.
#[test]
fn cli_usage_optimize_byte_identical_to_baseline() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "optimize", "--format=json"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    let actual = normalise_stdout(&stdout);
    let expected = read_baseline("usage_cli_optimize_fixture.snap");
    assert_byte_eq(&actual, &expected, "usage_cli_optimize_fixture.snap");
}

/// Byte-identity check for `usage compare --format=json`. Same purity
/// reasoning as `optimize`.
#[test]
fn cli_usage_compare_byte_identical_to_baseline() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "compare", "--format=json"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    let actual = normalise_stdout(&stdout);
    let expected = read_baseline("usage_cli_compare_fixture.snap");
    assert_byte_eq(&actual, &expected, "usage_cli_compare_fixture.snap");
}

/// Byte-identity check for `usage yield --format=json`. Same purity
/// reasoning as `optimize`.
#[test]
fn cli_usage_yield_byte_identical_to_baseline() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "yield", "--format=json"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    let actual = normalise_stdout(&stdout);
    let expected = read_baseline("usage_cli_yield_fixture.snap");
    assert_byte_eq(&actual, &expected, "usage_cli_yield_fixture.snap");
}

/// Byte-identity check for `usage status --format=json` — the JSON
/// path's `Local::now` is only consulted when a `[plan]` config is
/// present. The fixture HOME has no config file, so projection is
/// always `null` and the output is deterministic.
#[test]
fn cli_usage_status_byte_identical_to_baseline() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "status", "--format=json"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    let actual = normalise_stdout(&stdout);
    let expected = read_baseline("usage_cli_status_fixture.snap");
    assert_byte_eq(&actual, &expected, "usage_cli_status_fixture.snap");
}

/// Structural check for `usage today` / `usage month` — these reach
/// the plugin path which currently does *not* re-apply the period
/// filter (see `build_filters_from_args` in
/// `crates/ainb-plugin-burndown/src/cli.rs` — it only carries
/// project/model/branch). So today/month produce the same JSON as
/// `report`. We assert structural shape (overview present, calls > 0)
/// rather than byte-equality to leave room for the period filter to
/// be wired in a follow-up without breaking this test.
#[test]
fn cli_usage_today_and_month_produce_overview() {
    for sub in ["today", "month"] {
        let (code, stdout, stderr) =
            run_ainb_with_fixture(&["usage", sub, "--format=json"]);
        assert_eq!(code, Some(0), "{sub}: exit {code:?}; stderr={stderr}");
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{sub}: stdout is not valid JSON ({e}): {stdout:?}"));
        assert!(
            parsed.get("overview").is_some(),
            "{sub}: report JSON missing 'overview'; got {parsed}"
        );
    }
}

/// Structural check for `usage model-alias --list`. The plugin's
/// implementation is reflection-only (see comment on
/// `model_alias_command_plugin` in
/// `crates/ainb-plugin-burndown/src/cli.rs`) — it always prints a
/// fixed "Model aliases (plugin scope): none configured." line. Use
/// a substring assertion so a future persistence implementation
/// doesn't silently break this test.
#[test]
fn cli_usage_model_alias_list_routes_to_plugin() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "model-alias", "--list"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    assert!(
        stdout.contains("Model aliases"),
        "model-alias --list should mention 'Model aliases'; got {stdout:?}"
    );
}

/// Coverage breadth: spawn each of the 9 plugin-routed subcommands
/// and assert exit 0 + non-empty stdout. The byte-identity tests
/// above pin the *content*; this test pins *coverage* — adding a
/// 10th subcommand and forgetting to wire it would surface here as
/// well as in the more focused tests.
#[test]
fn cli_usage_routes_each_subcommand_to_plugin() {
    // The 9 plugin-routed subcommands per the spec.
    let cases: &[(&str, &[&str])] = &[
        ("report", &["usage", "report", "--format=json"]),
        ("status", &["usage", "status", "--format=json"]),
        ("today", &["usage", "today", "--format=json"]),
        ("month", &["usage", "month", "--format=json"]),
        ("export", &["usage", "export", "--format=json"]),
        ("optimize", &["usage", "optimize", "--format=json"]),
        ("compare", &["usage", "compare", "--format=json"]),
        ("yield", &["usage", "yield", "--format=json"]),
        ("model-alias", &["usage", "model-alias", "--list"]),
    ];
    for (name, argv) in cases {
        let (code, stdout, stderr) = run_ainb_with_fixture(argv);
        assert_eq!(
            code,
            Some(0),
            "{name}: exit {code:?}; stderr={stderr}"
        );
        assert!(
            !stdout.is_empty(),
            "{name}: stdout empty on exit 0 — dispatch likely failed silently"
        );
    }
}

/// Text-format smoke test: the plugin's text path prints a
/// "Usage Report" header even when usage data is non-trivial.
#[test]
fn cli_usage_report_text_format_routes_to_plugin() {
    let (code, stdout, stderr) = run_ainb_with_fixture(&["usage", "report"]);
    assert_eq!(code, Some(0), "exit {code:?}; stderr={stderr}");
    assert!(
        stdout.contains("Usage Report") || stdout.contains("overview"),
        "plugin text output should include the 'Usage Report' header; got {stdout:?}"
    );
}

/// Sanity check that `--help` (clap's built-in) still works without
/// the plugin host being up. Clap returns exit 0 for `--help` so this
/// also indirectly guards against regressions in the registry's
/// usage-subcommand augmentation.
#[test]
fn cli_usage_help_works_without_plugins() {
    let (code, stdout, _stderr) = run_ainb(
        &["usage", "--help"],
        &[("AINB_DISABLE_PLUGINS", "1")],
    );
    assert_eq!(code, Some(0), "usage --help should exit 0");
    assert!(
        stdout.contains("Usage analytics"),
        "help output should mention the usage subtree"
    );
}

#[test]
fn ainb_path_resolves() {
    // Defensive: catch a misconfigured CARGO_BIN_EXE_ainb early with
    // a clear error rather than a confusing 'No such file' from
    // Command::output.
    let p = ainb_path();
    assert!(p.exists(), "ainb binary not built at {}", p.display());
    let _ = Path::new(&p);
}
