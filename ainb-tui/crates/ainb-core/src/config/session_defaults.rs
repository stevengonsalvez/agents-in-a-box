// ABOUTME: Persisted last-used state for the new-session screen.
// Stores screen-1 last-selection + per-repo last-used preset/branch/prompt.
// YAML at `~/.agents-in-a-box/session-defaults.yaml`. Best-effort load
// (corruption -> empty state, never blocks the launch flow).

#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::favorites_store::SourceType;

/// Per-repo last-used configuration. Recorded on every successful launch
/// so the user does not have to re-pick the same preset / prompt next time.
///
/// `last_used_at` is included for future LRU sorting on screen 1
/// (cheap to add now, expensive later — see TDD plan Phase 2 REFACTOR).
///
/// `source_type` + `source` carry the original provenance of a "recent" row
/// (finding #1) so the unified picker can reconstruct a real `RepoSource`
/// instead of a `Filter` sink — without these, Enter-on-Recent was a silent
/// no-op. Both fields are `#[serde(default)]` for back-compat with
/// pre-existing on-disk YAML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PerRepoDefaults {
    #[serde(default)]
    pub last_preset: Option<String>,
    #[serde(default)]
    pub last_branch_override: Option<String>,
    #[serde(default)]
    pub last_prompt: Option<String>,
    #[serde(default = "default_last_used_at")]
    pub last_used_at: DateTime<Utc>,
    /// Provenance type — mirrors `Favorite.source_type`. `None` for pre-fix
    /// recents (the picker falls back to `parse_with` at row build time).
    #[serde(default)]
    pub source_type: Option<SourceType>,
    /// Stringified source — git URL, owner/repo, or local path. `None` for
    /// pre-fix recents.
    #[serde(default)]
    pub source: Option<String>,
}

fn default_last_used_at() -> DateTime<Utc> {
    Utc::now()
}

impl Default for PerRepoDefaults {
    fn default() -> Self {
        Self {
            last_preset: None,
            last_branch_override: None,
            last_prompt: None,
            last_used_at: Utc::now(),
            source_type: None,
            source: None,
        }
    }
}

/// Top-level session-defaults persisted to `~/.agents-in-a-box/session-defaults.yaml`.
///
/// `last_repo` is the repo highlighted on screen 1 on next open;
/// `per_repo` keeps the launch history per repo (preset, branch, prompt).
///
/// `^R` (reset) on screen 1 clears `last_repo` ONLY — the per-repo history
/// is preserved so users do not lose their per-project preset preferences.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDefaults {
    #[serde(default)]
    pub last_repo: Option<String>,
    #[serde(default)]
    pub per_repo: HashMap<String, PerRepoDefaults>,
}

