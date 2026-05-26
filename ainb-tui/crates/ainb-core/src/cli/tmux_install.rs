// ABOUTME: `ainb tmux install` / `ainb tmux status` — manage the bundled
// rich tmux.conf at ~/.tmux.conf.
//
// `install` workflow:
//   1. Read existing ~/.tmux.conf (if any) and compare to the bundled conf.
//   2. If identical → exit early with a friendly "already installed" message.
//   3. If different → show a unified diff preview, prompt y/N (unless --yes).
//   4. Back up the existing file to ~/.tmux.conf.bak.<UTC-timestamp>.
//   5. Write the bundled conf to ~/.tmux.conf.
//   6. Deploy the bundled helper script(s) under ~/.tmux/scripts/.
//   7. Best-effort: clone TPM (~/.tmux/plugins/tpm) and install plugins.
//   8. Best-effort: `tmux source-file ~/.tmux.conf` to reload live sessions.
//
// `status` workflow: report not-installed / installed / stale, and show
// where the backup lives (if any).

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;

use super::OutputFormat;

/// The bundled rich tmux.conf shipped with the `ainb` binary.
const BUNDLED_CONF: &str = include_str!("../../../../config/tmux.conf");

/// Helper script the rich conf shells out to from status-right to render the
/// active pane's git branch.
const GIT_BRANCH_HELPER: &str =
    include_str!("../../../../config/tmux-helpers/git_branch.sh");

/// Default install path: `~/.tmux.conf`.
fn tmux_conf_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".tmux.conf"))
}

/// TPM clone destination: `~/.tmux/plugins/tpm`.
fn tpm_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".tmux/plugins/tpm"))
}

/// Where the git-branch helper gets deployed.
fn helper_script_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".tmux/scripts/git_branch.sh"))
}

/// Deploy the embedded helper script and chmod +x. Best-effort: returns false
/// on I/O failure but never errors the install.
fn deploy_helpers() -> bool {
    let Ok(dest) = helper_script_path() else { return false };
    if let Some(parent) = dest.parent() {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    if fs::write(&dest, GIT_BRANCH_HELPER).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));
    }
    true
}

/// Whether the user's `~/.tmux.conf` matches what we ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    /// File does not exist.
    Missing,
    /// File exists and is byte-identical to the bundled conf.
    UpToDate,
    /// File exists and differs from the bundled conf.
    Stale,
}

impl InstallState {
    fn label(self) -> &'static str {
        match self {
            InstallState::Missing => "not installed",
            InstallState::UpToDate => "up to date",
            InstallState::Stale => "stale (bundled conf differs from on-disk)",
        }
    }
}

fn detect_state(path: &Path) -> Result<InstallState> {
    if !path.exists() {
        return Ok(InstallState::Missing);
    }
    let on_disk = fs::read_to_string(path)
        .with_context(|| format!("read existing {}", path.display()))?;
    Ok(if on_disk == BUNDLED_CONF {
        InstallState::UpToDate
    } else {
        InstallState::Stale
    })
}

/// Produce a unified-diff-ish preview between the on-disk conf and the
/// bundled conf, capped at `max_lines` so it fits comfortably in a terminal.
/// Lines only on disk are prefixed `-`, lines only in the bundle `+`.
fn diff_preview(on_disk: &str, bundled: &str, max_lines: usize) -> String {
    let a: Vec<&str> = on_disk.lines().collect();
    let b: Vec<&str> = bundled.lines().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < a.len() && j < b.len() && out.len() < max_lines {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        // Emit one removal + one addition, advance both — naive but fine for a
        // preview. The user reviews intent, not byte-perfect diff alignment.
        out.push(format!("- {}", a[i]));
        if out.len() < max_lines {
            out.push(format!("+ {}", b[j]));
        }
        i += 1;
        j += 1;
    }
    while i < a.len() && out.len() < max_lines {
        out.push(format!("- {}", a[i]));
        i += 1;
    }
    while j < b.len() && out.len() < max_lines {
        out.push(format!("+ {}", b[j]));
        j += 1;
    }
    let remaining = (a.len().saturating_sub(i)) + (b.len().saturating_sub(j));
    if remaining > 0 {
        out.push(format!("  … {remaining} more line(s) differ"));
    }
    out.join("\n")
}

fn timestamp_suffix() -> String {
    Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

fn backup_existing(path: &Path) -> Result<PathBuf> {
    let suffix = timestamp_suffix();
    let backup = path.with_extension(format!("conf.bak.{suffix}"));
    fs::copy(path, &backup)
        .with_context(|| format!("backup {} → {}", path.display(), backup.display()))?;
    Ok(backup)
}

fn write_conf(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    fs::write(path, BUNDLED_CONF).with_context(|| format!("write {}", path.display()))
}

fn prompt_yes_no(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).context("read stdin")?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

/// Best-effort: clone TPM if missing. Returns true if TPM is present after
/// the call (either was already there, or we cloned it). Never errors —
/// network failures are noted but not fatal to the install.
fn ensure_tpm() -> bool {
    let Ok(dest) = tpm_path() else { return false };
    if dest.join("tpm").exists() {
        return true;
    }
    if let Some(parent) = dest.parent() {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let status = Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "https://github.com/tmux-plugins/tpm",
            dest.to_str().unwrap_or(""),
        ])
        .status();
    match status {
        Ok(s) if s.success() => true,
        _ => false,
    }
}

