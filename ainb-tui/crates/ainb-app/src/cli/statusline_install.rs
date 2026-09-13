// ABOUTME: Helpers for wiring `ainb statusline` into Claude Code's
// `~/.claude/settings.json`.
//
// Idempotency story:
//   - If our block is already present we no-op (`AlreadyInstalled`).
//   - If a different statusLine command is present we report it and let
//     the caller decide keep/replace — we never silently overwrite.
//   - All writes are preceded by a backup at `settings.json.bak.<unix-ts>`
//     so the user can always restore the prior config.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Legacy command we used to install into Claude Code's
/// `statusLine.command`. Retained for migration detection — fresh installs
/// write [`AINB_CLAUDECODE_STATUSLINE_CMD`] instead.
pub const AINB_STATUSLINE_CMD: &str = "ainb statusline";

/// Canonical command we install into Claude Code's `statusLine.command`.
/// Provider-namespaced so that other providers (Codex et al.) can grow
/// their own equivalents without lying at the top level.
pub const AINB_CLAUDECODE_STATUSLINE_CMD: &str = "ainb claudecode statusline";

/// Outcome of an `install_statusline()` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// We wrote our block where there was none before.
    Installed,
    /// Our block was already present; nothing changed.
    AlreadyInstalled,
    /// A different statusLine command was present; we did NOT overwrite.
    /// The caller decides keep / replace based on the value.
    ExistingDifferent { current_command: String },
    /// The legacy `ainb statusline` command was present and we rewrote it
    /// in place to the new `ainb claudecode statusline` form. Treated as
    /// a successful upgrade — the user already opted in to ainb owning
    /// the statusline; namespace is an internal concern.
    Migrated,
}

/// Status of the user's statusline configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatuslineStatus {
    /// `ainb statusline` is wired as the sole statusLine command.
    Configured,
    /// No statusLine block at all.
    NotConfigured,
    /// Some other command is wired.
    Other(String),
}

/// Resolve `~/.claude/settings.json`.
pub fn settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("HOME not set; cannot resolve ~/.claude")?;
    Ok(home.join(".claude").join("settings.json"))
}

/// Detect the current statusline configuration. Errors only on IO problems
/// reading an existing settings.json — a missing file maps to `NotConfigured`.
pub fn detect_statusline_status() -> Result<StatuslineStatus> {
    let path = settings_path()?;
    detect_statusline_status_at(&path)
}

