//! `ainb plugin lint <id_or_path>` — offline manifest + binary sanity check.
//!
//! Two input shapes accepted:
//!
//! 1. A path on disk pointing at either a plugin staging dir (containing
//!    `manifest.toml` + `<name>` binary) or a manifest file directly.
//! 2. A plain plugin id, resolved against the same search order
//!    [`crate::plugins::discover_plugin_root`] uses at runtime startup.
//!
//! The check is intentionally conservative — every probe failure produces
//! an actionable message. A clean run prints a one-line summary on stdout
//! and returns `Ok(())`; any failure propagates an `anyhow::Error` so the
//! main binary exits non-zero. JSON output emits a structured report
//! suitable for CI gating.
//!
//! What we verify (matches the dispatch contract):
//!
//! - Manifest schema parses against the Phase 7 ABI v2 [`Manifest`] type.
//! - `[plugin].abi_version == 2` (compat gate the runtime enforces too).
//! - `[plugin].name` matches the staging directory name.
//! - Binary path exists and is a regular file.
//! - On Unix, binary has any execute bit set (owner/group/other).
//! - `binary --version` exits cleanly inside a 5s timeout and emits a
//!   non-empty line. A binary without `--version` support is *not* fatal
//!   — we record the failure as a `warning` and keep going.
//! - Capability declarations are coherent (no impossible combinations
//!   like `read_claude_logs = true` while `read_sessions = false`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ainb_plugin_protocol::manifest::Manifest;
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::cli::OutputFormat;

/// Entry point. `arg` is either a plugin id or a filesystem path.
pub fn run(arg: &str, format: OutputFormat) -> Result<()> {
    let resolved = resolve_target(arg)?;
    let report = lint_target(&resolved);

    let ok = report.is_clean();
    emit_report(&report, format)?;

    if ok {
        Ok(())
    } else {
        Err(anyhow!(
            "plugin lint failed: {} error(s), {} warning(s)",
            report.errors.len(),
            report.warnings.len()
        ))
    }
}

/// On-disk target for a lint run.
#[derive(Debug)]
struct LintTarget {
    /// Plugin staging dir (`<root>/<name>/`).
    plugin_dir: PathBuf,
    /// Path to `manifest.toml` inside the staging dir.
    manifest_path: PathBuf,
    /// Originating user input (id or path) — surfaced in the report.
    input: String,
}

/// Resolve `arg` to a concrete on-disk plugin staging dir. The input is
/// treated as a path first (fast filesystem checks); if that fails we
/// fall back to id resolution against `discover_plugin_root`.
fn resolve_target(arg: &str) -> Result<LintTarget> {
    let candidate = PathBuf::from(arg);
    if candidate.exists() {
        return resolve_from_path(&candidate, arg);
    }
    resolve_from_id(arg)
}

fn resolve_from_path(p: &Path, original: &str) -> Result<LintTarget> {
    if p.is_dir() {
        let manifest = p.join("manifest.toml");
        if !manifest.exists() {
            bail!(
                "no manifest.toml in {} — pass a plugin staging dir or a manifest path",
                p.display()
            );
        }
        return Ok(LintTarget {
            plugin_dir: p.to_path_buf(),
            manifest_path: manifest,
            input: original.to_string(),
        });
    }
    if p.is_file()
        && p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("manifest.toml"))
    {
        let dir = p
            .parent()
            .ok_or_else(|| anyhow!("manifest path has no parent"))?
            .to_path_buf();
        return Ok(LintTarget {
            plugin_dir: dir,
            manifest_path: p.to_path_buf(),
            input: original.to_string(),
        });
    }
    bail!(
        "unsupported path target {}: pass a directory or a manifest.toml",
        p.display()
    );
}

fn resolve_from_id(id: &str) -> Result<LintTarget> {
    let root = crate::plugins::discover_plugin_root()
        .ok_or_else(|| anyhow!("no plugin root discovered; set AINB_PLUGIN_ROOT"))?;
    let dir = root.join(id);
    if !dir.exists() {
        bail!(
            "plugin '{id}' not found under {}",
            root.display()
        );
    }
    let manifest = dir.join("manifest.toml");
    if !manifest.exists() {
        bail!(
            "plugin '{id}' is missing manifest.toml in {}",
            dir.display()
        );
    }
    Ok(LintTarget {
        plugin_dir: dir,
        manifest_path: manifest,
        input: id.to_string(),
    })
}

