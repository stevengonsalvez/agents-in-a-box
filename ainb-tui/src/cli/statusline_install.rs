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

/// Command we install into Claude Code's `statusLine.command`.
pub const AINB_STATUSLINE_CMD: &str = "ainb statusline";

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

fn command_is_ours(cmd: &str) -> bool {
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
        StatuslineStatus::Configured => Ok(InstallOutcome::AlreadyInstalled),
        StatuslineStatus::Other(current) => Ok(InstallOutcome::ExistingDifferent {
            current_command: current,
        }),
        StatuslineStatus::NotConfigured => {
            backup(path)?;
            // Preserve every other key — only set `statusLine`.
            if let Some(obj) = value.as_object_mut() {
                obj.insert("statusLine".to_string(), block);
            } else {
                // Top-level wasn't an object — replace entire file (this
                // is malformed config from the user's POV).
                value = serde_json::json!({ "statusLine": block });
            }
            atomic_write_json(path, &value)?;
            Ok(InstallOutcome::Installed)
        }
    }
}

/// Replace whatever is in `statusLine.command` with our command. Caller
/// must have made the keep/replace decision; this is the "replace"
/// branch. Backs up first.
pub fn install_statusline_replace_at(path: &Path) -> Result<InstallOutcome> {
    if !path.exists() {
        return install_statusline_at(path);
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    backup(path)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("statusLine".to_string(), our_block());
    } else {
        value = serde_json::json!({ "statusLine": our_block() });
    }
    atomic_write_json(path, &value)?;
    Ok(InstallOutcome::Installed)
}

fn our_block() -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": AINB_STATUSLINE_CMD,
        "padding": 0,
    })
}

fn backup(path: &Path) -> Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak = path.with_extension(format!("json.bak.{ts}"));
    std::fs::copy(path, &bak)
        .with_context(|| format!("failed to back up {} -> {}", path.display(), bak.display()))?;
    Ok(())
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
    fn detect_status_returns_configured_for_our_command() {
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
        assert_eq!(v["statusLine"]["command"], "ainb statusline");

        // Backup created
        let bak_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.bak."))
            .count();
        assert_eq!(bak_count, 1, "exactly one backup should be created");
    }

    #[test]
    fn install_is_idempotent_when_already_ours() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"statusLine":{"type":"command","command":"ainb statusline","padding":0}}"#,
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