impl SessionDefaults {
    /// Best-effort load. Never returns an error — corrupt files or missing
    /// files both yield an empty `SessionDefaults`. Parse failures are
    /// surfaced via `tracing::warn!` so they are observable but do not
    /// block the launch flow (per spec risk-mitigation table).
    pub fn load_from(path: &Path) -> Self {
        let bytes = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                // Missing file is the normal case for fresh installs.
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "session-defaults: read failed, starting from empty state",
                    );
                }
                return Self::default();
            }
        };

        match serde_yaml::from_str::<SessionDefaults>(&bytes) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "session-defaults: parse failed, starting from empty state",
                );
                Self::default()
            }
        }
    }

    /// Atomic write via tmp-file + rename. Returns Result so callers can
    /// surface persistence errors (e.g., disk full) but the launch path
    /// MUST tolerate failures here without aborting the session creation.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("creating parent dir for {}", path.display())
            })?;
        }

        let yaml = serde_yaml::to_string(self).context("serializing SessionDefaults to YAML")?;

        // Atomic write: write to a sibling tmp file then rename. `rename`
        // is atomic on POSIX (same filesystem) so a `kill -9` mid-save
        // either leaves the old file intact or the new file complete —
        // never a partial.
        let tmp_path = match path.extension() {
            Some(_) => {
                let mut buf = path.as_os_str().to_owned();
                buf.push(".tmp");
                PathBuf::from(buf)
            }
            None => path.with_extension("tmp"),
        };

        fs::write(&tmp_path, yaml.as_bytes())
            .with_context(|| format!("writing tmp file {}", tmp_path.display()))?;

        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "renaming {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }

    /// Record a successful launch. Updates both `last_repo` (so the same
    /// repo is highlighted next time the new-session screen opens) and
    /// the `per_repo` entry for the launched repo (so the same preset /
    /// branch / prompt are pre-loaded next time the user picks this repo).
    ///
    /// `source_type` + `source` carry the original `RepoSource` provenance
    /// so the unified picker can reconstruct the variant on the next visit
    /// (finding #1 — Recents are dead). When `None`, the picker falls back
    /// to favorites lookup or `parse_with` re-parse.
    pub fn record_launch(
        &mut self,
        repo: &str,
        preset: &str,
        branch_override: Option<&str>,
        prompt: Option<&str>,
        source_type: Option<SourceType>,
        source: Option<&str>,
    ) {
        self.last_repo = Some(repo.to_string());

        let entry = self.per_repo.entry(repo.to_string()).or_default();
        entry.last_preset = Some(preset.to_string());
        entry.last_branch_override = branch_override.map(str::to_string);
        entry.last_prompt = prompt.map(str::to_string);
        entry.last_used_at = Utc::now();
        if source_type.is_some() {
            entry.source_type = source_type;
        }
        if let Some(s) = source {
            entry.source = Some(s.to_string());
        }
    }

    /// Clear `last_repo` only. Per-repo history is preserved so `^R` on
    /// screen 1 acts as "forget my screen-1 highlight" without nuking
    /// per-project preset preferences.
    pub fn reset_last_repo(&mut self) {
        self.last_repo = None;
    }

    /// Default on-disk path: `~/.agents-in-a-box/session-defaults.yaml`.
    /// Falls back to a temp dir derived sentinel when `dirs::home_dir()`
    /// returns None (effectively never on supported platforms, but we
    /// avoid panicking to honour the "never blocks the flow" promise).
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".agents-in-a-box").join("session-defaults.yaml"))
            .unwrap_or_else(|| PathBuf::from("session-defaults.yaml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_yaml() {
        let mut d = SessionDefaults::default();
        d.last_repo = Some("ainb-tui".into());
        d.per_repo.insert(
            "ainb-tui".into(),
            PerRepoDefaults {
                last_preset: Some("claude-opus-yolo".into()),
                last_branch_override: None,
                last_prompt: None,
                ..Default::default()
            },
        );
        let yaml = serde_yaml::to_string(&d).unwrap();
        let restored: SessionDefaults = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored.last_repo.as_deref(), Some("ainb-tui"));
        assert_eq!(
            restored.per_repo["ainb-tui"].last_preset.as_deref(),
            Some("claude-opus-yolo")
        );
    }

    #[test]
    fn corrupt_yaml_returns_empty_defaults_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session-defaults.yaml");
        std::fs::write(&path, "{ this is not: valid yaml }: [ broken").unwrap();
        let d = SessionDefaults::load_from(&path);
        assert!(
            d.last_repo.is_none(),
            "corrupt YAML must fall back to empty"
        );
        assert!(d.per_repo.is_empty());
    }

    #[test]
    fn missing_file_returns_empty_defaults_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing.yaml");
        let d = SessionDefaults::load_from(&path);
        assert!(d.last_repo.is_none());
        assert!(d.per_repo.is_empty());
    }

    #[test]
    fn save_writes_atomically_and_updates_per_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session-defaults.yaml");
        let mut d = SessionDefaults::load_from(&path);
        d.record_launch(
            "ainb-tui",
            "claude-opus-yolo",
            None,
            Some("fix the thing"),
            Some(SourceType::GithubShorthand),
            Some("stevengonsalvez/ainb-tui"),
        );
        d.save_to(&path).unwrap();

        // No leftover tmp file from atomic-write step.
        let tmp_file = path.with_extension("yaml.tmp");
        assert!(
            !tmp_file.exists(),
            "atomic-write left a tmp file behind: {}",
            tmp_file.display()
        );

        let restored = SessionDefaults::load_from(&path);
        assert_eq!(restored.last_repo.as_deref(), Some("ainb-tui"));
        assert_eq!(
            restored.per_repo["ainb-tui"].last_preset.as_deref(),
            Some("claude-opus-yolo")
        );
        assert_eq!(
            restored.per_repo["ainb-tui"].last_prompt.as_deref(),
            Some("fix the thing")
        );
        assert!(restored.per_repo["ainb-tui"].last_branch_override.is_none());
    }

    #[test]
    fn reset_clears_last_repo_but_preserves_per_repo_history() {
        let mut d = SessionDefaults::default();
        d.last_repo = Some("ainb-tui".into());
        d.per_repo.insert(
            "ainb-tui".into(),
            PerRepoDefaults {
                last_preset: Some("claude-opus-yolo".into()),
                ..Default::default()
            },
        );
        d.reset_last_repo();
        assert!(d.last_repo.is_none(), "^R must clear last_repo");
        assert!(
            d.per_repo.contains_key("ainb-tui"),
            "history must survive ^R reset"
        );
        assert_eq!(
            d.per_repo["ainb-tui"].last_preset.as_deref(),
            Some("claude-opus-yolo"),
            "per-repo preset must survive ^R reset"
        );
    }
}