/// Best-effort: run TPM's install hook so plugins are ready immediately.
///
/// `bin/install_plugins` needs `TMUX_PLUGIN_MANAGER_PATH` in its env, which is
/// only set once the tmux server has sourced a conf containing TPM's `run`
/// directive. So we invoke it via `tmux run-shell` to inherit the live server
/// env. Falls back to a direct invocation (with the env var injected manually)
/// when no server is running yet — that path silently no-ops if TPM init
/// hasn't happened, which is fine: `prefix + I` inside tmux always works.
fn run_tpm_install() -> bool {
    let Ok(tpm) = tpm_path() else { return false };
    let script = tpm.join("bin/install_plugins");
    if !script.exists() {
        return false;
    }
    let Ok(home) = dirs::home_dir().ok_or(()) else { return false };
    let plugin_dir = home.join(".tmux/plugins/");
    let script_str = script.to_str().unwrap_or("");

    // Preferred: run inside the tmux server so TPM env vars exist.
    let server_up = Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if server_up {
        return Command::new("tmux")
            .args(["run-shell", script_str])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }

    // Fallback: inject the env var and invoke directly.
    Command::new(&script)
        .env("TMUX_PLUGIN_MANAGER_PATH", &plugin_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Best-effort: reload every live tmux session with the new conf. Returns
/// the number of sessions that were reloaded (0 if `tmux` isn't running or
/// not installed).
fn reload_live_sessions(path: &Path) -> usize {
    // Check tmux server is up by listing sessions; a non-zero exit means no
    // sessions / no server, which is fine.
    let list = Command::new("tmux").args(["list-sessions", "-F", "#S"]).output();
    let count = match list {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        _ => 0,
    };
    if count == 0 {
        return 0;
    }
    let _ = Command::new("tmux")
        .args(["source-file", path.to_str().unwrap_or("")])
        .status();
    count
}

// ──────────────────────────────────────────────────────────────────────────
// Public CLI entrypoints
// ──────────────────────────────────────────────────────────────────────────

#[derive(clap::Args, Debug)]
pub struct InstallArgs {
    /// Skip the confirmation prompt (non-interactive use).
    #[arg(long, short)]
    pub yes: bool,

    /// Skip TPM clone + plugin install. Useful in restricted environments
    /// or for users who don't want plugins.
    #[arg(long)]
    pub no_plugins: bool,

    /// Skip `tmux source-file` reload of live sessions.
    #[arg(long)]
    pub no_reload: bool,
}

#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Show a diff preview if the on-disk conf is stale.
    #[arg(long)]
    pub diff: bool,
}

/// `ainb tmux install` entrypoint.
pub async fn install(args: InstallArgs, _format: OutputFormat) -> Result<()> {
    let target = tmux_conf_path()?;
    let state = detect_state(&target)?;

    match state {
        InstallState::UpToDate => {
            println!("✓ {} is already up to date.", target.display());
            return Ok(());
        }
        InstallState::Missing => {
            println!("No existing {} — installing fresh.", target.display());
        }
        InstallState::Stale => {
            let on_disk = fs::read_to_string(&target)?;
            println!(
                "Found existing {} ({} lines).",
                target.display(),
                on_disk.lines().count()
            );
            println!(
                "Proposed: ainb-tui bundled conf ({} lines).\n",
                BUNDLED_CONF.lines().count()
            );
            println!("--- diff preview (first 40 lines) ---");
            println!("{}", diff_preview(&on_disk, BUNDLED_CONF, 40));
            println!("--- end diff ---\n");
        }
    }

    if !args.yes && !prompt_yes_no("Apply?")? {
        println!("Aborted.");
        return Ok(());
    }

    if matches!(state, InstallState::Stale) {
        let backup = backup_existing(&target)?;
        println!("✓ Backed up existing conf → {}", backup.display());
    }

    write_conf(&target)?;
    println!("✓ Wrote {}", target.display());

    if deploy_helpers() {
        if let Ok(p) = helper_script_path() {
            println!("✓ Deployed helper {}", p.display());
        }
    } else {
        println!("! Failed to deploy git_branch.sh helper (status-right branch will be empty)");
    }

    // Step order matters here: TPM's `install_plugins` script needs
    // `TMUX_PLUGIN_MANAGER_PATH`, which the tmux server only sets once it has
    // sourced a conf containing TPM's `run` directive. So we:
    //   1. Clone TPM (so the conf's `run` line has something to load).
    //   2. Reload live sessions — this evaluates the new conf in-server,
    //      registering the TPM env vars.
    //   3. THEN invoke `install_plugins` via `tmux run-shell` so it inherits
    //      the live server env.
    let tpm_ready = if !args.no_plugins {
        let ok = ensure_tpm();
        if ok {
            println!("✓ TPM present at ~/.tmux/plugins/tpm");
        } else {
            println!(
                "! Could not clone TPM (network or git missing). \
                 Plugins won't load until you run `prefix + I` inside tmux, or:\n  \
                 git clone https://github.com/tmux-plugins/tpm ~/.tmux/plugins/tpm"
            );
        }
        ok
    } else {
        false
    };

    if !args.no_reload {
        let n = reload_live_sessions(&target);
        if n > 0 {
            println!("✓ Reloaded {n} live tmux session(s) via `tmux source-file`");
        } else {
            println!("· No live tmux sessions to reload.");
        }
    }

    if !args.no_plugins && tpm_ready {
        if run_tpm_install() {
            println!("✓ TPM installed plugins (resurrect + continuum + yank)");
            // Second reload: TPM's run-directive only loads plugins that exist
            // on disk at source-time. The first reload happened before the
            // clone, so the plugins' bindings (prefix+C-s save, prefix+C-r
            // restore, etc.) weren't registered. Re-source now so they bind.
            if !args.no_reload {
                let n = reload_live_sessions(&target);
                if n > 0 {
                    println!(
                        "✓ Re-sourced {n} session(s) so plugin bindings activate"
                    );
                }
            }
        } else {
            println!(
                "! TPM plugin install did not complete. Run `prefix + I` inside \
                 any tmux session to retry."
            );
        }
    }

    println!("\nDone. Try:");
    println!("  tmux new -s test     # start a session");
    println!("  C-b ←                 # detach (new alternative binding)");
    println!("  C-b r                 # reload conf");
    Ok(())
}

/// `ainb tmux status` entrypoint.
pub async fn status(args: StatusArgs, _format: OutputFormat) -> Result<()> {
    let target = tmux_conf_path()?;
    let state = detect_state(&target)?;

    println!("Bundled conf:    {} lines", BUNDLED_CONF.lines().count());
    println!("Install path:    {}", target.display());
    println!("State:           {}", state.label());

    if args.diff && matches!(state, InstallState::Stale) {
        let on_disk = fs::read_to_string(&target)?;
        println!("\n--- diff preview ---");
        println!("{}", diff_preview(&on_disk, BUNDLED_CONF, 80));
        println!("--- end diff ---");
    }

    // Surface the most recent backup, if any — handy for "what would
    // uninstall restore?".
    if let Some(parent) = target.parent() {
        if let Ok(read) = fs::read_dir(parent) {
            let mut backups: Vec<PathBuf> = read
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(".tmux.conf.bak."))
                        .unwrap_or(false)
                })
                .collect();
            backups.sort();
            if let Some(latest) = backups.last() {
                println!("Latest backup:   {}", latest.display());
            }
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_state_missing_when_path_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.conf");
        assert_eq!(detect_state(&path).unwrap(), InstallState::Missing);
    }

    #[test]
    fn detect_state_up_to_date_when_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".tmux.conf");
        fs::write(&path, BUNDLED_CONF).unwrap();
        assert_eq!(detect_state(&path).unwrap(), InstallState::UpToDate);
    }

    #[test]
    fn detect_state_stale_when_byte_different() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".tmux.conf");
        fs::write(&path, "set -g mouse off\n").unwrap();
        assert_eq!(detect_state(&path).unwrap(), InstallState::Stale);
    }

    #[test]
    fn diff_preview_marks_removed_and_added_lines() {
        let preview = diff_preview("a\nb\nc\n", "a\nB\nc\n", 10);
        assert!(preview.contains("- b"));
        assert!(preview.contains("+ B"));
    }

    #[test]
    fn diff_preview_truncates_at_cap() {
        let on_disk: String = (0..50).map(|i| format!("old-{i}\n")).collect();
        let bundle: String = (0..50).map(|i| format!("new-{i}\n")).collect();
        let preview = diff_preview(&on_disk, &bundle, 10);
        assert!(preview.lines().count() <= 11); // 10 diff lines + tail summary
        assert!(preview.contains("more line(s) differ"));
    }

    #[test]
    fn bundled_conf_contains_signature_markers() {
        // Light smoke check that include_str! is wired to the right file.
        assert!(BUNDLED_CONF.contains("Catppuccin Mocha"));
        assert!(BUNDLED_CONF.contains("bind Left detach-client"));
        assert!(BUNDLED_CONF.contains("@plugin 'tmux-plugins/tpm'"));
    }
}