/// One probe outcome — a finding for the user.
#[derive(Debug, Serialize, Clone)]
struct Finding {
    check: String,
    detail: String,
}

/// Aggregated result of every probe against a single target.
#[derive(Debug, Serialize)]
struct LintReport {
    input: String,
    plugin_dir: PathBuf,
    manifest_path: PathBuf,
    /// `Some` when the manifest parsed cleanly. We still want to surface
    /// non-manifest checks even when manifest parsing fails for the
    /// first probe (parse error itself goes into `errors`).
    plugin_name: Option<String>,
    plugin_version: Option<String>,
    abi_version: Option<u32>,
    binary_path: Option<PathBuf>,
    successes: Vec<Finding>,
    warnings: Vec<Finding>,
    errors: Vec<Finding>,
}

impl LintReport {
    fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

fn lint_target(t: &LintTarget) -> LintReport {
    let mut report = LintReport {
        input: t.input.clone(),
        plugin_dir: t.plugin_dir.clone(),
        manifest_path: t.manifest_path.clone(),
        plugin_name: None,
        plugin_version: None,
        abi_version: None,
        binary_path: None,
        successes: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let manifest = match read_manifest(&t.manifest_path) {
        Ok(m) => {
            report.plugin_name = Some(m.plugin.name.clone());
            report.plugin_version = Some(m.plugin.version.clone());
            report.abi_version = Some(m.plugin.abi_version);
            report.successes.push(Finding {
                check: "manifest_parse".into(),
                detail: format!("{} v{}", m.plugin.name, m.plugin.version),
            });
            Some(m)
        }
        Err(e) => {
            report.errors.push(Finding {
                check: "manifest_parse".into(),
                detail: format!("{e:#}"),
            });
            None
        }
    };

    if let Some(m) = manifest {
        check_abi_version(&m, &mut report);
        check_name_matches_dir(&m, t, &mut report);
        check_capabilities_coherent(&m, &mut report);

        let binary_path = t.plugin_dir.join(&m.plugin.name);
        report.binary_path = Some(binary_path.clone());

        check_binary_exists(&binary_path, &mut report);
        if binary_path.exists() {
            check_binary_executable(&binary_path, &mut report);
            check_binary_version(&binary_path, &mut report);
        }
    }

    report
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let manifest: Manifest = toml::from_str(&raw)
        .with_context(|| format!("parse {} as Manifest v2", path.display()))?;
    Ok(manifest)
}

fn check_abi_version(m: &Manifest, report: &mut LintReport) {
    if m.plugin.abi_version == 2 {
        report.successes.push(Finding {
            check: "abi_version".into(),
            detail: "abi_version=2".into(),
        });
    } else {
        report.errors.push(Finding {
            check: "abi_version".into(),
            detail: format!(
                "abi_version={} — runtime requires 2",
                m.plugin.abi_version
            ),
        });
    }
}

fn check_name_matches_dir(m: &Manifest, t: &LintTarget, report: &mut LintReport) {
    let dir_name = t
        .plugin_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if dir_name.is_empty() {
        return;
    }
    if dir_name == m.plugin.name {
        report.successes.push(Finding {
            check: "name_matches_dir".into(),
            detail: format!("dir={dir_name} matches plugin.name"),
        });
    } else {
        // Discovery scans by directory name, so a mismatch means the
        // runtime registers the plugin under the wrong id.
        report.errors.push(Finding {
            check: "name_matches_dir".into(),
            detail: format!(
                "[plugin].name='{}' but staging dir is '{dir_name}' — runtime will register the wrong id",
                m.plugin.name
            ),
        });
    }
}

fn check_capabilities_coherent(m: &Manifest, report: &mut LintReport) {
    let caps = &m.capabilities;
    let has_session_caps = caps.read_sessions.is_granted();
    let claude = caps.read_claude_logs.is_granted();
    let codex = caps.read_codex_logs.is_granted();

    // read_*_logs without read_sessions is meaningless — the runtime
    // bundles per-provider log dirs under the same fs guard surface.
    if (claude || codex) && !has_session_caps {
        report.warnings.push(Finding {
            check: "capabilities".into(),
            detail: "read_claude_logs/read_codex_logs granted without read_sessions — \
                     log paths live under the sessions guard, the per-provider grant \
                     has no effect on its own".into(),
        });
    }
    report.successes.push(Finding {
        check: "capabilities".into(),
        detail: "no incoherent grant combinations".into(),
    });
}

fn check_binary_exists(path: &Path, report: &mut LintReport) {
    if path.exists() && path.is_file() {
        report.successes.push(Finding {
            check: "binary_exists".into(),
            detail: path.display().to_string(),
        });
    } else {
        report.errors.push(Finding {
            check: "binary_exists".into(),
            detail: format!(
                "{} not found — runtime expects <plugin_dir>/<name>",
                path.display()
            ),
        });
    }
}

#[cfg(unix)]
fn check_binary_executable(path: &Path, report: &mut LintReport) {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(path) {
        Ok(md) => {
            let mode = md.permissions().mode();
            if mode & 0o111 != 0 {
                report.successes.push(Finding {
                    check: "binary_executable".into(),
                    detail: format!("mode={:o}", mode & 0o777),
                });
            } else {
                report.errors.push(Finding {
                    check: "binary_executable".into(),
                    detail: format!(
                        "{} has no execute bit (mode={:o}) — chmod +x",
                        path.display(),
                        mode & 0o777
                    ),
                });
            }
        }
        Err(e) => {
            report.errors.push(Finding {
                check: "binary_executable".into(),
                detail: format!("stat {}: {e}", path.display()),
            });
        }
    }
}

#[cfg(not(unix))]
fn check_binary_executable(_path: &Path, report: &mut LintReport) {
    report.successes.push(Finding {
        check: "binary_executable".into(),
        detail: "skipped on non-unix".into(),
    });
}

fn check_binary_version(path: &Path, report: &mut LintReport) {
    let timeout = Duration::from_secs(5);
    match run_with_timeout(path, &["--version"], timeout) {
        Ok((status, stdout, stderr)) if status.success() => {
            let line = first_nonempty_line(&stdout)
                .or_else(|| first_nonempty_line(&stderr))
                .unwrap_or_default();
            if line.is_empty() {
                report.warnings.push(Finding {
                    check: "binary_version".into(),
                    detail: "--version exited 0 but produced no output".into(),
                });
            } else {
                report.successes.push(Finding {
                    check: "binary_version".into(),
                    detail: line,
                });
            }
        }
        Ok((status, _, stderr)) => {
            report.warnings.push(Finding {
                check: "binary_version".into(),
                detail: format!(
                    "--version exited {} ({})",
                    status.code().unwrap_or(-1),
                    first_nonempty_line(&stderr).unwrap_or_default()
                ),
            });
        }
        Err(e) => {
            report.warnings.push(Finding {
                check: "binary_version".into(),
                detail: format!("--version probe failed: {e}"),
            });
        }
    }
}

fn first_nonempty_line(buf: &[u8]) -> Option<String> {
    String::from_utf8_lossy(buf)
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|s| s.trim().to_string())
}

/// Spawn `bin args...` with a wall-clock timeout. Returns the exit
/// status + captured stdout/stderr on success; bubbles any spawn error.
/// On timeout the child is force-killed and the call returns Err.
fn run_with_timeout(
    bin: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {} {:?}", bin.display(), args))?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut s) = child.stdout.take() {
                use std::io::Read;
                let _ = s.read_to_end(&mut stdout);
            }
            if let Some(mut s) = child.stderr.take() {
                use std::io::Read;
                let _ = s.read_to_end(&mut stderr);
            }
            return Ok((status, stdout, stderr));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {:?}", timeout);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn emit_report(report: &LintReport, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(report)?);
        }
        OutputFormat::Text | OutputFormat::Csv => {
            let label = report
                .plugin_name
                .as_deref()
                .unwrap_or(&report.input);
            println!(
                "lint: {label} ({} successes, {} warnings, {} errors)",
                report.successes.len(),
                report.warnings.len(),
                report.errors.len()
            );
            for f in &report.successes {
                println!("  ok   {:24} {}", f.check, f.detail);
            }
            for f in &report.warnings {
                println!("  warn {:24} {}", f.check, f.detail);
            }
            for f in &report.errors {
                println!("  err  {:24} {}", f.check, f.detail);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("manifest.toml");
        fs::write(&p, body).unwrap();
        p
    }

    fn make_executable(p: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(p).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(p, perms).unwrap();
        }
        #[cfg(not(unix))]
        let _ = p;
    }

    fn good_manifest() -> &'static str {
        r#"