/// Test seam: detect status from an explicit settings path.
pub fn detect_statusline_status_at(path: &Path) -> Result<StatuslineStatus> {
    if !path.exists() {
        return Ok(StatuslineStatus::NotConfigured);
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(classify_status(&value))
}

fn classify_status(value: &serde_json::Value) -> StatuslineStatus {
    let Some(cmd) = value.pointer("/statusLine/command").and_then(|v| v.as_str()) else {
        return StatuslineStatus::NotConfigured;
    };
    if command_is_ours(cmd) {
        StatuslineStatus::Configured
    } else {
        StatuslineStatus::Other(cmd.to_string())
    }
}

/// True if `cmd` is *either* the legacy `ainb statusline` or the new
/// canonical `ainb claudecode statusline` form. Used to decide whether
/// the on-disk config is "owned by ainb" — separately, the legacy form
/// is detected via [`is_legacy_command`] to drive the in-place migration.
fn command_is_ours(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    trimmed == AINB_STATUSLINE_CMD || trimmed == AINB_CLAUDECODE_STATUSLINE_CMD
}

/// True if `cmd` is the legacy top-level `ainb statusline` form. Drives
/// the migration branch in `install_statusline_at` so existing settings
/// land on the new namespaced command on the next install/init.
fn is_legacy_command(cmd: &str) -> bool {
    cmd.trim() == AINB_STATUSLINE_CMD
}

/// Has the cache file been written within `max_age_secs`?
pub fn is_cache_fresh(max_age_secs: u64) -> bool {
    let Some(path) = super::statusline::cache_path() else {
        return false;
    };
    is_cache_fresh_at(&path, max_age_secs)
}

/// Test seam.
pub fn is_cache_fresh_at(path: &Path, max_age_secs: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.elapsed() else {
        // System clock went backwards; treat as fresh — better than
        // confusingly stale-flagging a file we just wrote.
        return true;
    };
    elapsed.as_secs() <= max_age_secs
}

/// Install `ainb statusline` into `~/.claude/settings.json`.
///
/// See `InstallOutcome` for the four return states. Backs up any existing
/// file to `settings.json.bak.<unix-ts>` before writing.
pub fn install_statusline() -> Result<InstallOutcome> {
    let path = settings_path()?;
    install_statusline_at(&path)
}

/// Test seam: install at an explicit path.
pub fn install_statusline_at(path: &Path) -> Result<InstallOutcome> {
    let block = our_block();

    // Case 1: file does not exist — create with just our block.
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let initial = serde_json::json!({ "statusLine": block });
        atomic_write_json(path, &initial)?;
        return Ok(InstallOutcome::Installed);
    }

    // Case 2: file exists — load, classify, decide.
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    match classify_status(&value) {
        StatuslineStatus::Configured => {
            // Both legacy and new forms classify as `Configured`. If the
            // user is on the legacy form, rewrite in place to the new
            // namespaced command — they already opted in to ainb owning
            // the statusline; the old name is just a stale label.
            let current =
                value.pointer("/statusLine/command").and_then(|v| v.as_str()).unwrap_or("");
            if is_legacy_command(current) {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("statusLine".to_string(), block);
                }
                write_with_backup(path, &bytes, &value)?;
                Ok(InstallOutcome::Migrated)
            } else {
                Ok(InstallOutcome::AlreadyInstalled)
            }
        }
        StatuslineStatus::Other(current) => Ok(InstallOutcome::ExistingDifferent {
            current_command: current,
        }),
        StatuslineStatus::NotConfigured => {
            // Preserve every other key — only set `statusLine`.
            if let Some(obj) = value.as_object_mut() {
                obj.insert("statusLine".to_string(), block);
            } else {
                // Top-level wasn't an object — replace entire file (this
                // is malformed config from the user's POV).
                value = serde_json::json!({ "statusLine": block });
            }
            write_with_backup(path, &bytes, &value)?;
            Ok(InstallOutcome::Installed)
        }
    }
}

/// Replace whatever is in `statusLine.command` with our command. Caller
/// must have made the keep/replace decision; this is the "replace"
/// branch. Backup is written *after* the new file lands successfully so
/// a failed write cannot leave a stray `.bak` with no rewrite.
pub fn install_statusline_replace_at(path: &Path) -> Result<InstallOutcome> {
    if !path.exists() {
        return install_statusline_at(path);
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("statusLine".to_string(), our_block());
    } else {
        value = serde_json::json!({ "statusLine": our_block() });
    }
    write_with_backup(path, &bytes, &value)?;
    Ok(InstallOutcome::Installed)
}

fn our_block() -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": AINB_CLAUDECODE_STATUSLINE_CMD,
        "padding": 0,
    })
}

/// Maximum number of `settings.json.bak.*` files to retain alongside the
/// settings file. Older backups are pruned (best-effort) on each new
/// backup write so the directory does not accumulate stale copies over
/// time.
const MAX_BACKUPS: usize = 3;

/// Write `new_value` to `path` atomically, then write `prev_bytes` (the
/// in-memory snapshot of the previous on-disk contents) to a sibling
/// `<path>.bak.<unix-ts>` and prune older backups.
///
/// Backup-after-write ordering matters: if the new write fails (disk
/// full, permissions, rename race), no `.bak` is left behind. If the
/// new write succeeds but the backup write fails we swallow the error
/// and log — the new file is the important artefact, and a missing
/// backup is recoverable through normal version control.
fn write_with_backup(path: &Path, prev_bytes: &[u8], new_value: &serde_json::Value) -> Result<()> {
    atomic_write_json(path, new_value)?;
    if let Err(e) = write_backup_from_bytes(path, prev_bytes) {
        tracing::warn!(
            "wrote {} but failed to record backup: {}",
            path.display(),
            e
        );
    }
    Ok(())
}