[plugin]
name = "fixture"
version = "0.1.0"
abi_version = 2
description = "lint test fixture"
"#
    }

    fn make_fixture_plugin(name: &str, manifest_body: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join(name);
        fs::create_dir_all(&plugin_dir).unwrap();
        write_manifest(&plugin_dir, manifest_body);
        // Use /bin/sh as a stand-in binary that supports --version.
        let bin_path = plugin_dir.join(name);
        // Symlinking lets us reuse a real executable without needing
        // to compile one. On platforms without /bin/sh the test is
        // skipped via cfg(unix) above for the executable check; the
        // exists-check still passes on a regular file.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/bin/sh", &bin_path).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::write(&bin_path, b"stub").unwrap();
        }
        let _ = bin_path;
        tmp
    }

    #[test]
    fn lint_clean_fixture_passes() {
        let tmp = make_fixture_plugin("fixture", good_manifest());
        let report = lint_target(&LintTarget {
            plugin_dir: tmp.path().join("fixture"),
            manifest_path: tmp.path().join("fixture/manifest.toml"),
            input: "fixture".into(),
        });
        assert!(
            report.is_clean(),
            "expected clean lint, got errors: {:?}",
            report.errors
        );
        assert!(
            report
                .successes
                .iter()
                .any(|f| f.check == "manifest_parse"),
            "manifest_parse should be among successes"
        );
    }

    #[test]
    fn lint_rejects_wrong_abi_version() {
        let body = r#"
[plugin]
name = "fixture"
version = "0.1.0"
abi_version = 1
"#;
        let tmp = make_fixture_plugin("fixture", body);
        let report = lint_target(&LintTarget {
            plugin_dir: tmp.path().join("fixture"),
            manifest_path: tmp.path().join("fixture/manifest.toml"),
            input: "fixture".into(),
        });
        assert!(!report.is_clean());
        assert!(
            report.errors.iter().any(|f| f.check == "abi_version"),
            "expected abi_version error in: {:?}",
            report.errors
        );
    }

    #[test]
    fn lint_rejects_name_dir_mismatch() {
        let body = r#"
[plugin]
name = "actually-different"
version = "0.1.0"
abi_version = 2
"#;
        let tmp = make_fixture_plugin("fixture", body);
        let report = lint_target(&LintTarget {
            plugin_dir: tmp.path().join("fixture"),
            manifest_path: tmp.path().join("fixture/manifest.toml"),
            input: "fixture".into(),
        });
        assert!(!report.is_clean());
        assert!(
            report
                .errors
                .iter()
                .any(|f| f.check == "name_matches_dir"),
            "expected name_matches_dir error: {:?}",
            report.errors
        );
    }

    #[test]
    fn lint_reports_missing_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("fixture");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_manifest(&plugin_dir, good_manifest());
        // No binary written.
        let report = lint_target(&LintTarget {
            plugin_dir: plugin_dir.clone(),
            manifest_path: plugin_dir.join("manifest.toml"),
            input: "fixture".into(),
        });
        assert!(!report.is_clean());
        assert!(report.errors.iter().any(|f| f.check == "binary_exists"));
    }

    #[test]
    fn lint_warns_on_log_caps_without_sessions() {
        let body = r#"
[plugin]
name = "fixture"
version = "0.1.0"
abi_version = 2

[capabilities]
read_claude_logs = true
"#;
        let tmp = make_fixture_plugin("fixture", body);
        let report = lint_target(&LintTarget {
            plugin_dir: tmp.path().join("fixture"),
            manifest_path: tmp.path().join("fixture/manifest.toml"),
            input: "fixture".into(),
        });
        assert!(report.is_clean(), "warning shouldn't fail run");
        assert!(report.warnings.iter().any(|f| f.check == "capabilities"));
    }

    #[test]
    fn run_with_timeout_kills_hung_processes() {
        let res = run_with_timeout(
            Path::new("/bin/sh"),
            &["-c", "sleep 5"],
            Duration::from_millis(100),
        );
        assert!(res.is_err(), "expected timeout error, got {res:?}");
    }

    #[test]
    fn resolve_target_path_to_dir_works() {
        let tmp = make_fixture_plugin("fixture", good_manifest());
        let dir = tmp.path().join("fixture");
        let t = resolve_target(dir.to_str().unwrap()).expect("resolve dir");
        assert_eq!(t.plugin_dir, dir);
    }

    #[test]
    fn resolve_target_path_to_manifest_works() {
        let tmp = make_fixture_plugin("fixture", good_manifest());
        let manifest = tmp.path().join("fixture/manifest.toml");
        let t = resolve_target(manifest.to_str().unwrap()).expect("resolve manifest");
        assert_eq!(t.manifest_path, manifest);
    }
}