/// Write `prev_bytes` to a `<path>.bak.<unix-ts>` file and prune older
/// backups. Used by `write_with_backup` after a successful new-file
/// write — the bytes are the *previous* on-disk contents captured
/// before the write.
fn write_backup_from_bytes(path: &Path, prev_bytes: &[u8]) -> Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak = path.with_extension(format!("json.bak.{ts}"));
    std::fs::write(&bak, prev_bytes)
        .with_context(|| format!("failed to write backup {}", bak.display()))?;
    prune_old_backups(path, MAX_BACKUPS);
    Ok(())
}

/// Best-effort: keep only the `keep` newest `settings.json.bak.*` files
/// in `path`'s parent directory. Errors are swallowed — a stale backup
/// is clutter, not a correctness problem.
fn prune_old_backups(path: &Path, keep: usize) {
    let Some(parent) = path.parent() else { return };
    let Some(stem) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    // Match `<stem>.bak.<digits>` (we own this filename shape).
    let prefix = format!("{stem}.bak.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut backups: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let name_str = name.to_str()?;
            if !name_str.starts_with(&prefix) {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    if backups.len() <= keep {
        return;
    }
    // Newest first.
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, stale) in backups.into_iter().skip(keep) {
        let _ = std::fs::remove_file(stale);
    }
}

fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detect_status_returns_not_configured_for_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::NotConfigured
        );
    }

    #[test]
    fn detect_status_returns_not_configured_when_no_status_line_block() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, br#"{"theme":"dark"}"#).unwrap();
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::NotConfigured
        );
    }

    #[test]
    fn detect_status_returns_configured_for_legacy_command() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"ainb statusline"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::Configured
        );
    }

    #[test]
    fn detect_status_returns_configured_for_new_namespaced_command() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"ainb claudecode statusline"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::Configured
        );
    }

    #[test]
    fn detect_status_returns_other_for_chained_command() {
        // After dropping chain mode, a piped command is treated as a
        // foreign command — caller must explicitly replace.
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"~/bin/my-line.sh | ainb statusline"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::Other("~/bin/my-line.sh | ainb statusline".to_string())
        );
    }

    #[test]
    fn detect_status_returns_other_for_foreign_command() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"~/bin/my-line.sh"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::Other("~/bin/my-line.sh".to_string())
        );
    }

    #[test]
    fn install_creates_file_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let outcome = install_statusline_at(&path).unwrap();
        assert_eq!(outcome, InstallOutcome::Installed);
        assert!(path.exists());
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::Configured
        );
    }

    #[test]
    fn install_merges_into_existing_file_preserving_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"theme":"dark","mcpServers":{"foo":{"command":"foo"}}}"#,
        )
        .unwrap();

        let outcome = install_statusline_at(&path).unwrap();
        assert_eq!(outcome, InstallOutcome::Installed);

        // Reload and verify other keys preserved
        let bytes = std::fs::read(&path).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["theme"], "dark");
        assert!(v["mcpServers"].is_object());
        assert_eq!(v["statusLine"]["command"], AINB_CLAUDECODE_STATUSLINE_CMD);

        // Backup created
        let bak_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak."))
            .count();
        assert_eq!(bak_count, 1, "exactly one backup should be created");
    }

    #[test]
    fn install_is_idempotent_when_already_on_new_command() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"ainb claudecode statusline","padding":0}}"#,
        )
        .unwrap();
        let outcome = install_statusline_at(&path).unwrap();
        assert_eq!(outcome, InstallOutcome::AlreadyInstalled);

        // No backups created on no-op
        let bak_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak."))
            .count();
        assert_eq!(bak_count, 0);
    }

    #[test]
    fn install_migrates_legacy_command_to_namespaced_form() {
        // Existing settings.json carrying the pre-namespacing command
        // must be rewritten in place on the next install/init pass.
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"theme":"dark","statusLine":{"type":"command","command":"ainb statusline","padding":0}}"#,
        )
        .unwrap();

        let outcome = install_statusline_at(&path).unwrap();
        assert_eq!(outcome, InstallOutcome::Migrated);

        // Other keys preserved, command rewritten.
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["statusLine"]["command"], AINB_CLAUDECODE_STATUSLINE_CMD);

        // Migration is a real write — it must produce a backup so the
        // user can revert if the rewrite was unwanted.
        let bak_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak."))
            .count();
        assert_eq!(bak_count, 1, "migration must record a backup");
    }

    #[test]
    fn is_legacy_command_only_matches_old_top_level_form() {
        assert!(is_legacy_command("ainb statusline"));
        assert!(is_legacy_command("  ainb statusline\n"));
        assert!(!is_legacy_command(AINB_CLAUDECODE_STATUSLINE_CMD));
        assert!(!is_legacy_command("ainb usage"));
        assert!(!is_legacy_command(""));
    }

    #[test]
    fn command_is_ours_accepts_both_legacy_and_new() {
        assert!(command_is_ours("ainb statusline"));
        assert!(command_is_ours(AINB_CLAUDECODE_STATUSLINE_CMD));
        assert!(!command_is_ours("~/bin/foo"));
        assert!(!command_is_ours("~/bin/foo | ainb statusline"));
    }

    #[test]
    fn install_reports_existing_different_without_overwriting() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"~/bin/foo"}}"#,
        )
        .unwrap();
        let outcome = install_statusline_at(&path).unwrap();
        assert_eq!(
            outcome,
            InstallOutcome::ExistingDifferent {
                current_command: "~/bin/foo".to_string()
            }
        );
        // File untouched
        let bytes = std::fs::read(&path).unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains("~/bin/foo"));
    }

    #[test]
    fn replace_overwrites_existing_command() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"~/bin/foo"}}"#,
        )
        .unwrap();
        let outcome = install_statusline_replace_at(&path).unwrap();
        assert_eq!(outcome, InstallOutcome::Installed);
        assert_eq!(
            detect_statusline_status_at(&path).unwrap(),
            StatuslineStatus::Configured
        );
    }

    #[test]
    fn prune_keeps_only_three_newest_backups() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{}").unwrap();

        // Create five backup files with strictly-increasing mtimes so
        // the test does not depend on filesystem listing order.
        let now = std::time::SystemTime::now();
        let mut paths = Vec::new();
        for i in 0..5 {
            let bak = dir.path().join(format!("settings.json.bak.{i}"));
            std::fs::write(&bak, b"{}").unwrap();
            // Older backups get older mtimes (i=0 oldest, i=4 newest).
            let mtime = now - std::time::Duration::from_secs(60 * (5 - i as u64));
            let f = std::fs::OpenOptions::new().write(true).open(&bak).unwrap();
            f.set_modified(mtime).unwrap();
            paths.push(bak);
        }

        prune_old_backups(&settings, 3);

        // The two oldest (i=0, i=1) should be gone; the three newest
        // (i=2, i=3, i=4) should remain.
        assert!(!paths[0].exists(), "oldest backup should be pruned");
        assert!(!paths[1].exists(), "second-oldest backup should be pruned");
        assert!(paths[2].exists(), "third-newest backup should remain");
        assert!(paths[3].exists(), "second-newest backup should remain");
        assert!(paths[4].exists(), "newest backup should remain");

        // Sanity: only three backups left in the directory.
        let remaining = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak."))
            .count();
        assert_eq!(remaining, 3);
    }

    #[test]
    fn prune_is_noop_when_under_threshold() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{}").unwrap();
        for i in 0..2 {
            std::fs::write(dir.path().join(format!("settings.json.bak.{i}")), b"{}").unwrap();
        }
        prune_old_backups(&settings, 3);
        let remaining = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak."))
            .count();
        assert_eq!(remaining, 2);
    }

    #[test]
    fn is_cache_fresh_returns_false_for_missing_file() {
        let dir = tempdir().unwrap();
        assert!(!is_cache_fresh_at(&dir.path().join("nope"), 60));
    }

    #[test]
    fn is_cache_fresh_returns_true_for_just_written_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("live.json");
        std::fs::write(&path, b"{}").unwrap();
        assert!(is_cache_fresh_at(&path, 60));
    }
}
